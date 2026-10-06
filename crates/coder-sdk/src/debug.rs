//! The experimental chat debug runs endpoint, which the generated client does not cover.

use serde::Deserialize;

use crate::{Client, Error, Result};

/// One MCP server's connect outcome, from a debug run's `mcp_connect` summary.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct McpConnectOutcome {
    pub config_id: uuid::Uuid,
    pub slug: String,
    /// `connected`, `timeout`, `error`, or `no_tools`, or a newer value.
    pub outcome: String,
    #[serde(default)]
    pub tool_count: i64,
    /// The redacted connect error, empty when the outcome is `connected` or `no_tools`.
    #[serde(default)]
    pub error: String,
}

#[derive(Deserialize)]
struct RunSummary {
    #[serde(default)]
    summary: serde_json::Map<String, serde_json::Value>,
}

impl Client {
    /// The MCP connect outcomes of `chat`'s newest debug run that recorded any, one per
    /// server with its latest entry, in the order the servers first appear. `None` when no run
    /// recorded outcomes, which is also what a user without debug logging sees. The endpoint is
    /// experimental and may change.
    pub async fn latest_mcp_connect(
        &self,
        chat: uuid::Uuid,
    ) -> Result<Option<Vec<McpConnectOutcome>>> {
        let url = self
            .base_url()
            .join(&format!("/api/experimental/chats/{chat}/debug/runs"))
            .map_err(|e| Error::Transport(e.to_string()))?;
        let response = self.http().get(url).send().await?;
        let status = response.status().as_u16();
        let body = response.bytes().await?;
        if status != 200 {
            return Err(self.error_from_status(status, &body));
        }
        let runs: Vec<RunSummary> =
            serde_json::from_slice(&body).map_err(|e| Error::Decode(e.to_string()))?;
        for run in runs {
            let Some(entries) = run.summary.get("mcp_connect") else {
                continue;
            };
            let entries: Vec<McpConnectOutcome> = serde_json::from_value(entries.clone())
                .map_err(|e| Error::Decode(e.to_string()))?;
            if entries.is_empty() {
                continue;
            }
            let mut latest: Vec<McpConnectOutcome> = Vec::new();
            for entry in entries {
                match latest.iter_mut().find(|o| o.config_id == entry.config_id) {
                    Some(slot) => *slot = entry,
                    None => latest.push(entry),
                }
            }
            return Ok(Some(latest));
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use secrecy::SecretString;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::{Client, Error, Session};

    fn client(url: &str) -> Client {
        Client::new(&Session {
            url: url.parse().unwrap(),
            token: SecretString::from("test-token-not-real"),
        })
        .unwrap()
    }

    #[tokio::test]
    async fn the_newest_run_with_outcomes_wins_and_the_last_entry_per_server_counts() {
        let server = MockServer::start().await;
        let chat = uuid::Uuid::new_v4();
        let (github, linear) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        Mock::given(method("GET"))
            .and(path(format!("/api/experimental/chats/{chat}/debug/runs")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": uuid::Uuid::new_v4(), "summary": {}},
                {"id": uuid::Uuid::new_v4(), "summary": {"mcp_connect": [
                    {"config_id": github, "slug": "github", "outcome": "connected", "duration_ms": 40, "tool_count": 12},
                    {"config_id": linear, "slug": "linear", "outcome": "timeout", "duration_ms": 5000, "error": "context deadline exceeded"},
                    {"config_id": github, "slug": "github", "outcome": "error", "duration_ms": 9, "error": "401 Unauthorized"}
                ]}},
                {"id": uuid::Uuid::new_v4(), "summary": {"mcp_connect": [
                    {"config_id": linear, "slug": "linear", "outcome": "connected", "duration_ms": 30}
                ]}}
            ])))
            .mount(&server)
            .await;
        let outcomes = client(&server.uri())
            .latest_mcp_connect(chat)
            .await
            .unwrap()
            .expect("a run recorded outcomes");
        let by_slug: Vec<(&str, &str)> = outcomes
            .iter()
            .map(|o| (o.slug.as_str(), o.outcome.as_str()))
            .collect();
        assert_eq!(by_slug, [("github", "error"), ("linear", "timeout")]);
        assert_eq!(outcomes[0].error, "401 Unauthorized");
    }

    #[tokio::test]
    async fn no_recorded_outcomes_is_none_and_a_refusal_is_an_error() {
        let server = MockServer::start().await;
        let (quiet, hidden) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        Mock::given(method("GET"))
            .and(path(format!("/api/experimental/chats/{quiet}/debug/runs")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/api/experimental/chats/{hidden}/debug/runs")))
            .respond_with(
                ResponseTemplate::new(404)
                    .set_body_json(serde_json::json!({"message": "Resource not found"})),
            )
            .mount(&server)
            .await;
        let c = client(&server.uri());
        assert_eq!(c.latest_mcp_connect(quiet).await.unwrap(), None);
        assert!(matches!(
            c.latest_mcp_connect(hidden).await,
            Err(Error::Api { status: 404, .. })
        ));
    }

    #[tokio::test]
    async fn a_refusal_names_the_message_but_not_the_token() {
        let server = MockServer::start().await;
        let chat = uuid::Uuid::new_v4();
        let secret_token = "s3cr3t-session-token-do-not-leak";
        Mock::given(method("GET"))
            .and(path(format!("/api/experimental/chats/{chat}/debug/runs")))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "message": format!("bad token {secret_token}"),
                "detail": format!("echoed {secret_token}"),
                "validations": [{"field": "token", "detail": format!("got {secret_token}")}],
            })))
            .mount(&server)
            .await;
        let client = Client::new(&Session {
            url: server.uri().parse().unwrap(),
            token: SecretString::from(secret_token),
        })
        .unwrap();
        let err = client.latest_mcp_connect(chat).await.unwrap_err();
        let rendered = format!("{err} {err:?}");
        assert!(rendered.contains("[redacted]"), "{rendered}");
        assert!(!rendered.contains(secret_token), "{rendered}");
    }
}
