//! Downloading a chat file, which the generated client cannot do: the server declares no
//! body for `GET /api/v2/chats/files/{file}` (`@Success 200`, `coderd/exp_chats.go`,
//! `chatFileByID`), so the generated `get_chat_file` returns `ResponseValue<()>` and drops the
//! bytes. The download here sends the session token in its header, as every request does, and
//! never uses the signed-URL endpoints, which put a token in the URL.

use std::time::Duration;

use reqwest::header::{CONTENT_DISPOSITION, CONTENT_TYPE, HeaderName};

use crate::stream::{MAX_REFUSAL_BODY, capped_body};
use crate::{Client, Error, Result};

/// How long a download may take, from connecting to its last byte. The shared client's 60
/// second total covers the body too, which a 10 MiB file on a link slower than about
/// 170 KiB/s would outlast, so a download sets its own.
pub(crate) const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// A chat file on its way, read a chunk at a time so the caller can write each piece out
/// rather than hold the whole file.
#[derive(Debug)]
pub struct ChatFileDownload {
    /// The stored media type, from `Content-Type`.
    pub media_type: Option<String>,
    /// The stored name, from `Content-Disposition`: the decoded RFC 5987 `filename*` form when
    /// present, else `filename`. It is data only. A person or a model chose it, so it may hold
    /// `/`, `..`, or control characters; callers must sanitize it before using it as a path.
    pub file_name: Option<String>,
    /// The body's length, from `Content-Length`, when the server sent one. It is only a hint:
    /// a chunked body has none, and a server can send more or fewer bytes than it declared.
    /// The SDK sets no cap on the body, so a caller that must bound a download counts the
    /// bytes `chunk` returns and stops reading at its own limit.
    pub size: Option<u64>,
    response: reqwest::Response,
}

impl ChatFileDownload {
    /// The next piece of the body, or `None` at its end. A read that outlasts the download's
    /// timeout fails with an error whose `Error::is_timeout` is true.
    pub async fn chunk(&mut self) -> Result<Option<Vec<u8>>> {
        Ok(self.response.chunk().await?.map(|bytes| bytes.to_vec()))
    }
}

/// The value of header `name` in `response`, when it is text.
fn header(response: &reqwest::Response, name: HeaderName) -> Option<String> {
    response
        .headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

/// The start of parameter `key` (such as `filename=`) in a `Content-Disposition` value, at
/// the beginning or after a `;` or whitespace. Lowercasing changes only ASCII letters, so
/// byte offsets stay the same in `value`.
fn param_start(value: &str, key: &str) -> Option<usize> {
    let lower = value.to_ascii_lowercase();
    lower
        .match_indices(key)
        .map(|(i, _)| i)
        .find(|&i| i == 0 || matches!(lower.as_bytes()[i - 1], b';' | b' ' | b'\t'))
        .map(|i| i + key.len())
}

/// The `filename*` parameter (RFC 5987, `charset'language'percent-encoded`), decoded. Only
/// UTF-8 is known. `None` when it is missing, empty, in another charset, or malformed, so
/// the caller can fall back to the plain `filename`.
fn extended_name(value: &str) -> Option<String> {
    let rest = &value[param_start(value, "filename*=")?..];
    let token = rest.split(';').next().unwrap_or_default().trim();
    let mut parts = token.splitn(3, '\'');
    let (charset, _language, encoded) = (parts.next()?, parts.next()?, parts.next()?);
    if !charset.eq_ignore_ascii_case("utf-8") || encoded.contains(['"', ' ', '\t']) {
        return None;
    }
    let mut bytes = Vec::with_capacity(encoded.len());
    let mut raw = encoded.bytes();
    while let Some(b) = raw.next() {
        if b == b'%' {
            let hex = [raw.next()?, raw.next()?];
            let hex = std::str::from_utf8(&hex).ok()?;
            bytes.push(u8::from_str_radix(hex, 16).ok()?);
        } else {
            bytes.push(b);
        }
    }
    let name = String::from_utf8(bytes).ok()?;
    (!name.is_empty()).then_some(name)
}

/// The plain `filename` parameter, quoted (with `\` escapes, as Go's
/// `mime.FormatMediaType` writes it) or bare.
fn plain_name(value: &str) -> Option<String> {
    let rest = &value[param_start(value, "filename=")?..];
    let name = match rest.strip_prefix('"') {
        Some(quoted) => {
            let mut name = String::new();
            let mut chars = quoted.chars();
            loop {
                match chars.next()? {
                    '"' => break,
                    '\\' => name.push(chars.next()?),
                    c => name.push(c),
                }
            }
            name
        }
        None => rest.split(';').next().unwrap_or_default().trim().to_owned(),
    };
    (!name.is_empty()).then_some(name)
}

/// The file name in a `Content-Disposition` value. The RFC 5987 `filename*` form wins over
/// `filename` when both are present (RFC 6266); a missing, empty, cut-off, or malformed
/// value falls through to the next, and then to `None`. It never fails.
pub(crate) fn disposition_name(value: &str) -> Option<String> {
    extended_name(value).or_else(|| plain_name(value))
}

impl Client {
    /// The request `download_chat_file` sends: a `GET` with its own `DOWNLOAD_TIMEOUT`, which
    /// overrides the shared client's total for this request only.
    fn download_request(&self, file: uuid::Uuid, timeout: Duration) -> Result<reqwest::Request> {
        let url = self
            .base_url()
            .join(&format!("/api/v2/chats/files/{file}"))
            .map_err(|e| Error::Transport(e.to_string()))?;
        Ok(self.http().get(url).timeout(timeout).build()?)
    }

    /// Starts downloading chat file `file`. The session token goes only in the request's
    /// header, which the shared client adds. A refusal's text has the token redacted, as every
    /// hand-built request's does, and its body is read only up to a small cap.
    pub async fn download_chat_file(&self, file: uuid::Uuid) -> Result<ChatFileDownload> {
        self.download_chat_file_within(file, DOWNLOAD_TIMEOUT).await
    }

    pub(crate) async fn download_chat_file_within(
        &self,
        file: uuid::Uuid,
        timeout: Duration,
    ) -> Result<ChatFileDownload> {
        let request = self.download_request(file, timeout)?;
        let response = self.http().execute(request).await?;
        let status = response.status().as_u16();
        if status != 200 {
            let body = capped_body(response, MAX_REFUSAL_BODY).await;
            return Err(self.error_from_status(status, &body));
        }
        let media_type = header(&response, CONTENT_TYPE);
        let file_name = header(&response, CONTENT_DISPOSITION)
            .as_deref()
            .and_then(disposition_name);
        let size = response.content_length();
        Ok(ChatFileDownload {
            media_type,
            file_name,
            size,
            response,
        })
    }
}

#[cfg(test)]
mod tests {
    use secrecy::SecretString;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::{DOWNLOAD_TIMEOUT, disposition_name};
    use crate::{Client, Error, Session};

    const TOKEN: &str = "s3cr3t-session-token-do-not-leak";

    fn client(url: &str) -> Client {
        Client::new(&Session {
            url: url.parse().unwrap(),
            token: SecretString::from(TOKEN),
        })
        .unwrap()
    }

    #[tokio::test]
    async fn a_file_downloads_in_chunks_with_the_token_only_in_a_header() {
        let server = MockServer::start().await;
        let file = uuid::Uuid::new_v4();
        let body: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        Mock::given(method("GET"))
            .and(path(format!("/api/v2/chats/files/{file}")))
            .and(header("Coder-Session-Token", TOKEN))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(body.clone())
                    .insert_header("content-type", "application/zip")
                    .insert_header(
                        "content-disposition",
                        "attachment; filename=\"build; logs.zip\"",
                    ),
            )
            .expect(1)
            .mount(&server)
            .await;
        let mut download = client(&server.uri())
            .download_chat_file(file)
            .await
            .unwrap();
        assert_eq!(download.media_type.as_deref(), Some("application/zip"));
        assert_eq!(download.file_name.as_deref(), Some("build; logs.zip"));
        assert_eq!(download.size, Some(body.len() as u64));
        let mut got = Vec::new();
        while let Some(chunk) = download.chunk().await.unwrap() {
            got.extend_from_slice(&chunk);
        }
        assert_eq!(got, body);
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests[0].url.query(), None, "nothing rides in the URL");
        assert!(!requests[0].url.as_str().contains(TOKEN));
    }

    #[tokio::test]
    async fn a_non_ascii_name_round_trips_through_a_download() {
        let server = MockServer::start().await;
        let file = uuid::Uuid::new_v4();
        Mock::given(path(format!("/api/v2/chats/files/{file}")))
            .respond_with(ResponseTemplate::new(200).set_body_bytes("x").insert_header(
                "content-disposition",
                "attachment; filename=\"r_sum_.pdf\"; filename*=UTF-8''r%C3%A9sum%C3%A9.pdf",
            ))
            .mount(&server)
            .await;
        let download = client(&server.uri())
            .download_chat_file(file)
            .await
            .unwrap();
        assert_eq!(download.file_name.as_deref(), Some("résumé.pdf"));
    }

    #[tokio::test]
    async fn a_missing_file_is_a_404_api_error() {
        let server = MockServer::start().await;
        let file = uuid::Uuid::new_v4();
        Mock::given(path(format!("/api/v2/chats/files/{file}")))
            .respond_with(
                ResponseTemplate::new(404)
                    .set_body_json(serde_json::json!({"message": "Resource not found."})),
            )
            .mount(&server)
            .await;
        match client(&server.uri()).download_chat_file(file).await {
            Err(Error::Api {
                status, message, ..
            }) => {
                assert_eq!(status, 404);
                assert_eq!(message, "Resource not found.");
            }
            other => panic!("expected a 404, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_refusal_redacts_the_token_and_a_rejected_token_is_unauthorized() {
        let server = MockServer::start().await;
        let (refused, rejected) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        Mock::given(path(format!("/api/v2/chats/files/{refused}")))
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "message": format!("bad token {TOKEN}"),
                "detail": format!("echoed {TOKEN}"),
            })))
            .mount(&server)
            .await;
        Mock::given(path(format!("/api/v2/chats/files/{rejected}")))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let c = client(&server.uri());
        let err = c.download_chat_file(refused).await.unwrap_err();
        let rendered = format!("{err} {err:?}");
        assert!(rendered.contains("[redacted]"), "{rendered}");
        assert!(!rendered.contains(TOKEN), "{rendered}");
        assert!(matches!(
            c.download_chat_file(rejected).await,
            Err(Error::Unauthorized)
        ));
    }

    #[test]
    fn a_download_has_its_own_five_minute_timeout() {
        let request = client("http://127.0.0.1:1")
            .download_request(uuid::Uuid::nil(), DOWNLOAD_TIMEOUT)
            .unwrap();
        assert_eq!(DOWNLOAD_TIMEOUT, std::time::Duration::from_secs(300));
        assert_eq!(
            request.timeout(),
            Some(&DOWNLOAD_TIMEOUT),
            "the shared 60 second total would cut off a 10 MiB file on a slow link"
        );
    }

    #[test]
    fn the_disposition_name_is_read_quoted_or_bare() {
        assert_eq!(
            disposition_name("inline; filename=\"a \\\"b\\\" c.png\"").as_deref(),
            Some("a \"b\" c.png")
        );
        assert_eq!(
            disposition_name("attachment; FILENAME=notes.txt; size=3").as_deref(),
            Some("notes.txt")
        );
        assert_eq!(disposition_name("inline"), None);
        assert_eq!(disposition_name("inline; filename=\"\""), None);
        assert_eq!(disposition_name("inline; filename=\"cut"), None);
    }

    #[test]
    fn the_extended_filename_is_decoded_and_preferred() {
        assert_eq!(
            disposition_name("attachment; filename*=UTF-8''r%C3%A9sum%C3%A9.pdf").as_deref(),
            Some("résumé.pdf")
        );
        assert_eq!(
            disposition_name("attachment; filename*=utf-8'en'caf%C3%A9.txt").as_deref(),
            Some("café.txt"),
            "the charset is case-insensitive and a language is skipped"
        );
        assert_eq!(
            disposition_name(
                "attachment; filename=\"resume.pdf\"; filename*=UTF-8''r%C3%A9sum%C3%A9.pdf"
            )
            .as_deref(),
            Some("résumé.pdf"),
            "filename* wins whatever its position"
        );
        assert_eq!(
            disposition_name(
                "attachment; filename*=UTF-8''r%C3%A9sum%C3%A9.pdf; filename=\"resume.pdf\""
            )
            .as_deref(),
            Some("résumé.pdf")
        );
        assert_eq!(
            disposition_name("attachment; filename*=UTF-8''a%20b%3Bc.txt").as_deref(),
            Some("a b;c.txt")
        );
    }

    #[test]
    fn a_malformed_extended_filename_falls_back_to_the_plain_one_then_none() {
        for bad in [
            "filename*=UTF-8''%ff%fe.txt",
            "filename*=UTF-8''bad%2.txt",
            "filename*=UTF-8''bad%zz.txt",
            "filename*=UTF-8'no-second-quote",
            "filename*=KLINGON''abc.txt",
            "filename*=ISO-8859-1''caf%E9.txt",
            "filename*=UTF-8''",
            "filename*=",
        ] {
            assert_eq!(
                disposition_name(&format!("attachment; {bad}; filename=\"plain.txt\"")).as_deref(),
                Some("plain.txt"),
                "{bad}"
            );
            assert_eq!(
                disposition_name(&format!("attachment; {bad}")),
                None,
                "{bad}"
            );
        }
    }

    /// Serves one response on a fresh local port with `serve` driving the socket after the
    /// request has been read, for the cases wiremock cannot script: a body that arrives in
    /// timed pieces, or stalls.
    async fn raw_server<F, Fut>(serve: F) -> String
    where
        F: FnOnce(tokio::net::TcpStream) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        use tokio::io::AsyncReadExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut seen = Vec::new();
            let mut buf = [0u8; 1024];
            while !seen.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = socket.read(&mut buf).await.unwrap();
                if n == 0 {
                    return;
                }
                seen.extend_from_slice(&buf[..n]);
            }
            serve(socket).await;
        });
        url
    }

    #[tokio::test]
    async fn a_body_arrives_in_the_pieces_the_server_flushed() {
        use tokio::io::AsyncWriteExt;
        let pieces: [&[u8]; 3] = [b"first-piece|", b"second-piece|", b"third-piece"];
        let url = raw_server(move |mut socket| async move {
            socket
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ntransfer-encoding: chunked\r\n\r\n")
                .await
                .unwrap();
            for piece in pieces {
                socket
                    .write_all(format!("{:x}\r\n", piece.len()).as_bytes())
                    .await
                    .unwrap();
                socket.write_all(piece).await.unwrap();
                socket.write_all(b"\r\n").await.unwrap();
                socket.flush().await.unwrap();
                tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            }
            socket.write_all(b"0\r\n\r\n").await.unwrap();
        })
        .await;
        let mut download = client(&url)
            .download_chat_file(uuid::Uuid::new_v4())
            .await
            .unwrap();
        assert_eq!(download.size, None, "a chunked body has no Content-Length");
        let mut chunks = Vec::new();
        while let Some(chunk) = download.chunk().await.unwrap() {
            chunks.push(chunk);
        }
        let expected: Vec<Vec<u8>> = pieces.iter().map(|p| p.to_vec()).collect();
        assert_eq!(
            chunks, expected,
            "buffering the body would return it as one chunk"
        );
    }

    #[tokio::test]
    async fn a_body_read_past_the_timeout_is_a_timeout_error() {
        use tokio::io::AsyncWriteExt;
        let url = raw_server(|mut socket| async move {
            socket
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 100\r\n\r\npartial")
                .await
                .unwrap();
            socket.flush().await.unwrap();
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        })
        .await;
        let mut download = client(&url)
            .download_chat_file_within(uuid::Uuid::new_v4(), std::time::Duration::from_millis(300))
            .await
            .unwrap();
        let err = loop {
            match download.chunk().await {
                Ok(Some(_)) => {}
                Ok(None) => panic!("the body ended instead of timing out"),
                Err(e) => break e,
            }
        };
        assert!(err.is_timeout(), "{err:?}");
        assert!(!format!("{err} {err:?}").contains(TOKEN));
    }

    #[tokio::test]
    async fn a_dropped_connection_is_a_transport_error_and_not_a_timeout() {
        use tokio::io::AsyncWriteExt;
        let url = raw_server(|mut socket| async move {
            socket
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 100\r\n\r\npartial")
                .await
                .unwrap();
            socket.flush().await.unwrap();
        })
        .await;
        let mut download = client(&url)
            .download_chat_file(uuid::Uuid::new_v4())
            .await
            .unwrap();
        let err = loop {
            match download.chunk().await {
                Ok(Some(_)) => {}
                Ok(None) => panic!("the body ended instead of failing"),
                Err(e) => break e,
            }
        };
        assert!(matches!(err, Error::Transport(_)), "{err:?}");
        assert!(!err.is_timeout(), "{err:?}");
    }
}
