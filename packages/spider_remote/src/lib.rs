//! SpiderClient remote monitoring.
//!
//! The desktop client, agent, and trusted backend are deliberately separate:
//!
//! * The launcher never holds the Discord bot token.
//! * The agent has a per-device bearer credential and a narrow action allowlist.
//! * The backend verifies Discord interactions and authorizes every command
//!   against the device owner.
//!
//! See `docs/REMOTE.md` for deployment and the threat model.

pub const AGENT_VERSION: &str = env!("CARGO_PKG_VERSION");

pub mod agent;
pub mod backend;
pub mod bridge;
pub mod command_policy;
pub mod config;
pub mod discord;
pub mod protocol;
pub mod redact;
pub mod screenshot;
pub mod security;
pub mod store;

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("configuration error: {0}")]
    Config(String),
    #[error("pairing failed: {0}")]
    Pairing(String),
    #[error("authentication failed: {0}")]
    Auth(String),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("another agent is already running: {0}")]
    AlreadyRunning(String),
    #[error("backend returned HTTP {status}: {message}")]
    HttpStatus { status: u16, message: String },
    #[error("network error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

impl From<String> for AgentError {
    fn from(value: String) -> Self {
        Self::Config(value)
    }
}
