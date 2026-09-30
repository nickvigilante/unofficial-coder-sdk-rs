//! Sending a chat message with fields the generated request type cannot express.
//!
//! `CodersdkCreateChatMessageRequest::mcp_server_ids` is a `Vec<uuid::Uuid>` with
//! `skip_serializing_if = "Vec::is_empty"`, so the generated client can only say "leave the
//! selection unchanged" (the field absent) or "replace it with these servers" (a non-empty
//! array): it drops an empty selection instead of sending `[]`. The server's
//! `CreateChatMessageRequest.MCPServerIDs` is a pointer (`codersdk/chats.go:766`): nil means no
//! change, and a non-nil, possibly empty, slice replaces the chat's MCP servers wholesale
//! (`coderd/exp_chats.go:2792-2797`, `normalizeRequestedChatMCPServerIDs`). `send_chat_message`
//! below always writes `mcp_server_ids`, so turning every server off for the next message is
//! expressible.

use crate::{Client, Error, Result};

impl Client {
    /// Sends `body` to `chat` like the generated `send_chat_message`, but always sends
    /// `mcp_server_ids`, so an empty slice turns every MCP server off for the chat instead of
    /// leaving the selection unchanged.
    pub async fn send_chat_message_with_mcp_servers(
        &self,
        chat: uuid::Uuid,
        body: &crate::types::CodersdkCreateChatMessageRequest,
        mcp_server_ids: &[uuid::Uuid],
    ) -> Result<()> {
        let mut value = serde_json::to_value(body).map_err(|e| Error::Decode(e.to_string()))?;
        value["mcp_server_ids"] = serde_json::json!(mcp_server_ids);
        let url = self
            .base_url()
            .join(&format!("/api/v2/chats/{chat}/messages"))
            .map_err(|e| Error::Transport(e.to_string()))?;
        let response = self.http().post(url).json(&value).send().await?;
        let status = response.status().as_u16();
        if status == 200 {
            return Ok(());
        }
        let bytes = response.bytes().await?;
        Err(Error::from_status(status, &bytes))
    }
}

#[cfg(test)]
mod tests {
    use secrecy::SecretString;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::types::{
        CodersdkChatInputPart, CodersdkChatInputPartType, CodersdkCreateChatMessageRequest,
    };
    use crate::{Client, Session};

    fn client(url: &str) -> Client {
        Client::new(&Session {
            url: url.parse().unwrap(),
            token: SecretString::from("test-token-not-real"),
        })
        .unwrap()
    }

    fn text_body(text: &str) -> CodersdkCreateChatMessageRequest {
        CodersdkCreateChatMessageRequest {
            content: vec![CodersdkChatInputPart {
                type_: Some(CodersdkChatInputPartType("text".into())),
                text: Some(text.into()),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    /// Pins the generated client's existing behavior: an untouched `mcp_server_ids` (the "no
    /// change" case) never reaches the wire, because `skip_serializing_if` drops the empty
    /// `Vec` the generated type defaults to.
    #[tokio::test]
    async fn an_untouched_selection_is_not_sent() {
        let server = MockServer::start().await;
        let chat = uuid::Uuid::new_v4();
        Mock::given(method("POST"))
            .and(path(format!("/api/v2/chats/{chat}/messages")))
            .and(body_json(serde_json::json!({
                "content": [{"type": "text", "text": "hi"}]
            })))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"queued": false})),
            )
            .expect(1)
            .mount(&server)
            .await;
        client(&server.uri())
            .api()
            .send_chat_message(&chat, &text_body("hi"))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn an_empty_selection_is_sent_as_an_empty_list() {
        let server = MockServer::start().await;
        let chat = uuid::Uuid::new_v4();
        Mock::given(method("POST"))
            .and(path(format!("/api/v2/chats/{chat}/messages")))
            .and(body_json(serde_json::json!({
                "content": [{"type": "text", "text": "hi"}],
                "mcp_server_ids": []
            })))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"queued": false})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let client = client(&server.uri());
        let body = text_body("hi");
        client
            .send_chat_message_with_mcp_servers(chat, &body, &[])
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_non_empty_selection_is_sent_verbatim() {
        let server = MockServer::start().await;
        let chat = uuid::Uuid::new_v4();
        let (github, linear) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        Mock::given(method("POST"))
            .and(path(format!("/api/v2/chats/{chat}/messages")))
            .and(body_json(serde_json::json!({
                "content": [{"type": "text", "text": "hi"}],
                "mcp_server_ids": [github, linear]
            })))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"queued": false})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let client = client(&server.uri());
        let body = text_body("hi");
        client
            .send_chat_message_with_mcp_servers(chat, &body, &[github, linear])
            .await
            .unwrap();
    }
}
