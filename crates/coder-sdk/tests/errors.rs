use coder_sdk::{Client, Error, Session};
use secrecy::SecretString;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn client(server: &MockServer) -> Client {
    let session = Session {
        url: server.uri().parse().unwrap(),
        token: SecretString::from("test-token-not-real"),
    };
    Client::new(&session).unwrap()
}

#[tokio::test]
async fn server_version_sends_token_and_parses_version() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/buildinfo"))
        .and(header("Coder-Session-Token", "test-token-not-real"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"version": "v2.38.0+abc"})),
        )
        .mount(&server)
        .await;
    assert_eq!(
        client(&server).await.server_version().await.unwrap(),
        "v2.38.0+abc"
    );
}

#[tokio::test]
async fn unauthorized_maps_to_unauthorized() {
    let server = MockServer::start().await;
    Mock::given(path("/api/v2/buildinfo"))
        .respond_with(
            ResponseTemplate::new(401).set_body_json(serde_json::json!({"message": "no"})),
        )
        .mount(&server)
        .await;
    assert!(matches!(
        client(&server).await.server_version().await,
        Err(Error::Unauthorized)
    ));
}

#[tokio::test]
async fn generated_call_errors_keep_server_message_and_validations() {
    let server = MockServer::start().await;
    let chat = uuid::Uuid::new_v4();
    Mock::given(path(format!("/api/v2/chats/{chat}")))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "message": "Invalid request.",
            "detail": "bad field",
            "validations": [{"field": "title", "detail": "too long"}]
        })))
        .mount(&server)
        .await;
    let c = client(&server).await;
    let err = match c.api().get_chat_by_id(&chat).await {
        Ok(_) => panic!("expected an error"),
        Err(e) => Error::from_progenitor(e).await,
    };
    match err {
        Error::Api {
            status,
            message,
            detail,
            validations,
        } => {
            assert_eq!(status, 400);
            assert_eq!(message, "Invalid request.");
            assert_eq!(detail.as_deref(), Some("bad field"));
            assert_eq!(validations[0].field, "title");
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn token_never_appears_in_debug_output() {
    let session = Session {
        url: "https://example.com".parse().unwrap(),
        token: SecretString::from("test-token-not-real"),
    };
    assert!(!format!("{session:?}").contains("test-token-not-real"));
}
