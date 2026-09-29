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
}

impl Client {
    /// Builds a client that sends the session token on every request, including WebSocket upgrades.
    pub fn new(session: &Session) -> Result<Client> {
        let mut token = HeaderValue::from_str(session.token.expose_secret())
            .map_err(|_| Error::InvalidToken)?;
        token.set_sensitive(true);
        let mut headers = HeaderMap::new();
        headers.insert("Coder-Session-Token", token);
        let http = base_builder(headers.clone()).build()?;
        let ws_http = base_builder(headers).http1_only().build()?;
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

    /// The deployment URL.
    pub fn base_url(&self) -> &Url {
        &self.base
    }

    /// The server's version string from `/api/v2/buildinfo`.
    pub async fn server_version(&self) -> Result<String> {
        let url = self
            .base
            .join("/api/v2/buildinfo")
            .map_err(|e| Error::Transport(e.to_string()))?;
        let response = self.http.get(url).send().await?;
        let status = response.status().as_u16();
        let body = response.bytes().await?;
        if status != 200 {
            return Err(Error::from_status(status, &body));
        }
        let value: serde_json::Value =
            serde_json::from_slice(&body).map_err(|e| Error::Decode(e.to_string()))?;
        value["version"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| Error::Decode("buildinfo has no version".into()))
    }
}
