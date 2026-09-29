use std::time::Duration;

use coder_sdk::{Client, Error, Session, StreamEventType};
use futures::{SinkExt, StreamExt};
use secrecy::SecretString;
use tokio::net::TcpListener;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

/// The ceiling for every test's `collect()`: a hang fails the test instead of stalling CI.
const COLLECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Starts a one-connection WebSocket server that sends `frames`, then closes with `close`.
async fn serve(frames: Vec<String>, close: Option<CloseFrame>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
        for frame in frames {
            ws.send(Message::text(frame)).await.unwrap();
        }
        match close {
            Some(frame) => ws.close(Some(frame)).await.unwrap(),
            None => drop(ws),
        }
    });
    format!("http://{addr}")
}

fn client(url: &str) -> Client {
    Client::new(&Session {
        url: url.parse().unwrap(),
        token: SecretString::from("test-token-not-real"),
    })
    .unwrap()
}

fn normal_close() -> Option<CloseFrame> {
    Some(CloseFrame {
        code: CloseCode::Normal,
        reason: "".into(),
    })
}

#[tokio::test]
async fn batched_frame_yields_each_event_in_order() {
    let frame = r#"[{"type":"status","status":{"status":"running"}},{"type":"preview_reset"},{"type":"status","status":{"status":"waiting"}}]"#;
    let url = serve(vec![frame.into()], normal_close()).await;
    let events: Vec<_> = timeout(
        COLLECT_TIMEOUT,
        client(&url)
            .stream_chat(uuid::Uuid::new_v4(), None)
            .await
            .unwrap()
            .collect(),
    )
    .await
    .unwrap();
    let kinds: Vec<_> = events.into_iter().map(|e| e.unwrap().kind).collect();
    assert_eq!(
        kinds,
        vec![
            StreamEventType::Status,
            StreamEventType::PreviewReset,
            StreamEventType::Status
        ]
    );
}

#[tokio::test]
async fn unknown_event_type_is_yielded_not_fatal() {
    let frame = r#"[{"type":"brand_new_event","whatever":1},{"type":"status","status":{"status":"a_status_from_the_future"}}]"#;
    let url = serve(vec![frame.into()], normal_close()).await;
    let events: Vec<_> = timeout(
        COLLECT_TIMEOUT,
        client(&url)
            .stream_chat(uuid::Uuid::new_v4(), None)
            .await
            .unwrap()
            .collect(),
    )
    .await
    .unwrap();
    assert_eq!(events.len(), 2);
    let first = events[0].as_ref().unwrap();
    assert_eq!(
        first.kind,
        StreamEventType::Unknown("brand_new_event".into())
    );
    assert_eq!(first.raw["whatever"], 1);
    let second = events[1].as_ref().unwrap();
    assert_eq!(second.kind, StreamEventType::Status);
    assert!(
        second.event.is_some(),
        "an unknown status value must still decode"
    );
}

#[tokio::test]
async fn abnormal_close_yields_error_then_ends() {
    let url = serve(vec![r#"[{"type":"preview_reset"}]"#.into()], None).await;
    let events: Vec<_> = timeout(
        COLLECT_TIMEOUT,
        client(&url)
            .stream_chat(uuid::Uuid::new_v4(), None)
            .await
            .unwrap()
            .collect(),
    )
    .await
    .unwrap();
    assert_eq!(events.len(), 2);
    assert!(events[0].is_ok());
    assert!(matches!(events[1], Err(Error::StreamClosed { .. })));
}

#[tokio::test]
async fn close_frame_with_going_away_yields_error_then_ends() {
    let url = serve(
        vec![r#"[{"type":"preview_reset"}]"#.into()],
        Some(CloseFrame {
            code: CloseCode::Away,
            reason: "".into(),
        }),
    )
    .await;
    let events: Vec<_> = timeout(
        COLLECT_TIMEOUT,
        client(&url)
            .stream_chat(uuid::Uuid::new_v4(), None)
            .await
            .unwrap()
            .collect(),
    )
    .await
    .unwrap();
    assert_eq!(events.len(), 2);
    assert!(events[0].is_ok());
    assert!(matches!(
        events[1],
        Err(Error::StreamClosed {
            code: Some(1001),
            ..
        })
    ));
}

#[tokio::test]
async fn large_batched_frame_yields_all_events_in_order() {
    let big_text = "x".repeat(5_000);
    let events: Vec<String> = (0..256)
        .map(|i| {
            format!(
                r#"{{"type":"message_part","message_part":{{"role":"assistant","seq":{i},"part":{{"type":"text","text":"{big_text}"}}}}}}"#,
                i = i + 1
            )
        })
        .collect();
    let frame = format!("[{}]", events.join(","));
    assert!(frame.len() > 1_000_000);
    let url = serve(vec![frame], normal_close()).await;
    let got: Vec<_> = timeout(
        COLLECT_TIMEOUT,
        client(&url)
            .stream_chat(uuid::Uuid::new_v4(), None)
            .await
            .unwrap()
            .collect(),
    )
    .await
    .unwrap();
    assert_eq!(got.len(), 256);
    let seqs: Vec<i64> = got
        .iter()
        .map(|e| {
            e.as_ref().unwrap().raw["message_part"]["seq"]
                .as_i64()
                .unwrap()
        })
        .collect();
    assert_eq!(seqs, (1..=256).collect::<Vec<_>>());
}

#[tokio::test]
async fn upgrade_401_maps_to_unauthorized() {
    let server = wiremock::MockServer::start().await;
    // Only a request carrying the session token matches; an unmatched request gets wiremock's
    // default 404, which would surface as `Error::Api` and fail the assertion below. That is
    // what pins the token being sent on the upgrade request, not just on ordinary calls.
    wiremock::Mock::given(wiremock::matchers::header(
        "Coder-Session-Token",
        "test-token-not-real",
    ))
    .respond_with(wiremock::ResponseTemplate::new(401))
    .mount(&server)
    .await;
    let result = client(&server.uri())
        .stream_chat(uuid::Uuid::new_v4(), None)
        .await;
    assert!(matches!(result, Err(Error::Unauthorized)));
}

#[tokio::test]
async fn watch_yields_one_event_per_frame_with_kind() {
    let url = serve(
        vec![
            r#"{"kind":"title_change","chat":{"id":"00000000-0000-0000-0000-000000000001","title":"Hi"}}"#.into(),
            r#"{"kind":"something_new"}"#.into(),
        ],
        normal_close(),
    )
    .await;
    let got: Vec<_> = timeout(
        COLLECT_TIMEOUT,
        client(&url).watch_chats().await.unwrap().collect(),
    )
    .await
    .unwrap();
    let kinds: Vec<_> = got.into_iter().map(|e| e.unwrap().kind).collect();
    assert_eq!(
        kinds,
        vec!["title_change".to_string(), "something_new".to_string()]
    );
}

/// Connects to the developer's real Coder deployment and confirms the WebSocket upgrade
/// succeeds over TLS. This is the regression test for the h2-ALPN-vs-upgrade failure mode:
/// the shared `reqwest::Client` (http2 enabled by default) picks h2 over TLS, which a Go
/// server's `Upgrade: websocket` handshake rejects, so this must run against a real TLS
/// deployment rather than the plaintext `serve()` test server above to catch it.
///
/// This test prints nothing derived from the session token; it only asserts on whether the
/// upgrade and the subsequent read succeeded.
#[tokio::test]
#[ignore = "connects to the developer's real deployment"]
async fn real_watch_upgrades_over_tls() {
    let session = coder_sdk::discover_session().expect("discover a real coder cli session");
    let client = Client::new(&session).expect("build a client from the discovered session");
    let result = client.watch_chats().await;
    assert!(
        result.is_ok(),
        "the websocket upgrade must succeed over TLS"
    );
    let stream = result.unwrap();
    // Wait at most 5s for either the first event or a timeout; either outcome is fine here,
    // since the upgrade itself (already asserted above) is what this test pins.
    let _ = timeout(Duration::from_secs(5), stream.take(1).collect::<Vec<_>>()).await;
}
