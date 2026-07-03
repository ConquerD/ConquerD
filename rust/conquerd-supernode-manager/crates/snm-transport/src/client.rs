use std::env;
use std::path::Path;

use async_trait::async_trait;

use crate::embedded::{upload_local_file_embedded, EmbeddedTransport};
use crate::openssh::RemoteOutput;
use crate::openssh::{upload_local_file as upload_local_file_openssh, OpenSshTransport};
use crate::traits::{Transport, TransportError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SshBackend {
    #[default]
    Embedded,
    OpenSsh,
}

impl SshBackend {
    pub fn from_env() -> Self {
        match env::var("SNM_SSH_BACKEND")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "openssh" | "system" => Self::OpenSsh,
            _ => Self::Embedded,
        }
    }

    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.to_ascii_lowercase().as_str() {
            "embedded" | "russh" => Ok(Self::Embedded),
            "openssh" | "system" => Ok(Self::OpenSsh),
            other => Err(format!(
                "unknown ssh backend '{other}' (expected embedded or openssh)"
            )),
        }
    }
}

#[derive(Debug, Clone)]
pub enum SshTransport {
    Embedded(EmbeddedTransport),
    OpenSsh(OpenSshTransport),
}

impl SshTransport {
    pub fn new(raw_target: impl Into<String>, backend: SshBackend) -> Self {
        let target = raw_target.into();
        match backend {
            SshBackend::Embedded => Self::Embedded(EmbeddedTransport::new(target)),
            SshBackend::OpenSsh => Self::OpenSsh(OpenSshTransport::new(target)),
        }
    }

    pub fn from_env(raw_target: impl Into<String>) -> Self {
        Self::new(raw_target, SshBackend::from_env())
    }

    pub fn backend(&self) -> SshBackend {
        match self {
            Self::Embedded(_) => SshBackend::Embedded,
            Self::OpenSsh(_) => SshBackend::OpenSsh,
        }
    }
}

#[async_trait]
impl Transport for SshTransport {
    async fn run(&self, command: &str) -> Result<RemoteOutput, TransportError> {
        match self {
            Self::Embedded(t) => t.run(command).await,
            Self::OpenSsh(t) => t.run(command).await,
        }
    }

    async fn upload_bytes(
        &self,
        remote_path: &str,
        contents: &[u8],
        mode: u32,
    ) -> Result<(), TransportError> {
        match self {
            Self::Embedded(t) => t.upload_bytes(remote_path, contents, mode).await,
            Self::OpenSsh(t) => t.upload_bytes(remote_path, contents, mode).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_backend_names() {
        assert_eq!(SshBackend::parse("embedded").unwrap(), SshBackend::Embedded);
        assert_eq!(SshBackend::parse("openssh").unwrap(), SshBackend::OpenSsh);
        assert!(SshBackend::parse("invalid").is_err());
    }
}

pub async fn upload_local_file(
    transport: &SshTransport,
    local_path: &Path,
    remote_path: &str,
    mode: u32,
) -> Result<(), TransportError> {
    match transport {
        SshTransport::Embedded(t) => {
            upload_local_file_embedded(t, local_path, remote_path, mode).await
        }
        SshTransport::OpenSsh(t) => {
            upload_local_file_openssh(t, local_path, remote_path, mode).await
        }
    }
}
