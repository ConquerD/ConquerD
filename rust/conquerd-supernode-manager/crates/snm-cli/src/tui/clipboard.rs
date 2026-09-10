pub fn extract_conquerd_url(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let start = lower.find("conquerd://").or_else(|| {
        let mut i = 0;
        while let Some(rel) = lower[i..].find("d://") {
            let at = i + rel;
            if at == 0 || !lower.as_bytes()[at - 1].is_ascii_alphanumeric() {
                return Some(at);
            }
            i = at + 1;
        }
        None
    })?;
    let rest = &text[start..];
    let end = rest
        .find(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == '\n' || c == '\r')
        .unwrap_or(rest.len());
    Some(rest[..end].to_string())
}

pub fn copy_target_from_logs(text: &str) -> String {
    extract_conquerd_url(text).unwrap_or_else(|| text.to_string())
}

pub fn copy_to_clipboard(text: &str) -> Result<(), String> {
    arboard::Clipboard::new()
        .map_err(|e| format!("clipboard: {e}"))?
        .set_text(text)
        .map_err(|e| format!("clipboard: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_invite_url_from_logs_text() {
        let text = "source: /var/lib/conquerd/a/reusable_invite.json\n\nconquerd://abc123\n";
        assert_eq!(
            extract_conquerd_url(text).as_deref(),
            Some("conquerd://abc123")
        );
        let minted = "invite ready: d://invite#abc trailing";
        assert_eq!(
            extract_conquerd_url(minted).as_deref(),
            Some("d://invite#abc")
        );
    }
}
