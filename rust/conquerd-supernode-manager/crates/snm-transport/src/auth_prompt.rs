use std::io::{self, IsTerminal, Write};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};

/// Non-interactive SSH password for the embedded backend.
pub const SSH_PASSWORD_ENV: &str = "SNM_SSH_PASSWORD";

const MASK_CHAR: char = '*';

static SESSION_PASSWORD: OnceLock<Mutex<Option<String>>> = OnceLock::new();

fn session_store() -> &'static Mutex<Option<String>> {
    SESSION_PASSWORD.get_or_init(|| Mutex::new(None))
}

pub fn password_from_env() -> Option<String> {
    match std::env::var(SSH_PASSWORD_ENV) {
        Ok(value) if !value.is_empty() => Some(value),
        _ => None,
    }
}

/// Password from `SNM_SSH_PASSWORD` or a prior interactive connect in this process.
pub fn resolved_password() -> Option<String> {
    password_from_env().or_else(|| session_store().lock().ok()?.clone())
}

pub fn cache_session_password(password: impl Into<String>) {
    let password = password.into();
    if password.is_empty() {
        return;
    }
    if let Ok(mut slot) = session_store().lock() {
        *slot = Some(password);
    }
}

pub fn clear_session_password() {
    if let Ok(mut slot) = session_store().lock() {
        *slot = None;
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

pub fn read_prompt_response(prompt: &str, echo: bool) -> Result<String, String> {
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
        cache_session_password(&password);
        Ok(password)
    }
}

pub fn read_prompt_responses(prompts: &[(String, bool)]) -> Result<Vec<String>, String> {
    prompts
        .iter()
        .map(|(prompt, echo)| read_prompt_response(prompt, *echo))
        .collect()
}

pub async fn prompt_for_password() -> Result<String, String> {
    if !interactive_allowed() {
        return Err(format!(
            "stdin is not a TTY; set {SSH_PASSWORD_ENV} or use Connect from the TUI"
        ));
    }
    eprintln!();
    let password = tokio::task::spawn_blocking(|| read_prompt_response("Password", false))
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
        assert!(std::env::var(&key).unwrap().is_empty());
        std::env::remove_var(&key);
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
