use serde::Deserialize;

/// One field-level validation message from the server.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Validation {
    pub field: String,
    pub detail: String,
}

#[derive(Debug, Deserialize, Default)]
struct ApiBody {
    #[serde(default)]
    message: String,
    #[serde(default)]
    detail: Option<String>,
    #[serde(default)]
    validations: Vec<Validation>,
}

/// Errors returned by coder-sdk.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("the session token was rejected; run `coder login`")]
    Unauthorized,
    #[error("{message}")]
    Api {
        status: u16,
        message: String,
        detail: Option<String>,
        validations: Vec<Validation>,
    },
    #[error("transport error: {0}")]
    Transport(String),
    #[error("could not decode response: {0}")]
    Decode(String),
    #[error("not logged in: {0}")]
    NotLoggedIn(String),
    #[error("stream closed: {reason}")]
    StreamClosed { code: Option<u16>, reason: String },
    #[error("the session token contains characters that cannot be sent in a header")]
    InvalidToken,
}

/// Convenience alias for coder-sdk results.
pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// Builds an error from an HTTP status and a response body in codersdk.Response shape.
    pub fn from_status(status: u16, body: &[u8]) -> Error {
        if status == 401 {
            return Error::Unauthorized;
        }
        let parsed: ApiBody = serde_json::from_slice(body).unwrap_or_default();
        let message = if parsed.message.is_empty() {
            format!("HTTP {status}")
        } else {
            parsed.message
        };
        Error::Api {
            status,
            message,
            detail: parsed.detail.filter(|d| !d.is_empty()),
            validations: parsed.validations,
        }
    }

    /// Converts a generated-client error, reading the response body when it is still available.
    ///
    /// `E` also needs `std::fmt::Debug` here: progenitor-client 0.15 only implements
    /// `Display`/`Debug` for `Error<E>` when `E: Debug`, which the catch-all arm below
    /// relies on to stringify the remaining variants.
    pub async fn from_progenitor<E: serde::Serialize + std::fmt::Debug>(
        err: progenitor_client::Error<E>,
    ) -> Error {
        use progenitor_client::Error as P;
        match err {
            P::ErrorResponse(value) => {
                let status = value.status().as_u16();
                let body = serde_json::to_vec(&value.into_inner()).unwrap_or_default();
                Error::from_status(status, &body)
            }
            P::UnexpectedResponse(response) => {
                let status = response.status().as_u16();
                let body = response.bytes().await.unwrap_or_default();
                Error::from_status(status, &body)
            }
            P::InvalidResponsePayload(_, e) => Error::Decode(e.to_string()),
            other => Error::Transport(other.to_string()),
        }
    }
}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        if e.status().map(|s| s.as_u16()) == Some(401) {
            return Error::Unauthorized;
        }
        Error::Transport(e.to_string())
    }
}
