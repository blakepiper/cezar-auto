use std::collections::HashMap;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use futures_core::Stream;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use url::Url;

const INITIAL_RECONNECT_DELAY: Duration = Duration::from_millis(50);
const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(3);

/// One event frame from the WebSocket topic bus.
#[derive(Debug, Clone, PartialEq)]
pub struct EngineEvent {
    pub topic: String,
    pub data: Value,
}

#[derive(Clone)]
pub(crate) struct WsTopicBus {
    url: Url,
    state: Arc<Mutex<BusState>>,
}

struct BusState {
    next_id: usize,
    topics: HashMap<String, HashMap<usize, UnboundedSender<EngineEvent>>>,
    commands: Option<UnboundedSender<Command>>,
}

enum Command {
    Subscribe(String),
    Unsubscribe(String),
    Shutdown,
}

impl WsTopicBus {
    pub(crate) fn new(url: Url) -> Self {
        Self {
            url,
            state: Arc::new(Mutex::new(BusState {
                next_id: 0,
                topics: HashMap::new(),
                commands: None,
            })),
        }
    }

    pub(crate) fn subscribe(
        &self,
        topic: impl Into<String>,
    ) -> futures_core::stream::BoxStream<'static, EngineEvent> {
        let topic = topic.into();
        let (sender, receiver) = unbounded_channel();
        let mut start_task = None;
        let mut command = None;
        let id;
        {
            let Ok(mut state) = self.state.lock() else {
                return Box::pin(TopicSubscription {
                    topic,
                    id: 0,
                    receiver,
                    state: self.state.clone(),
                });
            };
            id = state.next_id;
            state.next_id += 1;
            let first_for_topic = state.topics.get(&topic).is_none_or(HashMap::is_empty);
            state
                .topics
                .entry(topic.clone())
                .or_default()
                .insert(id, sender);
            if state.commands.is_none() {
                let (command_sender, command_receiver) = unbounded_channel();
                state.commands = Some(command_sender);
                start_task = Some(command_receiver);
            } else if first_for_topic {
                command = state.commands.clone();
            }
        }
        if let Some(command_receiver) = start_task {
            let bus = self.clone();
            if tokio::runtime::Handle::try_current().is_ok() {
                tokio::spawn(run_bus(bus, command_receiver));
            }
        } else if let Some(command) = command {
            let _ = command.send(Command::Subscribe(topic.clone()));
        }

        Box::pin(TopicSubscription {
            topic,
            id,
            receiver,
            state: self.state.clone(),
        })
    }
}

struct TopicSubscription {
    topic: String,
    id: usize,
    receiver: UnboundedReceiver<EngineEvent>,
    state: Arc<Mutex<BusState>>,
}

impl Stream for TopicSubscription {
    type Item = EngineEvent;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.receiver.poll_recv(context)
    }
}

impl Drop for TopicSubscription {
    fn drop(&mut self) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        let last_for_topic = state.topics.get_mut(&self.topic).is_some_and(|listeners| {
            listeners.remove(&self.id);
            listeners.is_empty()
        });
        if last_for_topic {
            state.topics.remove(&self.topic);
            if let Some(commands) = &state.commands {
                let _ = commands.send(Command::Unsubscribe(self.topic.clone()));
            }
        }
        if state.topics.is_empty()
            && let Some(commands) = state.commands.take()
        {
            let _ = commands.send(Command::Shutdown);
        }
    }
}

async fn run_bus(bus: WsTopicBus, mut commands: UnboundedReceiver<Command>) {
    let mut delay = INITIAL_RECONNECT_DELAY;
    loop {
        if no_topics(&bus) {
            return;
        }
        if let Ok((mut socket, _)) = connect_async(bus.url.to_string()).await {
            delay = INITIAL_RECONNECT_DELAY;
            for topic in topics(&bus) {
                if socket
                    .send(subscribe_frame("subscribe", &topic))
                    .await
                    .is_err()
                {
                    break;
                }
            }
            loop {
                tokio::select! {
                    command = commands.recv() => {
                        match command {
                            Some(Command::Subscribe(topic)) => {
                                if socket.send(subscribe_frame("subscribe", &topic)).await.is_err() { break; }
                            }
                            Some(Command::Unsubscribe(topic)) => {
                                if socket.send(subscribe_frame("unsubscribe", &topic)).await.is_err() { break; }
                            }
                            Some(Command::Shutdown) | None => {
                                let _ = socket.close(None).await;
                                return;
                            }
                        }
                    }
                    message = socket.next() => {
                        match message {
                            Some(Ok(Message::Text(text))) => handle_frame(&bus, text.as_ref()),
                            Some(Ok(Message::Ping(payload))) => {
                                if socket.send(Message::Pong(payload)).await.is_err() { break; }
                            }
                            Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                            _ => {}
                        }
                    }
                }
            }
        }
        if no_topics(&bus) {
            return;
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(MAX_RECONNECT_DELAY);
    }
}

fn no_topics(bus: &WsTopicBus) -> bool {
    bus.state
        .lock()
        .map(|state| state.topics.is_empty())
        .unwrap_or(true)
}

fn topics(bus: &WsTopicBus) -> Vec<String> {
    bus.state
        .lock()
        .map(|state| state.topics.keys().cloned().collect())
        .unwrap_or_default()
}

fn subscribe_frame(kind: &str, topic: &str) -> Message {
    Message::Text(json!({"type": kind, "topic": topic}).to_string().into())
}

fn handle_frame(bus: &WsTopicBus, text: &str) {
    let Ok(Value::Object(frame)) = serde_json::from_str(text) else {
        return;
    };
    if frame.get("type").and_then(Value::as_str) != Some("event") {
        return;
    }
    let Some(topic) = frame.get("topic").and_then(Value::as_str) else {
        return;
    };
    let data = frame.get("data").cloned().unwrap_or(Value::Null);
    let event = EngineEvent {
        topic: topic.to_owned(),
        data,
    };
    let Ok(mut state) = bus.state.lock() else {
        return;
    };
    let Some(listeners) = state.topics.get_mut(topic) else {
        return;
    };
    listeners.retain(|_, sender| sender.send(event.clone()).is_ok());
}
