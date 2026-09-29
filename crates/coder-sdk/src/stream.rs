//! The chat stream and chat list WebSockets.

use futures::stream::BoxStream;
use futures::{Stream, StreamExt, stream};
use reqwest_websocket::{Message, Upgrade};

use crate::types::{CodersdkChatStreamEvent, CodersdkChatWatchEvent};
use crate::{Client, Error, Result, StreamEventType};

/// One event from `/api/v2/chats/{id}/stream`.
#[derive(Debug, Clone)]
pub struct StreamEvent {
    pub kind: StreamEventType,
    /// The typed event, or `None` if this SDK version could not decode it.
    pub event: Option<CodersdkChatStreamEvent>,
    pub raw: serde_json::Value,
}

/// One event from `/api/v2/chats/watch`.
#[derive(Debug, Clone)]
pub struct WatchEvent {
    pub kind: String,
    pub event: Option<CodersdkChatWatchEvent>,
    pub raw: serde_json::Value,
}

fn stream_event(raw: serde_json::Value) -> StreamEvent {
    let kind = StreamEventType::parse(raw["type"].as_str().unwrap_or_default());
    let event = match &kind {
        StreamEventType::Unknown(_) => None,
        _ => serde_json::from_value(raw.clone()).ok(),
    };
    StreamEvent { kind, event, raw }
}

fn watch_event(raw: serde_json::Value) -> WatchEvent {
    let kind = raw["kind"].as_str().unwrap_or_default().to_owned();
    let event = serde_json::from_value(raw.clone()).ok();
    WatchEvent { kind, event, raw }
}

impl Client {
    async fn open(&self, path_and_query: &str) -> Result<reqwest_websocket::WebSocket> {
        let url = self
            .base_url()
            .join(path_and_query)
            .map_err(|e| Error::Transport(e.to_string()))?;
        let response = self
            .ws_http()
            .get(url)
            .upgrade()
            .send()
            .await
            .map_err(|e| Error::Transport(e.to_string()))?;
        let status = response.status().as_u16();
        if status == 401 {
            return Err(Error::Unauthorized);
        }
        if status != 101 {
            return Err(Error::from_status(status, b""));
        }
        response
            .into_websocket()
            .await
            .map_err(|e| Error::Transport(e.to_string()))
    }

    /// Streams a chat's events. Frames are JSON arrays; each element is yielded separately.
    ///
    /// The stream ending, for any reason, means the subscription is gone: the server sends a
    /// normal (code 1000) close on its own teardown (for example, on redeploy) just as readily
    /// as on a deliberate unsubscribe, so the stream ending is not itself a signal that the
    /// caller asked for it to end. The caller must reconnect by calling `stream_chat` again,
    /// passing the highest durable message id it has seen as `after_id` so the server can replay
    /// anything missed. An `Error::Decode` item is not terminal; the stream keeps going after
    /// it. An `Error::StreamClosed` item, when the stream yields one, is always the last item.
    /// Callers must poll the stream continuously (not pause between items) so the server's pings
    /// are answered; the server closes with code 1001 after about 30 seconds without a pong.
    pub async fn stream_chat(
        &self,
        chat: uuid::Uuid,
        after_id: Option<i64>,
    ) -> Result<BoxStream<'static, Result<StreamEvent>>> {
        let mut path = format!("/api/v2/chats/{chat}/stream");
        if let Some(id) = after_id {
            path.push_str(&format!("?after_id={id}"));
        }
        let socket = self.open(&path).await?;
        Ok(frames(socket)
            .flat_map(|frame| {
                let items: Vec<Result<StreamEvent>> = match frame {
                    Ok(text) => match serde_json::from_str::<Vec<serde_json::Value>>(&text) {
                        Ok(values) => values.into_iter().map(|v| Ok(stream_event(v))).collect(),
                        Err(e) => vec![Err(Error::Decode(e.to_string()))],
                    },
                    Err(e) => vec![Err(e)],
                };
                stream::iter(items)
            })
            .boxed())
    }

    /// Streams chat list changes for the signed-in user, one event per frame.
    ///
    /// The stream ending, for any reason, means the subscription is gone: the server sends a
    /// normal (code 1000) close on its own teardown (for example, on redeploy) just as readily
    /// as on a deliberate unsubscribe, so the stream ending is not itself a signal that the
    /// caller asked for it to end. The caller must reconnect by calling `watch_chats` again;
    /// there is no cursor to resume from, so any events during the gap are missed. An
    /// `Error::Decode` item is not terminal; the stream keeps going after it. An
    /// `Error::StreamClosed` item, when the stream yields one, is always the last item. Callers
    /// must poll the stream continuously (not pause between items) so the server's pings are
    /// answered; the server closes with code 1001 after about 30 seconds without a pong.
    pub async fn watch_chats(&self) -> Result<BoxStream<'static, Result<WatchEvent>>> {
        let socket = self.open("/api/v2/chats/watch").await?;
        Ok(frames(socket)
            .map(|frame| {
                let text = frame?;
                let raw: serde_json::Value =
                    serde_json::from_str(&text).map_err(|e| Error::Decode(e.to_string()))?;
                Ok(watch_event(raw))
            })
            .boxed())
    }
}

/// Text frames until a normal close. Anything else ends with one `StreamClosed` error.
///
/// A normal (code 1000) close ends the stream with no error item at all: the server sends this
/// both when a caller-driven teardown happens and when the server tears the subscription down
/// on its own, so this alone never implies the subscription is still meaningful to reconnect
/// against without also passing along whatever cursor the caller already tracks.
fn frames(socket: reqwest_websocket::WebSocket) -> impl Stream<Item = Result<String>> + use<> {
    stream::unfold(Some(socket), |state| async move {
        let mut socket = state?;
        loop {
            match socket.next().await {
                Some(Ok(Message::Text(text))) => return Some((Ok(text), Some(socket))),
                Some(Ok(Message::Close { code, reason })) => {
                    let code = u16::from(code);
                    if code == 1000 {
                        return None;
                    }
                    return Some((
                        Err(Error::StreamClosed {
                            code: Some(code),
                            reason,
                        }),
                        None,
                    ));
                }
                Some(Ok(_)) => continue,
                Some(Err(e)) => {
                    return Some((
                        Err(Error::StreamClosed {
                            code: None,
                            reason: e.to_string(),
                        }),
                        None,
                    ));
                }
                None => {
                    return Some((
                        Err(Error::StreamClosed {
                            code: None,
                            reason: "connection ended without a close frame".into(),
                        }),
                        None,
                    ));
                }
            }
        }
    })
}
