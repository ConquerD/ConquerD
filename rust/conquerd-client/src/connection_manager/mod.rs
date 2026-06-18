//! Connection manager — signaling, QUIC transport, and invite handshake.
//!
//! Architecture:
//! - One [`ConnectionManager`] per session, owned by the application.
//! - An async `tokio::task` drives the WebSocket signaling loop.
//! - A `quinn::Endpoint` handles peer-to-peer QUIC connections.
//! - `mpsc` channels carry inbound events to the application layer.

mod events;
mod internal;
mod manager;
mod quic;
mod ws;

#[cfg(test)]
mod tests;

pub use events::{ConnectionCommand, ConnectionEvent};
pub use manager::ConnectionManager;
