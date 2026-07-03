mod auth_prompt;
mod client;
mod embedded;
mod known_hosts;
mod openssh;
mod target;
mod traits;

pub use auth_prompt::{clear_session_password, SSH_PASSWORD_ENV};
pub use client::*;
pub use embedded::EmbeddedTransport;
pub use openssh::OpenSshTransport;
pub use openssh::RemoteOutput;
pub use target::SshTarget;
pub use traits::*;

pub use openssh::shell_escape;
