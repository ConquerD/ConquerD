use std::collections::HashMap;
use std::io::{self, IsTerminal, Write};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};

/// Non-interactive SSH password for the embedded backend.
///
/// Per-host overrides use `SNM_SSH_PASSWORD_<HOST>`, where `<HOST>` is the
/// inventory host name run through [`env_key_suffix`]. The bare variable stays
/// the fallback for hosts without their own entry.
pub const SSH_PASSWORD_ENV: &str = "SNM_SSH_PASSWORD";

/// SSH login user. `SNM_SSH_USER_<HOST>` overrides the `user@` in
/// `inventory.toml`; the bare variable only fills in when the host's `ssh`
/// string has no `user@` prefix.
pub const SSH_USER_ENV: &str = "SNM_SSH_USER";

const MASK_CHAR: char = '*';

static SESSION_PASSWORDS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

fn session_store() -> &'static Mutex<HashMap<String, String>> {
    SESSION_PASSWORDS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Env-var suffix for an inventory host name: uppercased, with every character
/// outside `A-Z` / `0-9` replaced by `_` (`edge-1-a` becomes `EDGE_1_A`).
pub fn env_key_suffix(label: &str) -> String {
    label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

fn non_empty_env(key: &str) -> Option<String> {
    match std::env::var(key) {
        Ok(value) if !value.is_empty() => Some(value),
        _ => None,
    }
}

/// Name of the per-host variable for `base`, or `base` itself without a label.
pub fn env_name_for_host(base: &str, label: Option<&str>) -> String {
    match label.map(env_key_suffix).filter(|s| !s.is_empty()) {
        Some(suffix) => format!("{base}_{suffix}"),
        None => base.to_string(),
    }
}

/// `<base>_<HOST>` only — no fallback to the bare variable.
pub fn per_host_env(base: &str, label: Option<&str>) -> Option<String> {
    let label = label?;
    let suffix = env_key_suffix(label);
    if suffix.is_empty() {
        return None;
    }
    non_empty_env(&format!("{base}_{suffix}"))
}

/// `<base>_<HOST>`, then the bare `<base>`.
fn env_for_host(base: &str, label: Option<&str>) -> Option<String> {
    per_host_env(base, label).or_else(|| non_empty_env(base))
}

pub fn password_from_env(label: Option<&str>) -> Option<String> {
    env_for_host(SSH_PASSWORD_ENV, label)
}

/// Per-host login user (`SNM_SSH_USER_<HOST>`), which outranks `inventory.toml`.
pub fn per_host_user_from_env(label: Option<&str>) -> Option<String> {
    per_host_env(SSH_USER_ENV, label)
}

/// Fallback login user (`SNM_SSH_USER`), used only when nothing else names one.
pub fn default_user_from_env() -> Option<String> {
    non_empty_env(SSH_USER_ENV)
}

/// Password from the environment or from a prior interactive connect to the
/// same `cache_key` in this process.
pub fn resolved_password(label: Option<&str>, cache_key: &str) -> Option<String> {
    password_from_env(label).or_else(|| session_store().lock().ok()?.get(cache_key).cloned())
}

pub fn cache_session_password(cache_key: &str, password: impl Into<String>) {
    let password = password.into();
    if password.is_empty() || cache_key.is_empty() {
        return;
    }
    if let Ok(mut store) = session_store().lock() {
        store.insert(cache_key.to_string(), password);
    }
}

/// Drop every cached password. Passwords are never shared between hosts, so
/// this only matters on shutdown.
pub fn clear_session_password() {
    if let Ok(mut store) = session_store().lock() {
        store.clear();
    }
}

pub fn interactive_allowed() -> bool {
    io::stdin().is_terminal()
}

fn format_password_label(prompt: &str) -> String {
    if prompt.is_empty() {
        "Password: ".to_string()
    } else if prompt.ends_with(": ") || prompt.ends_with(':') {
        prompt.to_string()
    } else {
        format!("{prompt}: ")
    }
}

fn password_chars(text: &str) -> impl Iterator<Item = char> + '_ {
    text.chars().filter(|c| *c != '\n' && *c != '\r')
}

fn append_masked_chars(password: &mut String, text: &str) -> Result<(), String> {
    let count = password_chars(text).count();
    if count == 0 {
        return Ok(());
    }
    password.extend(password_chars(text));
    let mask = MASK_CHAR.to_string().repeat(count);
    eprint!("{mask}");
    io::stderr().flush().map_err(|e| e.to_string())?;
    Ok(())
}

fn erase_masked_char() -> Result<(), String> {
    // Backspace + space + backspace clears one displayed mask character.
    eprint!("\x08 \x08");
    io::stderr().flush().map_err(|e| e.to_string())
}

fn read_masked_password(prompt: &str) -> Result<String, String> {
    let label = format_password_label(prompt);
    eprint!("{label}");
    io::stderr().flush().map_err(|e| e.to_string())?;

    enable_raw_mode().map_err(|e| format!("enable raw mode: {e}"))?;
    let result = read_masked_password_raw();
    disable_raw_mode().map_err(|e| format!("disable raw mode: {e}"))?;
    eprintln!();
    result
}

fn read_masked_password_raw() -> Result<String, String> {
    let mut password = String::new();

    loop {
        if !event::poll(Duration::from_millis(100)).map_err(|e| e.to_string())? {
            continue;
        }

        match event::read().map_err(|e| e.to_string())? {
            Event::Key(key) => match handle_password_key(&mut password, key)? {
                PasswordKeyAction::Continue => {}
                PasswordKeyAction::Submit => return Ok(password),
                PasswordKeyAction::Cancel => {
                    return Err("password entry cancelled".into());
                }
            },
            Event::Paste(text) => append_masked_chars(&mut password, &text)?,
            _ => {}
        }
    }
}

enum PasswordKeyAction {
    Continue,
    Submit,
    Cancel,
}

fn handle_password_key(password: &mut String, key: KeyEvent) -> Result<PasswordKeyAction, String> {
    if key.kind != KeyEventKind::Press {
        return Ok(PasswordKeyAction::Continue);
    }

    match key.code {
        KeyCode::Enter => Ok(PasswordKeyAction::Submit),
        KeyCode::Esc => Ok(PasswordKeyAction::Cancel),
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Ok(PasswordKeyAction::Cancel)
        }
        KeyCode::Backspace => {
            if password.pop().is_some() {
                erase_masked_char()?;
            }
            Ok(PasswordKeyAction::Continue)
        }
        KeyCode::Char(c) => {
            password.push(c);
            eprint!("{MASK_CHAR}");
            io::stderr().flush().map_err(|e| e.to_string())?;
            Ok(PasswordKeyAction::Continue)
        }
        _ => Ok(PasswordKeyAction::Continue),
    }
}

/// `cache_key` scopes a typed password to one `user@host:port`; pass `None` to
/// leave it uncached.
pub fn read_prompt_response(
    prompt: &str,
    echo: bool,
    cache_key: Option<&str>,
) -> Result<String, String> {
    if echo {
        let label = if prompt.is_empty() {
            "Response: ".to_string()
        } else if prompt.ends_with(": ") || prompt.ends_with(':') {
            prompt.to_string()
        } else {
            format!("{prompt}: ")
        };
        eprint!("{label}");
        io::stderr().flush().map_err(|e| e.to_string())?;
        let mut line = String::new();
        io::stdin()
            .read_line(&mut line)
            .map_err(|e| format!("read stdin: {e}"))?;
        Ok(line.trim_end_matches(['\r', '\n']).to_string())
    } else {
        let password = read_masked_password(prompt)?;
        if let Some(key) = cache_key {
            cache_session_password(key, &password);
        }
        Ok(password)
    }
}

pub fn read_prompt_responses(
    prompts: &[(String, bool)],
    cache_key: Option<&str>,
) -> Result<Vec<String>, String> {
    prompts
        .iter()
        .map(|(prompt, echo)| read_prompt_response(prompt, *echo, cache_key))
        .collect()
}

pub async fn prompt_for_password(label: Option<&str>, cache_key: &str) -> Result<String, String> {
    if !interactive_allowed() {
        let var = env_name_for_host(SSH_PASSWORD_ENV, label);
        return Err(format!(
            "stdin is not a TTY; set {var} or use Connect from the TUI"
        ));
    }
    eprintln!();
    let key = cache_key.to_string();
    let password =
        tokio::task::spawn_blocking(move || read_prompt_response("Password", false, Some(&key)))
            .await
            .map_err(|e| format!("prompt task: {e}"))??;
    Ok(password)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_from_env_empty_is_none() {
        let key = format!("{SSH_PASSWORD_ENV}_TEST_EMPTY");
        std::env::set_var(&key, "");
        assert!(non_empty_env(&key).is_none());
        std::env::remove_var(&key);
    }

    #[test]
    fn builds_env_suffix_from_host_name() {
        assert_eq!(env_key_suffix("acdc"), "ACDC");
        assert_eq!(env_key_suffix("ac1"), "AC1");
        assert_eq!(env_key_suffix("edge-1.a"), "EDGE_1_A");
        assert_eq!(env_key_suffix(""), "");
    }

    #[test]
    fn names_per_host_env_var() {
        assert_eq!(
            env_name_for_host(SSH_PASSWORD_ENV, Some("acdc")),
            "SNM_SSH_PASSWORD_ACDC"
        );
        assert_eq!(
            env_name_for_host(SSH_PASSWORD_ENV, None),
            "SNM_SSH_PASSWORD"
        );
    }

    #[test]
    fn session_passwords_do_not_leak_across_hosts() {
        // Asserts on the store directly: `resolved_password` would pick up an
        // ambient SNM_SSH_PASSWORD from the runner's environment.
        cache_session_password("root@10.0.0.1:22", "first");
        let store = session_store().lock().unwrap();
        assert_eq!(
            store.get("root@10.0.0.1:22").map(String::as_str),
            Some("first")
        );
        assert!(store.get("root@10.0.0.2:22").is_none());
    }

    #[test]
    fn strips_newlines_from_paste_text() {
        let chars: Vec<char> = password_chars("abc\r\ndef").collect();
        assert_eq!(chars, vec!['a', 'b', 'c', 'd', 'e', 'f']);
    }

    #[test]
    fn formats_password_label() {
        assert_eq!(format_password_label(""), "Password: ");
        assert_eq!(
            format_password_label("root@host's password: "),
            "root@host's password: "
        );
        assert_eq!(format_password_label("Verification"), "Verification: ");
    }
}
