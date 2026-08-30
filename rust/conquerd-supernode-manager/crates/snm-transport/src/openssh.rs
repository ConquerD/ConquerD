use std::path::Path;
use std::process::Stdio;

use async_trait::async_trait;
use tokio::process::Command;

use crate::auth_prompt::per_host_user_from_env;
use crate::target::SshTarget;
use crate::traits::{Transport, TransportError};

/// Non-interactive SSH defaults. `accept-new` adds unseen host keys to
/// `known_hosts` without prompting; changed keys are still rejected.
const SSH_OPTIONS: &[&str] = &[
    "-o",
    "BatchMode=yes",
    "-o",
    "StrictHostKeyChecking=accept-new",
    "-o",
    "ConnectTimeout=15",
];

#[derive(Debug, Clone)]
pub struct RemoteOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

#[derive(Debug, Clone)]
pub struct OpenSshTransport {
    target: String,
}

impl OpenSshTransport {
    pub fn new(target: impl Into<String>) -> Self {
        Self::new_for_host(target, None)
    }

    /// `label` is the inventory host name. Only `SNM_SSH_USER_<HOST>` applies
    /// here — passwords are the system `ssh` client's business, so per-host
    /// passwords in the secrets file are ignored on this backend.
    pub fn new_for_host(target: impl Into<String>, label: Option<&str>) -> Self {
        let raw = target.into();
        // Rewrite only when an override exists, so the default path keeps
        // handing `ssh` the inventory string verbatim (~/.ssh/config still wins).
        let target = match per_host_user_from_env(label) {
            Some(_) => SshTarget::parse_for_host(&raw, label).display(),
            None => raw,
        };
        Self { target }
    }

    pub fn target(&self) -> &str {
        &self.target
    }

    async fn ssh_output(&self, args: &[&str]) -> Result<RemoteOutput, TransportError> {
        let output = Command::new("ssh")
            .args(SSH_OPTIONS)
            .arg(&self.target)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| TransportError::Other(format!("spawn ssh: {e}")))?;

        Ok(RemoteOutput {
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            exit_code: output.status.code().unwrap_or(-1),
        })
    }
}

#[async_trait]
impl Transport for OpenSshTransport {
    async fn run(&self, command: &str) -> Result<RemoteOutput, TransportError> {
        self.ssh_output(&[command]).await
    }

    async fn upload_bytes(
        &self,
        remote_path: &str,
        contents: &[u8],
        mode: u32,
    ) -> Result<(), TransportError> {
        let tmp = tempfile::NamedTempFile::new()
            .map_err(|e| TransportError::Other(format!("temp file: {e}")))?;
        std::fs::write(tmp.path(), contents)
            .map_err(|e| TransportError::Other(format!("write temp file: {e}")))?;
        upload_local_file(self, tmp.path(), remote_path, mode).await
    }
}

pub async fn upload_local_file(
    transport: &OpenSshTransport,
    local_path: &Path,
    remote_path: &str,
    mode: u32,
) -> Result<(), TransportError> {
    let parent = remote_path.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
    if !parent.is_empty() {
        let mkdir = format!("mkdir -p {}", shell_escape(parent));
        let out = transport.run(&mkdir).await?;
        if out.exit_code != 0 {
            return Err(TransportError::Other(format!(
                "mkdir {parent} failed: {}",
                out.stderr.trim()
            )));
        }
    }

    let dest = format!("{}:{}", transport.target(), remote_path);
    let output = Command::new("scp")
        .args(SSH_OPTIONS)
        .arg("-q")
        .arg(local_path)
        .arg(&dest)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| TransportError::Other(format!("spawn scp: {e}")))?;

    if !output.status.success() {
        return Err(TransportError::Other(format!(
            "scp failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }

    if mode != 0 {
        let chmod = format!("chmod {mode:o} {}", shell_escape(remote_path));
        let out = transport.run(&chmod).await?;
        if out.exit_code != 0 {
            return Err(TransportError::Other(format!(
                "chmod failed: {}",
                out.stderr.trim()
            )));
        }
    }
    Ok(())
}

pub fn shell_escape(s: &str) -> String {
    if s.is_empty() {
        return "''".into();
    }
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '@' | '.' | '_' | '-' | '/'))
    {
        return s.into();
    }
    format!("'{}'", s.replace('\'', "'\"'\"'"))
}

#[cfg(test)]
mod tests {
    use super::shell_escape;

    #[test]
    fn escapes_spaces() {
        assert_eq!(shell_escape("hello world"), "'hello world'");
    }

    #[test]
    fn passes_through_simple_paths() {
        assert_eq!(
            shell_escape("/opt/conquerd/bin/current"),
            "/opt/conquerd/bin/current"
        );
    }

    #[test]
    fn escapes_single_quotes() {
        assert_eq!(shell_escape("it's"), "'it'\"'\"'s'");
    }

    #[test]
    fn empty_string() {
        assert_eq!(shell_escape(""), "''");
    }
}
