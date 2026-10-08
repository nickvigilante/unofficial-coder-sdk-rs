//! The user's own AI provider keys, at `/api/v2/users/me/ai-provider-keys`.
//!
//! The generated client covers these routes, but `update_user_ai_provider_key` takes the key
//! as an `Option<String>` field and its errors skip the token redaction, so these calls are
//! built by hand: the key goes from the caller's buffer into a zeroized body, plain http is
//! refused for any host but this machine, and an error that repeats the key is hidden.
//! Only an error that repeats the whole key is hidden; one that echoes part of it is out of
//! scope.
//! The server never returns a key, only whether one is set (`UserAIProviderKeyConfig` in
//! `codersdk/chats.go`).

use serde::Deserialize;
use zeroize::Zeroizing;

use crate::{Client, Error, Result};

/// One provider as the user sees it: whether they set a key, whether the deployment has one,
/// and whether the deployment accepts personal keys at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderKeyStatus {
    pub provider_id: uuid::Uuid,
    pub name: String,
    pub display_name: String,
    /// `openai`, `anthropic`, and so on, as the server names the provider's type.
    pub provider_type: String,
    pub enabled: bool,
    pub has_user_key: bool,
    pub has_deployment_key: bool,
    /// The deployment's `AllowBYOK`, repeated on every row.
    pub byok_enabled: bool,
}

#[derive(Deserialize)]
struct WireProvider {
    id: uuid::Uuid,
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    enabled: bool,
}

#[derive(Deserialize)]
struct WireRow {
    provider: WireProvider,
    #[serde(default)]
    has_user_api_key: bool,
    #[serde(default)]
    has_provider_api_key: bool,
    #[serde(default)]
    byok_enabled: bool,
}

impl From<WireRow> for ProviderKeyStatus {
    fn from(w: WireRow) -> ProviderKeyStatus {
        ProviderKeyStatus {
            provider_id: w.provider.id,
            display_name: if w.provider.display_name.is_empty() {
                w.provider.name.clone()
            } else {
                w.provider.display_name
            },
            name: w.provider.name,
            provider_type: w.provider.kind,
            enabled: w.provider.enabled,
            has_user_key: w.has_user_api_key,
            has_deployment_key: w.has_provider_api_key,
            byok_enabled: w.byok_enabled,
        }
    }
}

/// Whether a key may be sent to `url`: always over https, and over http only to this machine.
pub(crate) fn sends_keys_to(url: &url::Url) -> bool {
    if url.scheme() != "http" {
        return true;
    }
    match url.host() {
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

/// `{"api_key":<key as a JSON string>}` in a zeroized buffer. Every byte escapes to at most six
/// (`\u001f`), so reserving that much up front means the buffer never reallocates and leaves
/// no copy of the key behind.
pub(crate) fn key_body(key: &str) -> Result<Zeroizing<Vec<u8>>> {
    let mut body = Zeroizing::new(Vec::with_capacity(key.len() * 6 + 16));
    body.extend_from_slice(b"{\"api_key\":");
    serde_json::to_writer(&mut *body, key).map_err(|e| Error::Decode(e.to_string()))?;
    body.push(b'}');
    Ok(body)
}

/// `err` with its text replaced when it repeats `key`, so a server or proxy that echoes the
/// request cannot put the key in front of the user.
fn hide_key(err: Error, key: &str) -> Error {
    const HIDDEN: &str = "the server's reply was hidden because it contained the key";
    if key.is_empty() {
        return err;
    }
    match err {
        Error::Api {
            status,
            message,
            detail,
            validations,
        } => {
            let leaked = message.contains(key)
                || detail.as_deref().is_some_and(|d| d.contains(key))
                || validations.iter().any(|v| v.detail.contains(key));
            if leaked {
                Error::Api {
                    status,
                    message: HIDDEN.into(),
                    detail: None,
                    validations: Vec::new(),
                }
            } else {
                Error::Api {
                    status,
                    message,
                    detail,
                    validations,
                }
            }
        }
        Error::Transport(text) if text.contains(key) => Error::Transport(HIDDEN.into()),
        Error::Timeout(text) if text.contains(key) => Error::Timeout(HIDDEN.into()),
        Error::Decode(text) if text.contains(key) => Error::Decode(HIDDEN.into()),
        other => other,
    }
}

impl Client {
    fn provider_keys_url(&self, provider: Option<uuid::Uuid>) -> Result<url::Url> {
        let path = match provider {
            Some(id) => format!("/api/v2/users/me/ai-provider-keys/{id}"),
            None => "/api/v2/users/me/ai-provider-keys".to_owned(),
        };
        self.base_url()
            .join(&path)
            .map_err(|e| Error::Transport(e.to_string()))
    }

    /// Every provider the user can set a key for, with whether one is set. Never a key.
    pub async fn list_provider_keys(&self) -> Result<Vec<ProviderKeyStatus>> {
        let response = self
            .http()
            .get(self.provider_keys_url(None)?)
            .send()
            .await?;
        let status = response.status().as_u16();
        let body = response.bytes().await?;
        if status != 200 {
            return Err(self.error_from_status(status, &body));
        }
        let rows: Vec<WireRow> =
            serde_json::from_slice(&body).map_err(|e| Error::Decode(e.to_string()))?;
        Ok(rows.into_iter().map(ProviderKeyStatus::from).collect())
    }

    /// Stores `key` as the user's key for `provider`, replacing any key set before. The key
    /// is read from the caller's buffer into a zeroized body; `body.to_vec()` then makes a
    /// plain copy that reqwest owns, which is freed but not wiped.
    pub async fn set_provider_key(
        &self,
        provider: uuid::Uuid,
        key: &str,
    ) -> Result<ProviderKeyStatus> {
        if !sends_keys_to(self.base_url()) {
            return Err(Error::Transport(
                "refusing to send a provider key over plain http to a host other than this machine"
                    .into(),
            ));
        }
        let url = self.provider_keys_url(Some(provider))?;
        let body = key_body(key)?;
        let response = self
            .http()
            .put(url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.to_vec())
            .send()
            .await
            .map_err(|e| hide_key(Error::from(e), key))?;
        drop(body);
        let status = response.status().as_u16();
        let bytes = response.bytes().await?;
        if status != 200 {
            return Err(hide_key(self.error_from_status(status, &bytes), key));
        }
        let row: WireRow = serde_json::from_slice(&bytes)
            .map_err(|e| hide_key(Error::Decode(e.to_string()), key))?;
        Ok(row.into())
    }

    /// Removes the user's key for `provider`. The server answers 204 whether or not one was set.
    pub async fn delete_provider_key(&self, provider: uuid::Uuid) -> Result<()> {
        let response = self
            .http()
            .delete(self.provider_keys_url(Some(provider))?)
            .send()
            .await?;
        let status = response.status().as_u16();
        if status == 204 || status == 200 {
            return Ok(());
        }
        let bytes = response.bytes().await?;
        Err(self.error_from_status(status, &bytes))
    }
}

#[cfg(test)]
mod tests {
    use secrecy::SecretString;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::{Client, Error, Session};

    const TOKEN: &str = "test-token-not-real";
    const KEY: &str = "sk-test-not-real-0001";

    fn client(url: &str) -> Client {
        Client::new(&Session {
            url: url.parse().unwrap(),
            token: SecretString::from(TOKEN),
        })
        .unwrap()
    }

    fn row(id: uuid::Uuid, has_user: bool) -> serde_json::Value {
        serde_json::json!({
            "provider": {
                "id": id, "type": "openai", "name": "openai", "display_name": "OpenAI",
                "icon": "", "enabled": true, "deleted": false
            },
            "has_user_api_key": has_user,
            "has_provider_api_key": true,
            "byok_enabled": true
        })
    }

    #[tokio::test]
    async fn lists_each_provider_and_whether_a_key_is_set() {
        let server = MockServer::start().await;
        let id = uuid::Uuid::new_v4();
        Mock::given(method("GET"))
            .and(path("/api/v2/users/me/ai-provider-keys"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!([row(id, true)])),
            )
            .mount(&server)
            .await;
        let rows = client(&server.uri()).list_provider_keys().await.unwrap();
        assert_eq!(
            rows,
            vec![super::ProviderKeyStatus {
                provider_id: id,
                name: "openai".into(),
                display_name: "OpenAI".into(),
                provider_type: "openai".into(),
                enabled: true,
                has_user_key: true,
                has_deployment_key: true,
                byok_enabled: true,
            }]
        );
    }

    #[tokio::test]
    async fn sets_a_key_with_the_key_only_in_the_body() {
        let server = MockServer::start().await;
        let id = uuid::Uuid::new_v4();
        Mock::given(method("PUT"))
            .and(path(format!("/api/v2/users/me/ai-provider-keys/{id}")))
            .and(body_json(serde_json::json!({ "api_key": KEY })))
            .respond_with(ResponseTemplate::new(200).set_body_json(row(id, true)))
            .expect(1)
            .mount(&server)
            .await;
        let saved = client(&server.uri())
            .set_provider_key(id, KEY)
            .await
            .unwrap();
        assert!(saved.has_user_key);
        let requests = server.received_requests().await.unwrap();
        let url = requests[0].url.to_string();
        assert!(!url.contains(KEY), "the key never goes in the URL: {url}");
    }

    #[tokio::test]
    async fn a_key_with_quotes_and_control_characters_is_escaped() {
        let server = MockServer::start().await;
        let id = uuid::Uuid::new_v4();
        let odd = "sk-\"quoted\"\\-\u{1}-end";
        Mock::given(method("PUT"))
            .and(path(format!("/api/v2/users/me/ai-provider-keys/{id}")))
            .and(body_json(serde_json::json!({ "api_key": odd })))
            .respond_with(ResponseTemplate::new(200).set_body_json(row(id, true)))
            .expect(1)
            .mount(&server)
            .await;
        client(&server.uri())
            .set_provider_key(id, odd)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn an_error_that_echoes_the_key_is_hidden() {
        let server = MockServer::start().await;
        let id = uuid::Uuid::new_v4();
        Mock::given(method("PUT"))
            .and(path(format!("/api/v2/users/me/ai-provider-keys/{id}")))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "message": format!("bad key {KEY}"),
                "detail": format!("got {KEY} with {TOKEN}"),
            })))
            .mount(&server)
            .await;
        let err = client(&server.uri())
            .set_provider_key(id, KEY)
            .await
            .unwrap_err();
        let shown = format!("{err} {err:?}");
        assert!(!shown.contains(KEY), "{shown}");
        assert!(!shown.contains(TOKEN), "{shown}");
        assert!(shown.contains("contained the key"), "{shown}");
    }

    #[tokio::test]
    async fn a_refusal_keeps_the_servers_message() {
        let server = MockServer::start().await;
        let id = uuid::Uuid::new_v4();
        Mock::given(method("PUT"))
            .and(path(format!("/api/v2/users/me/ai-provider-keys/{id}")))
            .respond_with(
                ResponseTemplate::new(403)
                    .set_body_json(serde_json::json!({ "message": "BYOK is disabled." })),
            )
            .mount(&server)
            .await;
        match client(&server.uri()).set_provider_key(id, KEY).await {
            Err(Error::Api {
                status: 403,
                message,
                ..
            }) => assert_eq!(message, "BYOK is disabled."),
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn plain_http_is_refused_unless_the_host_is_this_machine() {
        let id = uuid::Uuid::new_v4();
        match client("http://coder.example.com")
            .set_provider_key(id, KEY)
            .await
        {
            Err(Error::Transport(text)) => {
                assert!(text.contains("plain http"), "{text}");
                assert!(!text.contains(KEY), "{text}");
            }
            other => panic!("{other:?}"),
        }
        // A loopback host is allowed, so the wiremock tests above can run over http.
        for url in ["http://localhost:1", "http://127.0.0.1:1", "http://[::1]:1"] {
            assert!(super::sends_keys_to(&url.parse().unwrap()), "{url}");
        }
        assert!(!super::sends_keys_to(&"http://10.0.0.1".parse().unwrap()));
        assert!(super::sends_keys_to(
            &"https://coder.example.com".parse().unwrap()
        ));
    }

    #[tokio::test]
    async fn deletes_a_key() {
        let server = MockServer::start().await;
        let id = uuid::Uuid::new_v4();
        Mock::given(method("DELETE"))
            .and(path(format!("/api/v2/users/me/ai-provider-keys/{id}")))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        client(&server.uri()).delete_provider_key(id).await.unwrap();
    }

    #[test]
    fn the_body_reserves_room_for_the_worst_escape_up_front() {
        let body = super::key_body("ab\u{1}").unwrap();
        assert_eq!(&body[..], br#"{"api_key":"ab\u0001"}"#);
        assert!(
            body.capacity() >= 3 * 6 + 16,
            "capacity {}",
            body.capacity()
        );
    }
}
