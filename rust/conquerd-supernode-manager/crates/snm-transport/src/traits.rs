use async_trait::async_trait;

use crate::openssh::RemoteOutput;

#[async_trait]
pub trait Transport: Send + Sync {
    async fn run(&self, command: &str) -> Result<RemoteOutput, TransportError>;
    async fn upload_bytes(
        &self,
        remote_path: &str,
        contents: &[u8],
        mode: u32,
    ) -> Result<(), TransportError>;
}

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("remote command failed with exit {exit}: {stderr}")]
    CommandFailed {
        exit: i32,
        stderr: String,
        stdout: String,
    },
    #[error("{0}")]
    Other(String),
}
