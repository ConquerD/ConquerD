use snm_transport::shell_escape;

/// Identity of the supernode binary currently pointed at by `bin/current`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinaryIdentity {
    pub path: Option<String>,
    pub sha256_short: Option<String>,
    pub modified: Option<String>,
    pub build_id: Option<String>,
}

impl BinaryIdentity {
    pub fn missing() -> Self {
        Self {
            path: None,
            sha256_short: None,
            modified: None,
            build_id: None,
        }
    }
}

/// Remote shell snippet: prints up to four lines (sha12, mtime, build_id, path).
pub fn binary_probe_command(current_link: &str) -> String {
    let link = shell_escape(current_link);
    format!(
        r#"bin=$(readlink -f {link} 2>/dev/null || true); \
if [ -n "$bin" ] && [ -f "$bin" ]; then \
  sha256sum "$bin" | awk '{{print substr($1,1,12)}}'; \
  stat -c '%y' "$bin" 2>/dev/null | cut -d. -f1; \
  strings "$bin" 2>/dev/null | grep -E '^release-.{{3,80}}$|^[0-9a-f]{{12}}(-dirty)?$' | head -1; \
  printf '%s\n' "$bin"; \
else \
  echo '-'; \
fi"#
    )
}

fn non_empty(line: Option<&str>) -> Option<String> {
    line.filter(|s| !s.is_empty()).map(str::to_string)
}

pub fn parse_binary_probe_output(stdout: &str) -> BinaryIdentity {
    let lines: Vec<&str> = stdout.lines().map(str::trim).collect();

    if lines.is_empty() || lines[0] == "-" {
        return BinaryIdentity::missing();
    }

    let sha256_short = non_empty(lines.first().copied());
    let modified = non_empty(lines.get(1).copied());
    let build_id = non_empty(lines.get(2).copied());
    let path = non_empty(lines.get(3).copied()).or_else(|| {
        // When build_id is blank the remote prints an empty third line; path may sit at index 2.
        lines
            .get(2)
            .filter(|s| !s.is_empty() && s.starts_with('/'))
            .map(|s| (*s).to_string())
    });

    BinaryIdentity {
        path,
        sha256_short,
        modified,
        build_id,
    }
}

/// Fleet/TUI label: `{pin}@{sha12}` with optional `·MM-DD` from binary mtime.
pub fn format_pinned_version_display(
    pinned_version: &str,
    sha256_short: Option<&str>,
    modified: Option<&str>,
) -> String {
    let Some(sha) = sha256_short.filter(|s| !s.is_empty()) else {
        return pinned_version.to_string();
    };

    let mut label = format!("{pinned_version}@{sha}");
    if let Some(mtime) = modified {
        // `2026-06-14 21:31:06` → `06-14`
        if mtime.len() >= 10 {
            label.push('·');
            label.push_str(&mtime[5..10]);
        }
    }
    label
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_probe_output() {
        let out = "878696fcec9e\n2026-06-14 21:31:06\nabc123def456\n/opt/conquerd/bin/conquerd-supernode-nightly\n";
        let id = parse_binary_probe_output(out);
        assert_eq!(id.sha256_short.as_deref(), Some("878696fcec9e"));
        assert_eq!(id.modified.as_deref(), Some("2026-06-14 21:31:06"));
        assert_eq!(id.build_id.as_deref(), Some("abc123def456"));
        assert_eq!(
            id.path.as_deref(),
            Some("/opt/conquerd/bin/conquerd-supernode-nightly")
        );
    }

    #[test]
    fn parses_probe_without_build_id() {
        let out = "deadbeefcafe\n2026-06-14 03:02:39\n\n/opt/conquerd/bin/current\n";
        let id = parse_binary_probe_output(out);
        assert_eq!(id.build_id, None);
        assert_eq!(id.sha256_short.as_deref(), Some("deadbeefcafe"));
    }

    #[test]
    fn missing_binary_yields_empty_identity() {
        let id = parse_binary_probe_output("-\n");
        assert_eq!(id, BinaryIdentity::missing());
    }

    #[test]
    fn display_includes_pin_sha_and_date() {
        let label = format_pinned_version_display(
            "nightly",
            Some("878696fcec9e"),
            Some("2026-06-14 21:31:06"),
        );
        assert_eq!(label, "nightly@878696fcec9e·06-14");
    }

    #[test]
    fn display_falls_back_to_pin_only() {
        let label = format_pinned_version_display("nightly", None, None);
        assert_eq!(label, "nightly");
    }
}
