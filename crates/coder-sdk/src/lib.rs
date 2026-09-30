//! Unofficial hand-written layer over the generated Coder API client.

mod client;
mod debug;
mod enums;
mod error;
mod messages;
pub mod session;
mod stream;

pub use client::{Client, Session};
pub use coder_api_gen::types;
pub use debug::McpConnectOutcome;
pub use enums::{ChatStatus, PartType, StreamEventType};
pub use error::{Error, Result, Validation};
pub use session::discover_session;
pub use stream::{STREAM_IDLE_TIMEOUT, StreamEvent, UPGRADE_TIMEOUT, WatchEvent};

/// The coder/coder ref and commit this SDK was generated from, as written by `scripts/regenerate.sh`.
pub const GENERATED_FROM: &str = include_str!("../../../spec/coder-ref.txt");

#[cfg(test)]
mod tests {
    #[test]
    fn generated_from_names_a_coder_ref() {
        let value = super::GENERATED_FROM.trim();
        assert!(value.contains('(') && value.ends_with(')'), "{value}");
    }
}
