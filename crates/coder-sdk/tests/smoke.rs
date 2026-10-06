//! Run with `scripts/smoke.sh`, which starts a coderd container and sets the environment.

use coder_sdk::{Client, StreamEventType, discover_session};
use futures::StreamExt;
use std::time::Duration;

fn client() -> Client {
    Client::new(&discover_session().expect("CODER_URL and CODER_SESSION_TOKEN")).unwrap()
}

fn org() -> String {
    std::env::var("CODER_SMOKE_ORG").expect("CODER_SMOKE_ORG")
}

async fn create_idle_chat(c: &Client) -> uuid::Uuid {
    let url = c.base_url().join("/api/v2/chats").unwrap();
    let response = c
        .http()
        .post(url)
        .json(&serde_json::json!({"organization_id": org(), "content": []}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status().as_u16(),
        201,
        "{}",
        response.text().await.unwrap()
    );
    let chat: serde_json::Value = response.json().await.unwrap();
    chat["id"].as_str().unwrap().parse().unwrap()
}

#[tokio::test]
#[ignore = "needs scripts/smoke.sh"]
async fn server_version_is_reported() {
    let version = client().server_version().await.unwrap();
    assert!(version.starts_with('v'), "{version}");
}

#[tokio::test]
#[ignore = "needs scripts/smoke.sh"]
async fn generated_list_chats_decodes_real_response() {
    let c = client();
    create_idle_chat(&c).await;
    let chats = c
        .api()
        .list_chats(None, None, None, None, None)
        .await
        .expect("list_chats");
    assert!(!chats.into_inner().is_empty());
}

#[tokio::test]
#[ignore = "needs scripts/smoke.sh"]
async fn stream_snapshot_reports_waiting_status() {
    let c = client();
    let chat = create_idle_chat(&c).await;
    let mut events = c.stream_chat(chat, None).await.unwrap();
    let found = tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(event) = events.next().await {
            let event = event.unwrap();
            if event.kind == StreamEventType::Status {
                return event.raw["status"]["status"].as_str().map(str::to_owned);
            }
        }
        None
    })
    .await
    .expect("status event within 10s");
    assert_eq!(found.as_deref(), Some("waiting"));
}

#[tokio::test]
#[ignore = "needs scripts/smoke.sh"]
async fn watch_reports_created_chat() {
    let c = client();
    let mut events = c.watch_chats().await.unwrap();
    let chat = create_idle_chat(&c).await;
    let seen = tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(event) = events.next().await {
            let event = event.unwrap();
            if event.kind == "created"
                && event.raw["chat"]["id"].as_str() == Some(&chat.to_string())
            {
                return true;
            }
        }
        false
    })
    .await
    .expect("created event within 10s");
    assert!(seen);
}
