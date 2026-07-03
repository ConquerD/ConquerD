use std::future::Future;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use home::home_dir;
use russh::client::{self, KeyboardInteractiveAuthResponse, Prompt};
use russh::keys::{load_secret_key, PrivateKeyWithHashAlg, PublicKey};
use russh::ChannelMsg;
use russh_sftp::client::SftpSession;
use russh_sftp::protocol::OpenFlags;
use tokio::io::AsyncWriteExt;

use crate::auth_prompt::{
    cache_session_password, interactive_allowed, prompt_for_password, read_prompt_responses,
    resolved_password, SSH_PASSWORD_ENV,
};
use crate::known_hosts::KnownHosts;
use crate::openssh::shell_escape;
use crate::openssh::RemoteOutput;
use crate::target::SshTarget;
use crate::traits::{Transport, TransportError};

#[derive(Debug)]
enum ClientError {
    Russh(russh::Error),
    Other(String),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Russh(e) => write!(f, "{e}"),
            Self::Other(s) => write!(f, "{s}"),
        }
    }
}

impl std::error::Error for ClientError {}

impl From<russh::Error> for ClientError {
    fn from(value: russh::Error) -> Self {
        Self::Russh(value)
    }
}

#[derive(Debug, Clone)]
pub struct EmbeddedTransport {
    target: SshTarget,
}

impl EmbeddedTransport {
    pub fn new(raw_target: impl Into<String>) -> Self {
        Self {
            target: SshTarget::parse(&raw_target.into()),
        }
    }

    pub fn target(&self) -> &SshTarget {
        &self.target
    }

    async fn with_session<F, Fut, T>(&self, op: F) -> Result<T, TransportError>
    where
        F: FnOnce(client::Handle<ClientHandler>) -> Fut,
        Fut: Future<Output = Result<T, TransportError>>,
    {
        let config = Arc::new(client::Config {
            inactivity_timeout: Some(Duration::from_secs(30)),
            ..<_>::default()
        });
        let known_hosts =
            KnownHosts::load().map_err(|e| TransportError::Other(format!("known_hosts: {e}")))?;
        let handler = ClientHandler {
            host: self.target.host.clone(),
            known_hosts,
        };
        let addr = (self.target.host.as_str(), self.target.port);
        let mut session = client::connect(config, addr, handler)
            .await
            .map_err(|e| TransportError::Other(format!("ssh connect: {e}")))?;

        if !authenticate(&mut session, &self.target.user).await? {
            return Err(TransportError::Other(auth_failure_message()));
        }

        op(session).await
    }
}

struct ClientHandler {
    host: String,
    known_hosts: KnownHosts,
}

impl client::Handler for ClientHandler {
    type Error = ClientError;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKey,
    ) -> Result<bool, Self::Error> {
        self.known_hosts
            .verify_or_accept_new(&self.host, server_public_key)
            .map_err(|e| ClientError::Other(format!("host key verification: {e}")))
    }
}

fn auth_failure_message() -> String {
    if resolved_password().is_some() {
        "ssh authentication failed (keys, password, and keyboard-interactive were rejected)".into()
    } else if interactive_allowed() {
        "ssh authentication failed (password rejected or not accepted by server)".into()
    } else {
        format!(
            "ssh authentication failed (no accepted key in ~/.ssh; set {SSH_PASSWORD_ENV} or run from a terminal for interactive auth)"
        )
    }
}

async fn authenticate(
    session: &mut client::Handle<ClientHandler>,
    user: &str,
) -> Result<bool, TransportError> {
    if try_public_key_auth(session, user).await? {
        return Ok(true);
    }

    let mut password = resolved_password();
    if password.is_none() && interactive_allowed() {
        password = Some(
            prompt_for_password()
                .await
                .map_err(|e| TransportError::Other(e))?,
        );
    }

    if let Some(ref pw) = password {
        if try_password_auth(session, user, pw).await? {
            return Ok(true);
        }
        if try_keyboard_interactive_auth(session, user, Some(pw.clone())).await? {
            return Ok(true);
        }
    } else if interactive_allowed() {
        if try_keyboard_interactive_auth(session, user, None).await? {
            return Ok(true);
        }
    }

    Ok(false)
}

async fn try_public_key_auth(
    session: &mut client::Handle<ClientHandler>,
    user: &str,
) -> Result<bool, TransportError> {
    let rsa_hash = session
        .best_supported_rsa_hash()
        .await
        .map_err(|e| TransportError::Other(format!("rsa hash negotiation: {e}")))?
        .flatten();

    for key_path in default_identity_files() {
        let Ok(key) = load_secret_key(&key_path, None) else {
            continue;
        };
        let auth = session
            .authenticate_publickey(
                user.to_string(),
                PrivateKeyWithHashAlg::new(Arc::new(key), rsa_hash.clone()),
            )
            .await
            .map_err(|e| TransportError::Other(format!("publickey auth: {e}")))?;
        if auth.success() {
            return Ok(true);
        }
    }
    Ok(false)
}

async fn try_password_auth(
    session: &mut client::Handle<ClientHandler>,
    user: &str,
    password: &str,
) -> Result<bool, TransportError> {
    let auth = session
        .authenticate_password(user, password)
        .await
        .map_err(|e| TransportError::Other(format!("password auth: {e}")))?;
    if auth.success() {
        cache_session_password(password);
        return Ok(true);
    }
    Ok(false)
}

async fn try_keyboard_interactive_auth(
    session: &mut client::Handle<ClientHandler>,
    user: &str,
    env_password: Option<String>,
) -> Result<bool, TransportError> {
    let mut response = session
        .authenticate_keyboard_interactive_start(user, None)
        .await
        .map_err(|e| TransportError::Other(format!("keyboard-interactive auth: {e}")))?;

    loop {
        match response {
            KeyboardInteractiveAuthResponse::Success => {
                if let Some(password) = env_password.as_deref() {
                    cache_session_password(password);
                }
                return Ok(true);
            }
            KeyboardInteractiveAuthResponse::Failure { .. } => return Ok(false),
            KeyboardInteractiveAuthResponse::InfoRequest {
                name,
                instructions,
                prompts,
            } => {
                if !instructions.is_empty() {
                    eprintln!("{instructions}");
                } else if !name.is_empty() {
                    eprintln!("{name}");
                }

                let responses =
                    keyboard_interactive_responses(&prompts, env_password.as_deref()).await?;
                response = session
                    .authenticate_keyboard_interactive_respond(responses)
                    .await
                    .map_err(|e| {
                        TransportError::Other(format!("keyboard-interactive respond: {e}"))
                    })?;
            }
        }
    }
}

async fn keyboard_interactive_responses(
    prompts: &[Prompt],
    env_password: Option<&str>,
) -> Result<Vec<String>, TransportError> {
    if let Some(password) = env_password.filter(|_| can_auto_fill_password_prompts(prompts)) {
        return Ok(vec![password.to_string(); prompts.len()]);
    }

    if !interactive_allowed() {
        return Err(TransportError::Other(format!(
            "ssh server requested keyboard-interactive auth but stdin is not a TTY (set {SSH_PASSWORD_ENV})"
        )));
    }

    let prompt_specs: Vec<(String, bool)> =
        prompts.iter().map(|p| (p.prompt.clone(), p.echo)).collect();
    tokio::task::spawn_blocking(move || read_prompt_responses(&prompt_specs))
        .await
        .map_err(|e| TransportError::Other(format!("prompt task: {e}")))?
        .map_err(|e| TransportError::Other(e))
}

fn can_auto_fill_password_prompts(prompts: &[Prompt]) -> bool {
    prompts.len() == 1 && !prompts[0].echo
}

fn default_identity_files() -> Vec<std::path::PathBuf> {
    let Some(home) = home_dir() else {
        return Vec::new();
    };
    let ssh = home.join(".ssh");
    ["id_ed25519", "id_rsa", "id_ecdsa"]
        .into_iter()
        .map(|name| ssh.join(name))
        .filter(|p| p.exists())
        .collect()
}

async fn run_command(
    session: &client::Handle<ClientHandler>,
    command: &str,
) -> Result<RemoteOutput, TransportError> {
    let mut channel = session
        .channel_open_session()
        .await
        .map_err(|e| TransportError::Other(format!("open session: {e}")))?;
    channel
        .exec(true, command)
        .await
        .map_err(|e| TransportError::Other(format!("exec: {e}")))?;

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut exit_code = -1;

    while let Some(msg) = channel.wait().await {
        match msg {
            ChannelMsg::Data { data } => stdout.extend_from_slice(&data),
            ChannelMsg::ExtendedData { ext: 1, data } => stderr.extend_from_slice(&data),
            ChannelMsg::ExitStatus { exit_status } => exit_code = exit_status as i32,
            _ => {}
        }
    }

    Ok(RemoteOutput {
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        exit_code,
    })
}

async fn sftp_upload(
    session: &client::Handle<ClientHandler>,
    remote_path: &str,
    contents: &[u8],
) -> Result<(), TransportError> {
    let channel = session
        .channel_open_session()
        .await
        .map_err(|e| TransportError::Other(format!("open sftp channel: {e}")))?;
    channel
        .request_subsystem(true, "sftp")
        .await
        .map_err(|e| TransportError::Other(format!("sftp subsystem: {e}")))?;

    let sftp = SftpSession::new(channel.into_stream())
        .await
        .map_err(|e| TransportError::Other(format!("sftp session: {e}")))?;

    let mut file = sftp
        .open_with_flags(
            remote_path,
            OpenFlags::CREATE | OpenFlags::TRUNCATE | OpenFlags::WRITE,
        )
        .await
        .map_err(|e| TransportError::Other(format!("sftp open: {e}")))?;
    file.write_all(contents)
        .await
        .map_err(|e| TransportError::Other(format!("sftp write: {e}")))?;
    file.shutdown()
        .await
        .map_err(|e| TransportError::Other(format!("sftp close: {e}")))?;
    Ok(())
}

#[async_trait]
impl Transport for EmbeddedTransport {
    async fn run(&self, command: &str) -> Result<RemoteOutput, TransportError> {
        let command = command.to_string();
        self.with_session(|session| async move { run_command(&session, &command).await })
            .await
    }

    async fn upload_bytes(
        &self,
        remote_path: &str,
        contents: &[u8],
        mode: u32,
    ) -> Result<(), TransportError> {
        let remote_path = remote_path.to_string();
        let contents = contents.to_vec();
        self.with_session(|session| async move {
            if let Some(parent) = remote_path.rsplit_once('/').map(|(p, _)| p) {
                if !parent.is_empty() {
                    let mkdir = format!("mkdir -p {}", shell_escape(parent));
                    let out = run_command(&session, &mkdir).await?;
                    if out.exit_code != 0 {
                        return Err(TransportError::Other(format!(
                            "mkdir {parent} failed: {}",
                            out.stderr.trim()
                        )));
                    }
                }
            }
            sftp_upload(&session, &remote_path, &contents).await?;
            if mode != 0 {
                let chmod = format!("chmod {mode:o} {}", shell_escape(&remote_path));
                let out = run_command(&session, &chmod).await?;
                if out.exit_code != 0 {
                    return Err(TransportError::Other(format!(
                        "chmod failed: {}",
                        out.stderr.trim()
                    )));
                }
            }
            Ok(())
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_fill_only_single_hidden_prompt() {
        assert!(can_auto_fill_password_prompts(&[Prompt {
            prompt: "Password: ".into(),
            echo: false,
        }]));
        assert!(!can_auto_fill_password_prompts(&[Prompt {
            prompt: "Password: ".into(),
            echo: true,
        }]));
        assert!(!can_auto_fill_password_prompts(&[
            Prompt {
                prompt: "Password: ".into(),
                echo: false,
            },
            Prompt {
                prompt: "OTP: ".into(),
                echo: false,
            },
        ]));
    }
}

pub async fn upload_local_file_embedded(
    transport: &EmbeddedTransport,
    local_path: &Path,
    remote_path: &str,
    mode: u32,
) -> Result<(), TransportError> {
    let contents = std::fs::read(local_path)
        .map_err(|e| TransportError::Other(format!("read {}: {e}", local_path.display())))?;
    transport.upload_bytes(remote_path, &contents, mode).await
}
