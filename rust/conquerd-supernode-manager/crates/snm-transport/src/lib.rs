mod auth_prompt;
mod client;
mod embedded;
mod known_hosts;
mod openssh;
mod target;
mod traits;

pub use auth_prompt::{
    clear_session_password, env_key_suffix, env_name_for_host, password_from_env,
    per_host_user_from_env, SSH_PASSWORD_ENV, SSH_USER_ENV,
};
pub use client::*;
pub use embedded::EmbeddedTransport;
pub use openssh::OpenSshTransport;
pub use openssh::RemoteOutput;
pub use target::SshTarget;
pub use traits::*;

pub use openssh::shell_escape;
