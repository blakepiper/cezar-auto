use async_stream::try_stream;
use bytes::Bytes;
use futures_core::Stream;
use futures_util::StreamExt;

use crate::error::EngineError;

/// One parsed Server-Sent Event frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseFrame {
    pub id: Option<String>,
    pub event: Option<String>,
    pub data: String,
}

/// Parse an SSE byte stream without tying the client to browser EventSource.
pub fn parse_sse_stream<S>(mut input: S) -> impl Stream<Item = Result<SseFrame, EngineError>> + Send
where
    S: Stream<Item = Result<Bytes, reqwest::Error>> + Unpin + Send + 'static,
{
    try_stream! {
        let mut buffer = String::new();
        while let Some(chunk) = input.next().await {
            let chunk = chunk.map_err(|error| EngineError::Transport(error.to_string()))?;
            buffer.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(separator) = find_event_boundary(&buffer) {
                let event_text = buffer[..separator].to_owned();
                buffer.drain(..separator + boundary_length(&buffer[separator..]));
                if let Some(frame) = parse_frame(&event_text) {
                    yield frame;
                }
            }
        }
        if !buffer.trim().is_empty()
            && let Some(frame) = parse_frame(&buffer)
        {
            yield frame;
        }
    }
}

fn find_event_boundary(buffer: &str) -> Option<usize> {
    buffer.find("\n\n").or_else(|| buffer.find("\r\n\r\n"))
}

fn boundary_length(boundary: &str) -> usize {
    if boundary.starts_with("\r\n\r\n") {
        4
    } else {
        2
    }
}

fn parse_frame(event_text: &str) -> Option<SseFrame> {
    let mut id = None;
    let mut event = None;
    let mut data = Vec::new();
    for line in event_text.lines() {
        if line.starts_with(':') {
            continue;
        }
        let (field, value) = line.split_once(':').map_or((line, ""), |(field, value)| {
            (field, value.strip_prefix(' ').unwrap_or(value))
        });
        match field {
            "id" => id = Some(value.to_owned()),
            "event" => event = Some(value.to_owned()),
            "data" => data.push(value),
            _ => {}
        }
    }
    if id.is_none() && event.is_none() && data.is_empty() {
        None
    } else {
        Some(SseFrame {
            id,
            event,
            data: data.join("\n"),
        })
    }
}

#[cfg(test)]
mod tests {
    use futures_util::StreamExt;

    use super::*;

    #[tokio::test]
    async fn parses_multiline_data_and_comments() {
        let chunks = futures_util::stream::iter([
            Ok::<Bytes, reqwest::Error>(Bytes::from_static(b": ping\n\n")),
            Ok::<Bytes, reqwest::Error>(Bytes::from_static(
                b"id: 4\nevent: run-event\ndata: {\"seq\":4,\n",
            )),
            Ok::<Bytes, reqwest::Error>(Bytes::from_static(b"data: \"type\":\"run\"}\n\n")),
        ]);
        let frames: Vec<_> = parse_sse_stream(chunks).collect().await;
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].as_ref().unwrap().id.as_deref(), Some("4"));
        assert_eq!(
            frames[0].as_ref().unwrap().event.as_deref(),
            Some("run-event")
        );
        assert_eq!(
            frames[0].as_ref().unwrap().data,
            "{\"seq\":4,\n\"type\":\"run\"}"
        );
    }
}
