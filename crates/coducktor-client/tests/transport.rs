use std::time::Duration;

use coducktor_client::{Engine, EngineError, HttpEngine, Scope, Topic};
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio::net::TcpListener;
use tokio_tungstenite::{accept_async, tungstenite::Message};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn sse_resume_uses_after_seq_and_deduplicates_replayed_frames() {
    let server = MockServer::start().await;
    let body = concat!(
        "id: 2\n",
        "event: run-event\n",
        "data: {\"seq\":2,\"ts\":\"now\",\"type\":\"old\"}\n\n",
        "id: 3\n",
        "event: ui-event\n",
        "data: {\"seq\":3,\"ts\":\"now\",\"type\":\"new\"}\n\n",
    );
    Mock::given(method("GET"))
        .and(path("/api/v1/p/shop/runs/run-1/events"))
        .and(query_param("afterSeq", "2"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_raw(body, "text/event-stream"),
        )
        .mount(&server)
        .await;

    let engine = HttpEngine::new(server.uri()).unwrap();
    let mut events = engine
        .run_events(&Scope::Project("shop".into()), "run-1", None, Some(2.0))
        .await
        .unwrap();
    let event = events.next().await.unwrap().unwrap();

    assert_eq!(event.seq, 3.0);
    assert_eq!(event.channel.as_deref(), Some("ui-event"));
    assert_eq!(event.event.event_type, "new");
    assert!(events.next().await.is_none());
}

#[tokio::test]
async fn websocket_reconnect_resubscribes_held_topics() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        for attempt in 0..2 {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            let Some(Ok(Message::Text(frame))) = socket.next().await else {
                panic!("client did not subscribe");
            };
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&frame).unwrap(),
                json!({"type":"subscribe","topic":"health"})
            );
            socket
                .send(Message::Text(
                    json!({
                        "type":"event",
                        "topic":"health",
                        "data":{"attempt":attempt}
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
            if attempt == 0 {
                socket.close(None).await.unwrap();
            }
        }
    });

    let engine = HttpEngine::new(format!("http://{address}")).unwrap();
    let mut events = engine.subscribe(Topic::Health);
    let first = tokio::time::timeout(Duration::from_secs(2), events.next())
        .await
        .unwrap()
        .unwrap();
    let second = tokio::time::timeout(Duration::from_secs(3), events.next())
        .await
        .unwrap()
        .unwrap();

    assert_eq!(first.data, json!({"attempt": 0}));
    assert_eq!(second.data, json!({"attempt": 1}));
    drop(events);
    server.await.unwrap();
}

#[tokio::test]
async fn http_statuses_become_domain_errors() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/runs/missing"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/runs/conflict"))
        .respond_with(ResponseTemplate::new(409).set_body_json(json!({"error":"run is active"})))
        .mount(&server)
        .await;

    let engine = HttpEngine::new(server.uri()).unwrap();
    assert_eq!(
        engine.get_run(&Scope::Workspace, "missing").await,
        Err(EngineError::NotFound)
    );
    assert_eq!(
        engine.get_run(&Scope::Workspace, "conflict").await,
        Err(EngineError::Conflict {
            reason: "run is active".into()
        })
    );
}

#[tokio::test]
async fn run_mutations_hit_the_versioned_routes() {
    let server = MockServer::start().await;
    let run = json!({"id":"run-1","title":"Ship","workflow":"quick-task","task":"ship","status":"done","createdAt":"now","tokensUsed":0,"archived":false,"steps":[]});
    Mock::given(method("POST"))
        .and(path("/api/v1/p/shop/runs/run-1/archive"))
        .and(|request: &wiremock::Request| request.body.as_slice() == br#"{"archived":true}"#)
        .respond_with(ResponseTemplate::new(200).set_body_json(run.clone()))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/p/shop/runs/run-1/read"))
        .respond_with(ResponseTemplate::new(200).set_body_json(run.clone()))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/p/shop/runs/run-1/unread"))
        .respond_with(ResponseTemplate::new(200).set_body_json(run.clone()))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/p/shop/runs/run-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"deleted":true})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/p/shop/runs/archive-finished"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"archived":3})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/p/shop/runs/read-all"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"read":2})))
        .mount(&server)
        .await;

    let engine = HttpEngine::new(server.uri()).unwrap();
    let scope = Scope::Project("shop".into());
    let archived = engine.archive_run(&scope, "run-1", true).await.unwrap();
    assert_eq!(archived.record.id, "run-1");
    assert!(engine.read_run(&scope, "run-1").await.is_ok());
    assert!(engine.unread_run(&scope, "run-1").await.is_ok());
    assert!(engine.delete_run(&scope, "run-1").await.unwrap().deleted);
    assert_eq!(engine.archive_finished(&scope).await.unwrap().archived, 3.0);
    assert_eq!(engine.mark_all_read(&scope).await.unwrap().read, 2.0);
}

#[tokio::test]
async fn runs_index_is_workspace_level() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/workspace/runs-index"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "runs": [{
                "projectId":"coducktor",
                "id":"run-1",
                "title":"Ship",
                "status":"running",
                "createdAt":"now",
                "archived":false,
                "workflow":"quick-task"
            }],
            "perProjectLimit":200,
            "truncated":["coducktor"],
            "referenceStatuses":{}
        })))
        .mount(&server)
        .await;

    let engine = HttpEngine::new(server.uri()).unwrap();
    let index = engine.runs_index().await.unwrap();
    assert_eq!(index.runs.len(), 1);
    assert_eq!(index.runs[0].project_id, "coducktor");
    assert_eq!(index.per_project_limit, 200);
    assert_eq!(index.truncated, vec!["coducktor".to_owned()]);
}
