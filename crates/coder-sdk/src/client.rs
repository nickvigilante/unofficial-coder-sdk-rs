use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderValue};
use reqwest::redirect::{Action, Attempt, Policy};
use secrecy::{ExposeSecret, SecretString};
use url::Url;

use crate::{Error, Result};

/// True when `a` and `b` share a scheme, host, and (explicit or default) port.
fn same_origin(a: &Url, b: &Url) -> bool {
    a.scheme() == b.scheme()
        && a.host() == b.host()
        && a.port_or_known_default() == b.port_or_known_default()
}

/// Stops any redirect that crosses origins, so the `Coder-Session-Token` header reqwest
/// forwards on redirects never reaches a host other than the one the caller asked for.
/// Same-origin redirects still follow, up to reqwest's usual 10-hop default.
fn same_origin_redirect_policy(attempt: Attempt) -> Action {
    if attempt.previous().len() > 10 {
        return attempt.error("too many redirects");
    }
    match attempt.previous().first() {
        Some(original) if !same_origin(original, attempt.url()) => attempt.stop(),
        _ => attempt.follow(),
    }
}

/// How long any connection attempt, HTTP or WebSocket, may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a non-stream HTTP request may take end to end. WebSocket streams get no request
/// timeout, because a healthy stream stays open indefinitely.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// The `ClientBuilder` settings shared by every `reqwest::Client` this SDK builds.
fn base_builder(headers: HeaderMap) -> reqwest::ClientBuilder {
    let user_agent = concat!("unofficial-coder-sdk-rs/", env!("CARGO_PKG_VERSION"));
    reqwest::Client::builder()
        .default_headers(headers)
        .user_agent(user_agent)
        .redirect(Policy::custom(same_origin_redirect_policy))
}

/// A Coder deployment URL and the session token used to call it.
#[derive(Debug, Clone)]
pub struct Session {
    pub url: Url,
    pub token: SecretString,
}

/// An authenticated Coder API client.
#[derive(Clone)]
pub struct Client {
    base: Url,
    http: reqwest::Client,
    /// A client restricted to HTTP/1.1, used only for WebSocket upgrades.
    ///
    /// Over TLS, the shared client's ALPN offers h2, and Go servers pick it, which fails the
    /// `Upgrade: websocket` handshake. `reqwest-websocket`'s own `websocket()` helper forces
    /// `http1_only()` for the same reason.
    ws_http: reqwest::Client,
    api: coder_api_gen::Client,
    /// Kept only to redact the token out of a server response that echoes it back, such as a
    /// refused WebSocket upgrade; never logged, printed, or otherwise rendered.
    token: SecretString,
}

impl Client {
    /// Builds a client that sends the session token on every request, including WebSocket upgrades.
    pub fn new(session: &Session) -> Result<Client> {
        Self::with_timeouts(session, CONNECT_TIMEOUT, REQUEST_TIMEOUT)
    }

    /// `new` with caller-chosen timeouts, so tests need not wait a full minute.
    pub(crate) fn with_timeouts(
        session: &Session,
        connect: Duration,
        request: Duration,
    ) -> Result<Client> {
        let mut token = HeaderValue::from_str(session.token.expose_secret())
            .map_err(|_| Error::InvalidToken)?;
        token.set_sensitive(true);
        let mut headers = HeaderMap::new();
        headers.insert("Coder-Session-Token", token);
        let http = base_builder(headers.clone())
            .connect_timeout(connect)
            .timeout(request)
            .build()?;
        let ws_http = base_builder(headers)
            .http1_only()
            .connect_timeout(connect)
            .build()?;
        let base = session.url.clone();
        let api = coder_api_gen::Client::new_with_client(
            base.as_str().trim_end_matches('/'),
            http.clone(),
        );
        Ok(Client {
            base,
            http,
            ws_http,
            api,
            token: session.token.clone(),
        })
    }

    /// The generated client for any endpoint coder-sdk does not wrap.
    pub fn api(&self) -> &coder_api_gen::Client {
        &self.api
    }

    /// The underlying HTTP client, already carrying the session token.
    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    /// The HTTP/1.1-only client used for WebSocket upgrades.
    pub(crate) fn ws_http(&self) -> &reqwest::Client {
        &self.ws_http
    }

    /// Replaces every occurrence of the session token in `text` with `[redacted]`. Defends
    /// against a server response that echoes the token back, such as an auth failure message
    /// on a refused WebSocket upgrade.
    pub(crate) fn redact_token(&self, text: &str) -> String {
        text.replace(self.token.expose_secret(), "[redacted]")
    }

    /// [`Error::from_status`] for a response to a request this client sent by hand, with the
    /// session token redacted from the message, the detail, and every validation, in case the
    /// server or a proxy echoes it back.
    pub(crate) fn error_from_status(&self, status: u16, body: &[u8]) -> Error {
        let mut err = Error::from_status(status, body);
        if let Error::Api {
            message,
            detail,
            validations,
            ..
        } = &mut err
        {
            *message = self.redact_token(message);
            if let Some(detail) = detail {
                *detail = self.redact_token(detail);
            }
            for validation in validations.iter_mut() {
                validation.detail = self.redact_token(&validation.detail);
            }
        }
        err
    }

    /// The deployment URL.
    pub fn base_url(&self) -> &Url {
        &self.base
    }

    /// The server's version string from `/api/v2/buildinfo`. A refusal's text has the session
    /// token redacted, as every other hand-built request's does.
    pub async fn server_version(&self) -> Result<String> {
        let url = self
            .base
            .join("/api/v2/buildinfo")
            .map_err(|e| Error::Transport(e.to_string()))?;
        let response = self.http.get(url).send().await?;
        let status = response.status().as_u16();
        let body = response.bytes().await?;
        if status != 200 {
            return Err(self.error_from_status(status, &body));
        }
        let value: serde_json::Value =
            serde_json::from_slice(&body).map_err(|e| Error::Decode(e.to_string()))?;
        value["version"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| Error::Decode("buildinfo has no version".into()))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use futures::{SinkExt, StreamExt};
    use secrecy::SecretString;
    use tokio::net::TcpListener;
    use tokio_tungstenite::tungstenite::Message;
    use tokio_tungstenite::tungstenite::protocol::CloseFrame;
    use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

    use super::{Client, Session};
    use crate::Error;

    fn client(addr: std::net::SocketAddr, request: Duration) -> Client {
        Client::with_timeouts(
            &Session {
                url: format!("http://{addr}").parse().unwrap(),
                token: SecretString::from("test-token-not-real"),
            },
            Duration::from_secs(5),
            request,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn a_hung_request_fails_after_the_request_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (hold_tx, hold_rx) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(async move {
            // Accept the connection and never answer.
            let (_tcp, _) = listener.accept().await.unwrap();
            let _ = hold_rx.await;
        });
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            client(addr, Duration::from_millis(300)).server_version(),
        )
        .await
        .expect("the request timeout must fire before the test ceiling");
        drop(hold_tx);
        assert!(matches!(result, Err(Error::Transport(_))), "{result:?}");
    }

    #[tokio::test]
    async fn a_stream_outlives_the_request_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            tokio::time::sleep(Duration::from_millis(800)).await;
            ws.send(Message::text(r#"[{"type":"preview_reset"}]"#))
                .await
                .unwrap();
            ws.close(Some(CloseFrame {
                code: CloseCode::Normal,
                reason: "".into(),
            }))
            .await
            .unwrap();
            while ws.next().await.is_some() {}
        });
        let events: Vec<_> = tokio::time::timeout(
            Duration::from_secs(10),
            client(addr, Duration::from_millis(200))
                .stream_chat(uuid::Uuid::new_v4(), None)
                .await
                .unwrap()
                .collect(),
        )
        .await
        .unwrap();
        assert_eq!(events.len(), 1, "{events:?}");
        assert!(events[0].is_ok());
    }
}
