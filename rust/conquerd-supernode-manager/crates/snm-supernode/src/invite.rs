use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::Deserialize;
use snm_transport::{shell_escape, SshTransport, Transport};

use crate::layout::InstanceLayout;

#[derive(Debug, Clone)]
pub struct InviteInfo {
    pub label: String,
    pub invite_url: String,
    pub source_path: String,
}

pub fn reusable_invite_path(data_dir: &str) -> String {
    format!("{data_dir}/reusable_invite.json")
}

/// Read `identity.json` from the remote data directory and extract `public_key`.
///
/// The key is returned as the raw base64url string (no padding) that the
/// supernode uses as its `identity_pub` in the `[cluster]` section.
pub async fn collect_identity_pub(
    transport: &SshTransport,
    layout: &InstanceLayout,
) -> Result<String> {
    let identity_path = format!("{}/identity.json", layout.data_dir);
    let cmd = format!("cat {}", shell_escape(&identity_path));
    let output = transport.run(&cmd).await?;
    if output.exit_code != 0 {
        bail!(
            "failed to read {}: {} (has the supernode started at least once?)",
            identity_path,
            output.stderr.trim()
        );
    }
    parse_identity_pub(&output.stdout)
        .with_context(|| format!("parse identity.json at {identity_path}"))
}

/// Parse the `public_key` field out of an `identity.json` blob.
fn parse_identity_pub(raw: &str) -> Result<String> {
    #[derive(Deserialize)]
    struct IdentityFile {
        public_key: Option<String>,
        public_id: Option<String>,
    }
    let identity: IdentityFile = serde_json::from_str(raw.trim())
        .context("identity.json is not valid JSON")?;
    // Prefer `public_key`; fall back to `public_id` (some builds use that name).
    let key = identity
        .public_key
        .or(identity.public_id)
        .ok_or_else(|| anyhow::anyhow!("identity.json has no public_key or public_id field"))?;
    if key.trim().is_empty() {
        bail!("identity.json public_key is empty");
    }
    Ok(key.trim().to_string())
}

pub async fn fetch_invite(
    transport: &SshTransport,
    layout: &InstanceLayout,
    label: &str,
) -> Result<InviteInfo> {
    let source_path = reusable_invite_path(&layout.data_dir);
    let cmd = format!("cat {}", shell_escape(&source_path));
    let output = transport.run(&cmd).await?;
    if output.exit_code != 0 {
        bail!(
            "failed to read {}: {} (is the supernode installed and has it started at least once?)",
            source_path,
            output.stderr.trim()
        );
    }

    let invite_url = parse_reusable_invite(&output.stdout)?;
    Ok(InviteInfo {
        label: label.to_string(),
        invite_url,
        source_path,
    })
}

fn parse_reusable_invite(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        bail!("reusable_invite.json is empty");
    }

    if trimmed.starts_with("conquerd://") {
        let url = extract_conquerd_url(trimmed).context("parse invite URL")?;
        return Ok(url);
    }

    let value: serde_json::Value =
        serde_json::from_str(trimmed).context("parse reusable_invite.json as JSON")?;

    if let Some(url) = find_conquerd_url_in_value(&value) {
        return Ok(url);
    }

    if value.get("invite").is_some() {
        return build_conquerd_invite_url(&value);
    }

    bail!("no invite payload found in reusable_invite.json")
}

/// Field order must match conquerd-supernode invite signing.
#[derive(Debug, Deserialize, serde::Serialize)]
struct InvitePayload {
    inviter_peer_id: String,
    inviter_identity_pub: String,
    invite_id: String,
    expires_at: u64,
    inviter_ephemeral_pub: String,
    relay_hint: String,
    inviter_handle: String,
    is_supernode: bool,
    turn_hints: Vec<String>,
    signature: String,
}

fn build_conquerd_invite_url(root: &serde_json::Value) -> Result<String> {
    let invite = root
        .get("invite")
        .context("reusable_invite.json missing invite object")?;
    let payload: InvitePayload =
        serde_json::from_value(invite.clone()).context("parse invite object")?;
    let json = serde_json::to_string(&payload).context("serialize invite payload")?;
    let encoded = URL_SAFE_NO_PAD.encode(json.as_bytes());
    Ok(format!("conquerd://{encoded}"))
}



fn extract_conquerd_url(text: &str) -> Option<String> {
    let start = text.find("conquerd://")?;
    let rest = &text[start..];
    let end = rest
        .find(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == '\n' || c == '\r')
        .unwrap_or(rest.len());
    Some(rest[..end].to_string())
}

fn find_conquerd_url_in_value(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(s) => extract_conquerd_url(s),
        serde_json::Value::Array(items) => items.iter().find_map(find_conquerd_url_in_value),
        serde_json::Value::Object(map) => map.values().find_map(find_conquerd_url_in_value),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_REUSABLE: &str = r#"{
  "ephemeral_secret": "0UHFKZ_XMNeVjjrafd15Lz2fEsmrWNoz2MoOpRgwOXM",
  "invite": {
    "expires_at": 4934879142,
    "invite_id": "08b1ad2cfa2f9637bd6927d130fbd124",
    "inviter_ephemeral_pub": "CYdrDykEdDQ2y0athe0f-1gPMQo0F8LBIzPHfRuj3yw",
    "inviter_handle": "Relay Node",
    "inviter_identity_pub": "Zdjn_U6tnrPG-I1JFyL5G8m28xoQjmcXkYVTiUPU248",
    "inviter_peer_id": "c4166aa096f1592d8a4c4106465fda051e6427eb6322153306ae887d2e088748",
    "is_supernode": true,
    "relay_hint": "ws://155.138.244.189:35035",
    "signature": "njEUzsbQRikf62LubYP5xU49rq5kSp1z5to9nxDbV0PN-Zdnv11kwwgRbLI-ZbVBnyd6vxVUKunLaeJwDLR-Bg",
    "turn_hints": [
      "turn:155.138.244.189:3578"
    ]
  }
}"#;

    #[test]
    fn builds_conquerd_url_from_reusable_invite_json() {
        let url = parse_reusable_invite(SAMPLE_REUSABLE).unwrap();
        assert!(url.starts_with("conquerd://"));
        assert!(url.contains("eyJ"));
        assert_eq!(
            url,
            "conquerd://eyJpbnZpdGVyX3BlZXJfaWQiOiJjNDE2NmFhMDk2ZjE1OTJkOGE0YzQxMDY0NjVmZGEwNTFlNjQyN2ViNjMyMjE1MzMwNmFlODg3ZDJlMDg4NzQ4IiwiaW52aXRlcl9pZGVudGl0eV9wdWIiOiJaZGpuX1U2dG5yUEctSTFKRnlMNUc4bTI4eG9Ram1jWGtZVlRpVVBVMjQ4IiwiaW52aXRlX2lkIjoiMDhiMWFkMmNmYTJmOTYzN2JkNjkyN2QxMzBmYmQxMjQiLCJleHBpcmVzX2F0Ijo0OTM0ODc5MTQyLCJpbnZpdGVyX2VwaGVtZXJhbF9wdWIiOiJDWWRyRHlrRWREUTJ5MGF0aGUwZi0xZ1BNUW8wRjhMQkl6UEhmUnVqM3l3IiwicmVsYXlfaGludCI6IndzOi8vMTU1LjEzOC4yNDQuMTg5OjM1MDM1IiwiaW52aXRlcl9oYW5kbGUiOiJSZWxheSBOb2RlIiwiaXNfc3VwZXJub2RlIjp0cnVlLCJ0dXJuX2hpbnRzIjpbInR1cm46MTU1LjEzOC4yNDQuMTg5OjM1NzgiXSwic2lnbmF0dXJlIjoibmpFVXpzYlFSaWtmNjJMdWJZUDV4VTQ5cnE1a1NwMXo1dG85bnhEYlYwUE4tWmRudjExa3d3Z1JiTEktWmJWQm55ZDZ2eFZVS3VuTGFlSndETFItQmcifQ"
        );
    }

    #[test]
    fn parses_raw_url_file() {
        let url = parse_reusable_invite("conquerd://payload\n").unwrap();
        assert_eq!(url, "conquerd://payload");
    }

    #[test]
    fn finds_nested_url() {
        let raw = r#"{"data":{"link":"conquerd://nested"}}"#;
        let url = parse_reusable_invite(raw).unwrap();
        assert_eq!(url, "conquerd://nested");
    }
}
