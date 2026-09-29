//! Unofficial hand-written layer over the generated Coder API client.

mod client;
mod error;

pub use client::{Client, Session};
pub use coder_api_gen::types;
pub use error::{Error, Result, Validation};
