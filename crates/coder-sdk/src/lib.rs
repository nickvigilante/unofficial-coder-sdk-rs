//! Unofficial hand-written layer over the generated Coder API client.

mod client;
mod enums;
mod error;
pub mod session;
mod stream;

pub use client::{Client, Session};
pub use coder_api_gen::types;
pub use enums::{ChatStatus, PartType, StreamEventType};
pub use error::{Error, Result, Validation};
pub use session::discover_session;
pub use stream::{StreamEvent, WatchEvent};
