pub fn extract_conquerd_url(text: &str) -> Option<String> {
    let start = text.find("conquerd://")?;
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
    }
}
