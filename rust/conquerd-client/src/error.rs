//! Unified error type for conquerd-client.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Crypto error: {0}")]
    Crypto(String),

    #[error("Identity error: {0}")]
    Identity(String),

    #[error("Store error: {0}")]
    Store(String),

    #[error("Database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("WebSocket error: {0}")]
    WebSocket(String),

    #[error("QUIC error: {0}")]
    Quic(String),

    #[error("Protocol error: {0}")]
    Protocol(String),

    #[error("Feature error: {0}")]
    Feature(String),
}

pub type Result<T> = std::result::Result<T, ClientError>;
