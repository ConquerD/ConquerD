// ConquerD Supernode — main.rs
// Standalone Rust supernode binary: QUIC relay + SFU + WebSocket signaling + in-app portal (web.host.app.v1).

mod access;
mod config;
mod crypto;
mod handshake;
mod identity;
mod manifest;
mod peer_store;
mod protocol;
mod relay;
mod sfu;
mod sfu_module;
mod signaling;
mod stats;
mod ticket;
mod web_app_module;
mod webtransport;
mod wire;

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use parking_lot::RwLock;
use serde_json::json;
use tracing::{debug, info, warn};

use crate::access::create_access_controller;
use crate::config::Config;
use crate::handshake::HandshakeManager;
use crate::identity::Identity;
use crate::peer_store::{PeerRecord, PeerStore};
use crate::protocol::{MessageType, SignalingMessage};
use crate::relay::QUICRelayServer;
use crate::sfu::SFURoomManager;
use crate::signaling::{SignalingHandler, SignalingServer};
use crate::ticket::RelayTicket;
use conquerd_features::FeatureRegistry;
use conquerd_features::{NativeModuleLoader, TrustedKeyStore};

use crate::webtransport::BrowserBridge;

/// Application version (match client APP_VERSION).
const APP_VERSION: &str = "1.0.0";
/// Ticket renewal check interval.
const RENEWAL_CHECK_INTERVAL_S: u64 = 60;
/// Endpoint mailbox TTL (24 h).
const ENDPOINT_MAX_AGE_S: f64 = 86400.0;

/// Build the supernode's [`FeatureRegistry`] from the manifest at
/// `<data_dir>/supernode.toml`, falling back to the legacy env-var
/// `Config` toggles when the file is absent. The manifest is the single
/// source of truth for what the supernode advertises in `SUPERNODE_INFO`
/// / `CAPABILITY_ANNOUNCE`.
///
/// After registering well-known capabilities, any manifest entries with a
/// `cdylib_manifest` path are loaded via [`NativeModuleLoader`]. Signer
/// keys must be listed in `<data_dir>/trusted_module_keys.txt`; unknown
/// keys cause the entry to be skipped with a warning (no interactive
/// prompt on the supernode — add keys to the file to pre-authorise them).
fn build_feature_registry(config: &Config) -> FeatureRegistry {
    let registry = FeatureRegistry::new();
    let manifest = match manifest::SupernodeManifest::load_or_derive(&config.data_dir, config) {
        Ok(m) => m,
        Err(e) => {
            warn!(
                "[features] failed to load supernode.toml ({}); falling back to legacy env-var toggles",
                e
            );
            manifest::SupernodeManifest::from_legacy_config(config)
        }
    };
    let caps = manifest.enabled_capabilities();
    info!(
        "[features] loaded {} capability(ies) from manifest: {}",
        caps.len(),
        caps.iter()
            .map(|c| c.id.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    for cap in caps {
        if let Err(e) = registry.register(cap) {
            warn!("[features] failed to register capability: {}", e);
        }
    }

    // ── Native module loading (Phase 5) ─────────────────────────────────────
    //
    // Load cdylib entries from the manifest. Uses the trust store at
    // `<data_dir>/trusted_module_keys.txt`; unknown keys are denied with
    // a warning (headless supernode — no interactive prompt).
    let native_entries: Vec<_> = manifest.native_module_entries().cloned().collect();
    if !native_entries.is_empty() {
        let keys_path = config.data_dir.join("trusted_module_keys.txt");
        let trust_store = match TrustedKeyStore::load(&keys_path) {
            Ok(s) => s,
            Err(e) => {
                warn!("[features] failed to load trusted_module_keys.txt ({}); no native modules will be loaded", e);
                TrustedKeyStore::new()
            }
        };
        let loader = NativeModuleLoader::new(trust_store, |req| {
            warn!(
                "[features] native module '{}' by '{}' has untrusted signer key {}; \
                 add the key to trusted_module_keys.txt to allow loading",
                req.module_id, req.author, req.signer_pubkey
            );
            false // deny unknown keys on the headless supernode
        });

        for entry in native_entries {
            let manifest_path = entry.cdylib_manifest.as_ref().unwrap();
            // Derive cdylib path: same directory as the manifest, platform extension.
            let cdylib_path = {
                let stem = manifest_path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or(&entry.id);
                // Strip trailing ".module" from stem if present.
                let stem = stem.strip_suffix(".module").unwrap_or(stem);
                let ext = if cfg!(target_os = "windows") {
                    "dll"
                } else if cfg!(target_os = "macos") {
                    "dylib"
                } else {
                    "so"
                };
                manifest_path.with_file_name(format!("{stem}.{ext}"))
            };

            match loader.load(manifest_path, &cdylib_path) {
                Ok(module) => {
                    let id = module.descriptor().id.clone();
                    match registry.register_module(module) {
                        Ok(()) => info!("[features] loaded native module '{}'", id),
                        Err(e) => warn!(
                            "[features] failed to register native module '{}': {}",
                            id, e
                        ),
                    }
                }
                Err(e) => warn!(
                    "[features] failed to load native module from '{}': {}",
                    manifest_path.display(),
                    e
                ),
            }
        }
    }

    registry
}

#[cfg(test)]
mod build_feature_registry_tests {
    use super::*;

    fn cfg(data_dir: std::path::PathBuf) -> Config {
        Config {
            signaling_port: 0,
            relay_port: 0,
            web_port: None,
            chat_enabled: true,
            files_enabled: true,
            sfu_enabled: true,
            updates_enabled: false,
            auto_restart: false,
            invite_ttl_seconds: -1,
            web_title: String::new(),
            access_mode: crate::config::AccessMode::Open,
            access_code: String::new(),
            ad_duration: 0,
            tos_text: String::new(),
            ad_content: String::new(),
            demo_links: false,
            external_host: None,
            data_dir,
            web_localhost_only: false,
        }
    }

    fn tempdir() -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "conquerd-build-registry-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn falls_back_to_legacy_config_when_no_manifest() {
        let dir = tempdir();
        let registry = build_feature_registry(&cfg(dir));
        let ids: Vec<String> = registry.snapshot().iter().map(|c| c.id.clone()).collect();
        // Legacy config has all toggles on => chat+files+sfu present.
        assert!(ids.iter().any(|i| i == "core.chat.v1"));
        assert!(ids.iter().any(|i| i == "core.file.v1"));
        assert!(ids.iter().any(|i| i == "room.audio.sfu"));
    }

    #[test]
    fn manifest_file_overrides_legacy_toggles() {
        let dir = tempdir();
        std::fs::write(
            dir.join("supernode.toml"),
            "schema_version = 1\n\
             [[feature]]\n\
             id = \"core.chat.v1\"\n",
        )
        .unwrap();
        let registry = build_feature_registry(&cfg(dir));
        let ids: Vec<String> = registry.snapshot().iter().map(|c| c.id.clone()).collect();
        // Only the one capability declared in the manifest.
        assert_eq!(ids, vec!["core.chat.v1".to_string()]);
    }
}

/// Core supernode state shared across tasks.
struct SupernodeState {
    config: Config,
    identity: Identity,
    peer_store: RwLock<PeerStore>,
    handshake: RwLock<HandshakeManager>,
    relay: Option<QUICRelayServer>,
    sfu: Option<RwLock<SFURoomManager>>,
    signaling: SignalingServer,
    access_controller: Box<dyn access::AccessController>,
    start_time: Instant,
    /// peer_id → ticket expiry timestamp
    ticket_expiry: RwLock<HashMap<String, f64>>,
    /// peer_id → raw ENDPOINT_UPDATE message
    endpoint_mailbox: RwLock<HashMap<String, String>>,
    /// Pending hole-punch registrations: (peer_a, peer_b) canonical key → PunchRegistration
    pending_punches: RwLock<HashMap<(String, String), PunchRegistration>>,
    /// Capabilities advertised in `SUPERNODE_INFO`.
    features: FeatureRegistry,
    /// Browser-transport bridge. Always allocated, but only handed to a
    /// listener task when `web.host.h3.v1` is enabled in the manifest.
    web_bridge: BrowserBridge,
    /// SHA-256 fingerprint (hex) of the self-signed WebTransport TLS cert.
    ///
    /// Advertised in `SUPERNODE_INFO` so native clients can expose it to
    /// game pages via `/_conquerd/ctx.json`.  Games pass it to
    /// `new WebTransport(url, { serverCertificateHashes: [...] })` which
    /// pins the connection to this specific cert without needing a CA.
    /// This IS the ConquerD trust model: the supernode's identity was
    /// already verified via Ed25519 invite/handshake; the cert fingerprint
    /// is just an additional binding over that trusted channel.
    web_cert_fingerprint: Option<String>,
}

/// A pending hole-punch registration waiting for both peers.
struct PunchRegistration {
    registered_at: f64,
    /// peer_id → endpoint string
    endpoints: HashMap<String, String>,
}

impl SupernodeState {
    /// Send a signed message to a peer via signaling.
    fn send_signed(&self, target: &str, msg_type: MessageType, payload: serde_json::Value) {
        let msg = SignalingMessage::new(msg_type, &self.identity.public_id(), payload)
            .with_target(target)
            .sign(&self.identity);
        self.signaling.send_to_peer(target, &msg.to_json());
    }

    /// Build the JSON payload for `SUPERNODE_INFO`. Always includes the
    /// advertised capability list. When `web.host.app.v1` is enabled we
    /// also advertise the canonical `conquerd://` URL pointing at this
    /// node's identity; native clients use it to open the supernode's
    /// in-app portal in their embedded Chromium view.
    fn supernode_info_payload(&self) -> serde_json::Value {
        let caps: Vec<serde_json::Value> = self
            .features
            .snapshot()
            .iter()
            .map(|c| serde_json::to_value(c).unwrap_or(serde_json::Value::Null))
            .collect();
        let mut payload = json!({
            "node_title": self.config.web_title,
            "capabilities": caps,
        });
        if self.features.get("web.host.app.v1").is_some() {
            let obj = payload.as_object_mut().unwrap();
            obj.insert(
                "app_url".into(),
                json!(format!("conquerd://{}/", self.identity.public_id())),
            );
        }
        // Advertise WebTransport base URL and cert fingerprint when
        // web.host.h3.v1 is enabled.  Both are consumed by the native client
        // and injected into game pages via /_conquerd/ctx.json so that
        // `new WebTransport(url, { serverCertificateHashes: [{ algorithm:
        // 'sha-256', value: fingerprint }] })` can connect without any CA
        // cert.  The fingerprint IS the trust anchor — it was received over
        // the already Ed25519-verified SUPERNODE_INFO channel.
        if self.features.get("web.host.h3.v1").is_some() {
            let port = self.config.web_port.unwrap_or(8443);
            let host = match self.config.external_host.as_deref() {
                Some(h) if !h.is_empty() && h != "0.0.0.0" => h.to_owned(),
                _ => "localhost".to_owned(),
            };
            let obj = payload.as_object_mut().unwrap();
            obj.insert("wt_url".into(), json!(format!("https://{}:{}", host, port)));
            if let Some(ref fp) = self.web_cert_fingerprint {
                obj.insert("cert_fingerprint".into(), json!(fp));
            }
        }
        payload
    }

    /// Issue a relay ticket to a peer.
    fn issue_relay_ticket(&self, peer_pub: &str) {
        let Some(ref relay) = self.relay else { return };

        let external = self.config.external_host.as_deref().unwrap_or("0.0.0.0");
        if external == "0.0.0.0" {
            warn!(
                "Relay ticket for {} uses 0.0.0.0 — set supernode_host env var",
                &peer_pub[..12.min(peer_pub.len())]
            );
        }
        let ticket = RelayTicket::create(
            peer_pub,
            external,
            self.config.relay_port,
            &self.identity.signing_key,
        );

        relay.allow_peer_update(peer_pub);

        self.ticket_expiry
            .write()
            .insert(peer_pub.to_string(), ticket.expires_at);

        self.send_signed(
            peer_pub,
            MessageType::RelayGranted,
            json!({
                "ticket": ticket.to_value(),
                "relay_host": ticket.relay_host,
                "relay_port": ticket.relay_port,
            }),
        );

        // Send SUPERNODE_INFO (always includes capabilities; web fields
        // are added when the portal is running).
        self.send_signed(
            peer_pub,
            MessageType::SupernodeInfo,
            self.supernode_info_payload(),
        );

        info!(
            "Issued relay ticket to {}",
            &peer_pub[..12.min(peer_pub.len())]
        );
    }

    /// Handle a newly trusted peer.
    /// Send a `CAPABILITY_ANNOUNCE` to a freshly-trusted peer. Mirrors the
    /// capability snapshot we already include in `SUPERNODE_INFO` so peers
    /// have a single canonical channel to learn what this supernode speaks.
    fn announce_capabilities_to(&self, identity_pub: &str) {
        let caps: Vec<serde_json::Value> = self
            .features
            .snapshot()
            .iter()
            .map(|c| serde_json::to_value(c).unwrap_or(serde_json::Value::Null))
            .collect();
        self.send_signed(
            identity_pub,
            MessageType::CapabilityAnnounce,
            json!({ "capabilities": caps }),
        );
    }

    fn on_peer_trusted(&self, identity_pub: &str) {
        // Always advertise capabilities so peers can negotiate features
        // independently of whether relay access has been granted.
        self.announce_capabilities_to(identity_pub);

        if self.access_controller.check_access(identity_pub) {
            self.issue_relay_ticket(identity_pub);
        } else {
            // Trusted but not yet access-granted. The legacy HTTPS portal
            // redirect (`RelayPaymentRequired`) has been removed; out-of-band
            // grant flow (operator CLI / `access_controller`) is the path now.
            info!(
                "Peer {} is trusted but lacks relay access; awaiting operator grant",
                &identity_pub[..12.min(identity_pub.len())]
            );
        }

        // Replay stored endpoint updates
        self.replay_endpoint_updates(identity_pub);
    }

    /// Revoke a peer's relay access.
    #[allow(dead_code)]
    fn on_peer_revoked(&self, identity_pub: &str) {
        info!(
            "Trust revoked for {}",
            &identity_pub[..12.min(identity_pub.len())]
        );
        if let Some(ref relay) = self.relay {
            relay.revoke_peer(identity_pub);
        }
        self.ticket_expiry.write().remove(identity_pub);
        if let Some(ref sfu) = self.sfu {
            sfu.write().remove_peer_from_all(identity_pub);
        }
        self.send_signed(
            identity_pub,
            MessageType::RelayRevoke,
            json!({"reason": "trust_revoked"}),
        );
    }

    /// Broadcast updated SFU room list to all connected trusted peers.
    fn broadcast_room_list(&self) {
        let Some(ref sfu) = self.sfu else { return };
        for peer_id in self.signaling.connected_peer_ids() {
            if self.peer_store.read().is_trusted(&peer_id) {
                let rooms = sfu.read().get_rooms_for_peer(&peer_id);
                self.send_signed(&peer_id, MessageType::SfuRoomList, json!({"rooms": rooms}));
            }
        }
    }

    /// Resolve the best known endpoint for a peer (for hole punching).
    /// Priority: 1. NAT-mapped addr from relay QUIC conn, 2. endpoint mailbox.
    fn resolve_punch_endpoint(&self, peer_id: &str) -> Option<String> {
        // 1. Observed relay connection address
        if let Some(ref relay) = self.relay {
            if let Some(addr) = relay.get_peer_remote_addr(peer_id) {
                return Some(format!("{}:{}", addr.ip(), addr.port()));
            }
        }
        // 2. Endpoint mailbox — parse the raw JSON to extract listener
        if let Some(raw) = self.endpoint_mailbox.read().get(peer_id) {
            if let Ok(msg) = serde_json::from_str::<serde_json::Value>(raw) {
                if let Some(listener) = msg
                    .get("payload")
                    .and_then(|p| p.get("listener"))
                    .and_then(|v| v.as_str())
                {
                    // listener is typically ws://ip:port — extract host:port
                    if let Some(stripped) = listener.strip_prefix("ws://") {
                        return Some(stripped.to_string());
                    }
                    return Some(listener.to_string());
                }
            }
        }
        None
    }

    /// Attempt relay-coordinated hole punch for a newly-joined room member
    /// with every other relay-connected room member whose endpoint is known.
    fn try_relay_punch_for_room(&self, new_peer: &str, room_id: &str) {
        let Some(ref relay) = self.relay else { return };
        let room_peers = relay.get_room_peers(room_id);
        let new_ep = match self.resolve_punch_endpoint(new_peer) {
            Some(ep) => ep,
            None => {
                debug!(
                    "[relay-punch] No endpoint for new peer {} — skipping",
                    &new_peer[..12.min(new_peer.len())]
                );
                return;
            }
        };

        for other_peer in &room_peers {
            if other_peer == new_peer {
                continue;
            }
            let other_ep = match self.resolve_punch_endpoint(other_peer) {
                Some(ep) => ep,
                None => {
                    debug!(
                        "[relay-punch] No endpoint for peer {} — skipping pair",
                        &other_peer[..12.min(other_peer.len())]
                    );
                    continue;
                }
            };
            self.send_punch_ready(new_peer, other_peer, &new_ep, &other_ep);
        }
    }

    /// Send PUNCH_READY to both peers with coordinated timing.
    fn send_punch_ready(&self, peer_a: &str, peer_b: &str, ep_a: &str, ep_b: &str) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        let punch_at = now + 0.5; // 500ms from now

        self.send_signed(
            peer_a,
            MessageType::PunchReady,
            json!({
                "peer_id": peer_b,
                "peer_endpoint": ep_b,
                "your_endpoint": ep_a,
                "punch_at": punch_at,
            }),
        );
        self.send_signed(
            peer_b,
            MessageType::PunchReady,
            json!({
                "peer_id": peer_a,
                "peer_endpoint": ep_a,
                "your_endpoint": ep_b,
                "punch_at": punch_at,
            }),
        );
        info!(
            "[punch] PUNCH_READY sent to {} ↔ {} (punch_at={:.3})",
            &peer_a[..12.min(peer_a.len())],
            &peer_b[..12.min(peer_b.len())],
            punch_at,
        );
    }

    /// Handle a PUNCH_REGISTER message: store the registration, and if
    /// both peers have registered, send PUNCH_READY to both.
    fn handle_punch_register_msg(&self, sender: &str, target_peer: &str, sender_endpoint: &str) {
        // Verify both peers are trusted
        if !self.peer_store.read().is_trusted(sender) {
            warn!(
                "[punch] PUNCH_REGISTER from untrusted peer {}",
                &sender[..12.min(sender.len())]
            );
            return;
        }
        if !self.peer_store.read().is_trusted(target_peer) {
            warn!(
                "[punch] PUNCH_REGISTER for untrusted target {}",
                &target_peer[..12.min(target_peer.len())]
            );
            return;
        }

        // Canonical pair key (sorted order)
        let pair_key = if sender < target_peer {
            (sender.to_string(), target_peer.to_string())
        } else {
            (target_peer.to_string(), sender.to_string())
        };

        let now_ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();

        let mut punches = self.pending_punches.write();

        let entry = punches
            .entry(pair_key.clone())
            .or_insert_with(|| PunchRegistration {
                registered_at: now_ts,
                endpoints: HashMap::new(),
            });

        entry
            .endpoints
            .insert(sender.to_string(), sender_endpoint.to_string());

        info!(
            "[punch] Registration from {} → {} (endpoint={})",
            &sender[..12.min(sender.len())],
            &target_peer[..12.min(target_peer.len())],
            sender_endpoint,
        );

        // Check if both peers have registered
        if entry.endpoints.contains_key(sender) && entry.endpoints.contains_key(target_peer) {
            let ep_a = entry
                .endpoints
                .get(&pair_key.0)
                .cloned()
                .unwrap_or_default();
            let ep_b = entry
                .endpoints
                .get(&pair_key.1)
                .cloned()
                .unwrap_or_default();
            // Remove the entry before sending (release lock)
            punches.remove(&pair_key);
            drop(punches);

            self.send_punch_ready(&pair_key.0, &pair_key.1, &ep_a, &ep_b);
        } else {
            // Clean up stale entries (>30s old)
            let stale_keys: Vec<(String, String)> = punches
                .iter()
                .filter(|(_, info)| now_ts - info.registered_at > 30.0)
                .map(|(k, _)| k.clone())
                .collect();
            for key in stale_keys {
                debug!("[punch] Cleaning up stale punch registration");
                punches.remove(&key);
            }
        }
    }

    /// Store endpoint update in mailbox, persist to disk.
    fn on_endpoint_update(&self, sender: &str, raw: &str) {
        self.endpoint_mailbox
            .write()
            .insert(sender.to_string(), raw.to_string());
        self.save_endpoint_mailbox();
    }

    /// Replay stored endpoint updates to a reconnecting peer.
    fn replay_endpoint_updates(&self, target: &str) {
        let mailbox = self.endpoint_mailbox.read();
        let peer_store = self.peer_store.read();
        for (sender_id, raw) in mailbox.iter() {
            if sender_id != target && peer_store.is_trusted(sender_id) {
                self.signaling.send_to_peer(target, raw);
            }
        }
    }

    /// Persist endpoint mailbox to disk.
    fn save_endpoint_mailbox(&self) {
        let path = self.config.data_dir.join("supernode_endpoints.json");
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        let mailbox = self.endpoint_mailbox.read();
        let entries: Vec<serde_json::Value> = mailbox
            .iter()
            .map(|(peer_id, raw)| {
                json!({
                    "peer_id": peer_id,
                    "raw": raw,
                    "stored_at": now,
                })
            })
            .collect();
        drop(mailbox);
        if let Ok(data) = serde_json::to_string_pretty(&entries) {
            if let Err(e) = std::fs::write(&path, data) {
                warn!("Failed to save endpoint mailbox: {}", e);
            }
        }
    }

    /// Load endpoint mailbox from disk (filtering entries older than 24h).
    /// Accepts both current format (JSON array of {peer_id, raw, stored_at})
    /// and legacy format (JSON object {peer_id: {timestamp, ...raw_msg}}).
    fn load_endpoint_mailbox(data_dir: &std::path::Path) -> HashMap<String, String> {
        let path = data_dir.join("supernode_endpoints.json");
        let mut result = HashMap::new();
        let data = match std::fs::read_to_string(&path) {
            Ok(d) => d,
            Err(_) => return result,
        };
        let parsed: serde_json::Value = match serde_json::from_str(&data) {
            Ok(v) => v,
            Err(e) => {
                warn!("Failed to parse endpoint mailbox: {}", e);
                return result;
            }
        };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();

        if let Some(entries) = parsed.as_array() {
            // Rust format: [{peer_id, raw, stored_at}, ...]
            for entry in entries {
                let peer_id = entry.get("peer_id").and_then(|v| v.as_str()).unwrap_or("");
                let raw = entry.get("raw").and_then(|v| v.as_str()).unwrap_or("");
                let stored_at = entry
                    .get("stored_at")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                if peer_id.is_empty() || raw.is_empty() {
                    continue;
                }
                if now - stored_at > ENDPOINT_MAX_AGE_S {
                    continue;
                }
                result.insert(peer_id.to_string(), raw.to_string());
            }
        } else if let Some(obj) = parsed.as_object() {
            // Legacy format: {peer_id: {timestamp: ..., ...raw_msg}}
            for (peer_id, value) in obj {
                let stored_at = value
                    .get("timestamp")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                if now - stored_at > ENDPOINT_MAX_AGE_S {
                    continue;
                }
                // Store the entire raw message JSON string for replay
                if let Ok(raw) = serde_json::to_string(value) {
                    result.insert(peer_id.clone(), raw);
                }
            }
        } else {
            warn!("Unexpected endpoint mailbox format");
        }

        info!("Loaded {} endpoint mailbox entries", result.len());
        result
    }

    /// Collect stats for /health and /api/stats.
    pub(crate) fn collect_stats(&self) -> serde_json::Value {
        let mut features = vec![];
        if self.config.chat_enabled {
            features.push("chat");
        }
        if self.config.files_enabled {
            features.push("files");
        }
        if self.config.sfu_enabled {
            features.push("sfu");
        }
        if self.relay.is_some() {
            features.push("relay");
        }

        let mut value = stats::collect_stats(
            APP_VERSION,
            self.start_time,
            self.access_controller.mode_name(),
            &features,
            self.peer_store.read().trusted_count(),
            self.signaling.state().read().connected_count,
            self.relay.as_ref(),
            self.sfu.as_ref(),
        );

        // Merge portal config so the browser-side HTML can read it.
        if let Some(obj) = value.as_object_mut() {
            obj.insert("portal".into(), self.portal_config());
        }
        value
    }

    /// Returns the WebTransport base URL for the `web.host.h3.v1` listener.
    ///
    /// Used by the `/api/wt-url` endpoint so game pages loaded inside the
    /// native portal (where `location.hostname` is the base64url supernode
    /// ID rather than a resolvable DNS name) can discover the real address
    /// to pass to `new WebTransport(url)`.
    ///
    /// When `external_host` is not configured the URL falls back to
    /// `https://localhost:<web_port>`, which works for local development.
    pub(crate) fn wt_url_json(&self) -> serde_json::Value {
        if self.features.get("web.host.h3.v1").is_none() {
            return json!({"error": "web.host.h3.v1 not enabled on this supernode"});
        }
        let port = self.config.web_port.unwrap_or(8443);
        let host = match self.config.external_host.as_deref() {
            Some(h) if !h.is_empty() && h != "0.0.0.0" => h.to_owned(),
            _ => "localhost".to_owned(),
        };
        let url = format!("https://{}:{}", host, port);
        // Include the cert fingerprint alongside the URL so the SDK can use
        // serverCertificateHashes even when it reaches us via the /api/wt-url
        // fallback (i.e. before SUPERNODE_INFO has been delivered via the
        // native client trust chain).
        match &self.web_cert_fingerprint {
            Some(fp) => json!({"url": url, "certHash": fp}),
            None => json!({"url": url}),
        }
    }

    /// Returns the public portal configuration used by browser-side templates.
    /// Safe to expose: no secrets (access_code is omitted).
    pub(crate) fn portal_config(&self) -> serde_json::Value {
        json!({
            "title": self.config.web_title,
            "access_mode": self.access_controller.mode_name(),
            "demo_links": self.config.demo_links,
            "ad_duration": self.config.ad_duration,
            "tos_text": self.config.tos_text,
            "ad_content": self.config.ad_content,
            "web_localhost_only": self.config.web_localhost_only,
        })
    }

    pub(crate) fn collect_peers_info(&self) -> serde_json::Value {
        let store = self.peer_store.read();
        let connected_ids = self.signaling.connected_peer_ids();
        let connected_set: HashSet<&str> = connected_ids.iter().map(|s| s.as_str()).collect();

        let peers: Vec<serde_json::Value> = store
            .trusted_peer_ids()
            .iter()
            .filter_map(|id| {
                let rec = store.get_peer(id)?;
                let has_relay = self.access_controller.check_access(id);
                let online = connected_set.contains(id.as_str());
                Some(json!({
                    "handle": if rec.handle.is_empty() { &rec.peer_id[..12.min(rec.peer_id.len())] } else { &rec.handle },
                    "peer_id_short": &rec.peer_id[..12.min(rec.peer_id.len())],
                    "online": online,
                    "relay_access": has_relay,
                    "access_mode": self.access_controller.mode_name(),
                }))
            })
            .collect();

        json!(peers)
    }
}

/// Implements SignalingHandler for the supernode.
struct SupernodeHandler {
    state: Arc<SupernodeState>,
}

impl SignalingHandler for SupernodeHandler {
    fn on_message(&self, msg: SignalingMessage, raw: &str) {
        match msg.msg_type {
            MessageType::InviteHandshakeInit => {
                self.handle_handshake_init(&msg);
            }
            MessageType::EndpointUpdate => {
                self.state.on_endpoint_update(&msg.sender, raw);
            }
            MessageType::SupernodeInfoRequest => {
                self.state.send_signed(
                    &msg.sender,
                    MessageType::SupernodeInfo,
                    self.state.supernode_info_payload(),
                );
            }
            MessageType::SfuJoin => {
                self.handle_sfu_join(&msg);
            }
            MessageType::SfuLeave => {
                self.handle_sfu_leave(&msg);
            }
            MessageType::SfuRoomList => {
                self.handle_sfu_room_list(&msg);
            }
            MessageType::SfuChat => {
                self.handle_sfu_chat_broadcast(&msg, raw);
            }
            MessageType::SfuAudio => {
                self.handle_sfu_broadcast(&msg, raw, MessageType::SfuAudio);
            }
            MessageType::SfuFileOffer
            | MessageType::SfuFileChunk
            | MessageType::SfuFileComplete => {
                self.handle_sfu_broadcast(&msg, raw, msg.msg_type);
            }
            MessageType::SfuSubscribe => {
                self.handle_sfu_subscribe(&msg);
            }
            MessageType::SfuUnsubscribe => {
                self.handle_sfu_unsubscribe(&msg);
            }
            MessageType::SfuRoomCreate => {
                self.handle_sfu_room_create(&msg);
            }
            MessageType::SfuRoomInvite => {
                self.handle_sfu_room_invite(&msg);
            }
            MessageType::SfuRoomInviteGenerate => {
                self.handle_sfu_invite_generate(&msg);
            }
            MessageType::PunchRegister => {
                self.handle_punch_register(&msg);
            }
            MessageType::ChatMessage => {
                // Log chat (supernode acts as relay)
                if let Some(body) = msg.payload.get("body").and_then(|v| v.as_str()) {
                    info!(
                        "[chat] {}: {}",
                        &msg.sender[..12.min(msg.sender.len())],
                        body
                    );
                }
            }
            MessageType::TrustRequest | MessageType::TrustAccept => {
                // Clients send these to the supernode (target=supernode_id) with the
                // actual recipient in payload["target"]. Relay raw message to that peer.
                if let Some(target_id) = msg.payload.get("target").and_then(|v| v.as_str()) {
                    self.state.signaling.send_to_peer(target_id, raw);
                    debug!(
                        "Relayed {:?} from {} → {}",
                        msg.msg_type,
                        &msg.sender[..12.min(msg.sender.len())],
                        &target_id[..12.min(target_id.len())],
                    );
                } else {
                    debug!(
                        "[trust] {:?} from {} missing payload.target — dropped",
                        msg.msg_type,
                        &msg.sender[..12.min(msg.sender.len())],
                    );
                }
            }
            MessageType::VersionAnnounce
            | MessageType::UpdateOffer
            | MessageType::UpdateAccept
            | MessageType::UpdateReject
            | MessageType::HandleUpdate
            | MessageType::PresenceUpdate
            | MessageType::SpeakingState
            | MessageType::CallRequest
            | MessageType::CallAccept
            | MessageType::CallReject
            | MessageType::CallEnd
            | MessageType::EncryptedSignal
            | MessageType::FileTransferOffer
            | MessageType::FileTransferAccept
            | MessageType::FileTransferReject
            | MessageType::FileTransferChunk
            | MessageType::FileTransferComplete
            | MessageType::FileTransferAck
            | MessageType::FileTransferError
            | MessageType::ChatAck
            | MessageType::ChatTyping
            | MessageType::PeerRoomInvite
            | MessageType::BuildAttestation
            | MessageType::AttestationResponse
            | MessageType::CapabilityAnnounce
            | MessageType::Pong => {
                // Peer-to-peer relay only — forwarding handled by signaling.rs.
            }
            MessageType::CapabilityInvoke => {
                // Route to a targeted peer when the payload carries a `target` field;
                // otherwise treat as a supernode-directed invocation.
                if let Some(target_id) = msg.payload.get("target").and_then(|v| v.as_str()) {
                    self.state.signaling.send_to_peer(target_id, raw);
                    debug!(
                        "Relayed CAPABILITY_INVOKE from {} → {}",
                        &msg.sender[..12.min(msg.sender.len())],
                        &target_id[..12.min(target_id.len())],
                    );
                } else {
                    let feature_id = msg.payload.get("id").and_then(|v| v.as_str()).unwrap_or("");
                    if feature_id.is_empty() {
                        debug!(
                            "CAPABILITY_INVOKE from {} missing 'id' — dropped",
                            &msg.sender[..12.min(msg.sender.len())],
                        );
                    } else {
                        debug!(
                            "CAPABILITY_INVOKE '{}' from {} (supernode-directed, no module registered)",
                            feature_id,
                            &msg.sender[..12.min(msg.sender.len())],
                        );
                    }
                }
            }
            MessageType::Ping => {
                self.state
                    .send_signed(&msg.sender, MessageType::Pong, json!({}));
            }
            MessageType::RelayRequest => {
                // Client explicitly requests (or refreshes) a relay ticket.
                // Re-issue if the peer is trusted and has relay access.
                let trusted = self.state.peer_store.read().is_trusted(&msg.sender);
                if trusted && self.state.access_controller.check_access(&msg.sender) {
                    debug!(
                        "[relay] RelayRequest from {} — re-issuing ticket",
                        &msg.sender[..12.min(msg.sender.len())]
                    );
                    self.state.issue_relay_ticket(&msg.sender);
                } else {
                    debug!(
                        "[relay] RelayRequest from {} ignored (not trusted / no access)",
                        &msg.sender[..12.min(msg.sender.len())]
                    );
                }
            }
            _ => {
                // Unexpected message type — log for diagnostics.
                debug!(
                    "Unhandled message type {:?} from {}",
                    msg.msg_type,
                    &msg.sender[..12.min(msg.sender.len())],
                );
            }
        }
    }

    fn on_peer_connected(&self, identity_pub: &str) {
        let is_trusted = self.state.peer_store.read().is_trusted(identity_pub);
        if !is_trusted {
            info!(
                "Peer {} connected but NOT trusted — ignoring (handshake required)",
                &identity_pub[..12.min(identity_pub.len())],
            );
            return;
        }
        self.state.peer_store.write().touch_peer(identity_pub);
        self.state.on_peer_trusted(identity_pub);

        // Announce supernode version to the peer
        self.state.send_signed(
            identity_pub,
            MessageType::VersionAnnounce,
            json!({"version": APP_VERSION}),
        );

        // Send SFU room list
        if let Some(ref sfu) = self.state.sfu {
            let rooms = sfu.read().get_rooms_for_peer(identity_pub);
            self.state.send_signed(
                identity_pub,
                MessageType::SfuRoomList,
                json!({"rooms": rooms}),
            );
        }
    }

    fn on_peer_disconnected(&self, identity_pub: &str) {
        // Remove from SFU rooms
        if let Some(ref sfu) = self.state.sfu {
            let left_rooms = sfu.write().remove_peer_from_all(identity_pub);
            if !left_rooms.is_empty() {
                for (room_id, members) in &left_rooms {
                    // Notify remaining members
                    for member in members {
                        self.state.send_signed(
                            member,
                            MessageType::SfuPeerLeft,
                            json!({"peer_id": identity_pub, "room_id": room_id}),
                        );
                    }
                }
                // Broadcast updated room list (participant counts changed)
                self.state.broadcast_room_list();
            }
        }
    }
}

impl SupernodeHandler {
    fn handle_handshake_init(&self, msg: &SignalingMessage) {
        let hs = self.state.handshake.read();
        match hs.process_init(&msg.payload) {
            Ok((accept_payload, _session_key, joiner_pub)) => {
                drop(hs);

                // Add to peer store
                let joiner_peer_id = msg
                    .payload
                    .get("joiner_peer_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let joiner_handle = msg
                    .payload
                    .get("joiner_handle")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let joiner_listener = msg
                    .payload
                    .get("joiner_listener")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let transcript_hash = accept_payload
                    .get("transcript_hash")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                let mut store = self.state.peer_store.write();
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs_f64();
                store.add_peer(PeerRecord {
                    peer_id: joiner_peer_id,
                    identity_pub: joiner_pub.clone(),
                    relay_hints: if joiner_listener.is_empty() {
                        vec![]
                    } else {
                        vec![joiner_listener]
                    },
                    handle: joiner_handle,
                    blocked: false,
                    revoked: false,
                    auto_connect: false,
                    is_supernode: false,
                    transcript_hash,
                    created_at: now,
                    last_seen_at: now,
                    quic_port: msg
                        .payload
                        .get("joiner_quic_port")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as u16,
                });
                let _ = store.save();
                drop(store);

                // Send accept
                self.state.send_signed(
                    &joiner_pub,
                    MessageType::InviteHandshakeAccept,
                    accept_payload,
                );

                info!(
                    "Handshake complete with {}",
                    &joiner_pub[..12.min(joiner_pub.len())]
                );

                // Trigger trust flow
                self.state.on_peer_trusted(&joiner_pub);

                // Announce supernode version to the newly trusted peer
                self.state.send_signed(
                    &joiner_pub,
                    MessageType::VersionAnnounce,
                    json!({"version": APP_VERSION}),
                );

                // Send SFU room list so rooms appear immediately
                if let Some(ref sfu) = self.state.sfu {
                    let rooms = sfu.read().get_rooms_for_peer(&joiner_pub);
                    self.state.send_signed(
                        &joiner_pub,
                        MessageType::SfuRoomList,
                        json!({"rooms": rooms}),
                    );
                }

                // Send trusted peer list
                let trusted = self.state.peer_store.read().trusted_peer_ids();
                let peer_list: Vec<serde_json::Value> = trusted
                    .iter()
                    .filter_map(|id| {
                        let store = self.state.peer_store.read();
                        store.get_peer(id).map(|p| {
                            json!({
                                "peer_id": p.peer_id,
                                "identity_pub": p.identity_pub,
                                "handle": p.handle,
                                "is_supernode": p.is_supernode,
                                "auto_connect": p.auto_connect,
                            })
                        })
                    })
                    .collect();
                self.state.send_signed(
                    &joiner_pub,
                    MessageType::Welcome,
                    json!({"peers": peer_list}),
                );
            }
            Err(e) => {
                warn!("Handshake init error: {}", e);
                self.state.send_signed(
                    &msg.sender,
                    MessageType::InviteHandshakeReject,
                    json!({"reason": e}),
                );
            }
        }
    }

    fn handle_sfu_join(&self, msg: &SignalingMessage) {
        let Some(ref sfu) = self.state.sfu else {
            return;
        };
        let room_id = msg
            .payload
            .get("room_id")
            .and_then(|v| v.as_str())
            .unwrap_or(sfu::DEFAULT_ROOM_ID);

        let (ok, members) = sfu.write().join_room(&msg.sender, room_id);
        if !ok {
            return;
        }

        // Join relay room too
        if let Some(ref relay) = self.state.relay {
            relay.join_room(&msg.sender, room_id);
        }

        // Send member list to joiner
        self.state.send_signed(
            &msg.sender,
            MessageType::SfuMembers,
            json!({"room_id": room_id, "members": members}),
        );

        // Notify existing members
        for member in &members {
            if member != &msg.sender {
                self.state.send_signed(
                    member,
                    MessageType::SfuPeerJoined,
                    json!({"peer_id": msg.sender, "room_id": room_id}),
                );
            }
        }

        info!(
            "Peer {} joined room {}",
            &msg.sender[..12.min(msg.sender.len())],
            room_id
        );

        // Attempt relay-coordinated hole punch with existing room members
        self.state.try_relay_punch_for_room(&msg.sender, room_id);
    }

    fn handle_sfu_leave(&self, msg: &SignalingMessage) {
        let Some(ref sfu) = self.state.sfu else {
            return;
        };
        let room_id = msg
            .payload
            .get("room_id")
            .and_then(|v| v.as_str())
            .unwrap_or(sfu::DEFAULT_ROOM_ID);

        let remaining = sfu.write().leave_room(&msg.sender, room_id);

        if let Some(ref relay) = self.state.relay {
            relay.leave_room(&msg.sender);
        }

        for member in &remaining {
            self.state.send_signed(
                member,
                MessageType::SfuPeerLeft,
                json!({"peer_id": msg.sender, "room_id": room_id}),
            );
        }
    }

    fn handle_sfu_room_list(&self, msg: &SignalingMessage) {
        let Some(ref sfu) = self.state.sfu else {
            return;
        };
        let rooms = sfu.read().get_rooms_for_peer(&msg.sender);
        self.state.send_signed(
            &msg.sender,
            MessageType::SfuRoomList,
            json!({"rooms": rooms}),
        );
    }

    fn handle_sfu_broadcast(&self, msg: &SignalingMessage, raw: &str, mt: MessageType) {
        let Some(ref sfu) = self.state.sfu else {
            return;
        };
        let room_id = msg
            .payload
            .get("room_id")
            .and_then(|v| v.as_str())
            .unwrap_or(sfu::DEFAULT_ROOM_ID);
        let recipients = if matches!(
            mt,
            MessageType::SfuFileOffer | MessageType::SfuFileChunk | MessageType::SfuFileComplete
        ) {
            sfu.read().get_chat_recipients(room_id)
        } else {
            sfu.read().get_room_members(room_id)
        };
        for peer in &recipients {
            if peer != &msg.sender {
                self.state.signaling.send_to_peer(peer, raw);
            }
        }
    }

    /// Broadcast SFU_CHAT to voice participants AND text-chat subscribers.
    fn handle_sfu_chat_broadcast(&self, msg: &SignalingMessage, raw: &str) {
        let Some(ref sfu) = self.state.sfu else {
            return;
        };
        let room_id = msg
            .payload
            .get("room_id")
            .and_then(|v| v.as_str())
            .unwrap_or(sfu::DEFAULT_ROOM_ID);
        let recipients = sfu.read().get_chat_recipients(room_id);
        for peer in &recipients {
            if peer != &msg.sender {
                self.state.signaling.send_to_peer(peer, raw);
            }
        }
    }

    fn handle_sfu_subscribe(&self, msg: &SignalingMessage) {
        let Some(ref sfu) = self.state.sfu else {
            return;
        };
        let room_id = msg
            .payload
            .get("room_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if room_id.is_empty() {
            return;
        }
        let ok = sfu.write().subscribe(&msg.sender, room_id);
        if ok {
            debug!(
                "Peer {} subscribed to room {} text chat",
                &msg.sender[..12.min(msg.sender.len())],
                &room_id[..12.min(room_id.len())]
            );
        }
    }

    fn handle_sfu_unsubscribe(&self, msg: &SignalingMessage) {
        let Some(ref sfu) = self.state.sfu else {
            return;
        };
        let room_id = msg
            .payload
            .get("room_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if room_id.is_empty() {
            return;
        }
        sfu.write().unsubscribe(&msg.sender, room_id);
        debug!(
            "Peer {} unsubscribed from room {} text chat",
            &msg.sender[..12.min(msg.sender.len())],
            &room_id[..12.min(room_id.len())]
        );
    }

    fn handle_sfu_room_create(&self, msg: &SignalingMessage) {
        let Some(ref sfu) = self.state.sfu else {
            return;
        };
        let room_name = msg
            .payload
            .get("room_name")
            .and_then(|v| v.as_str())
            .unwrap_or("Room");
        let room_type: sfu::RoomType = msg
            .payload
            .get("room_type")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or(sfu::RoomType::Public);
        let room_id = msg.payload.get("room_id").and_then(|v| v.as_str());

        let mut sfu_lock = sfu.write();
        if let Some(room) = sfu_lock.create_room(room_id, room_name, room_type, &msg.sender) {
            let room_id_out = room.room_id.clone();
            let room_name_out = room.room_name.clone();
            let is_private = room_type == sfu::RoomType::Private;
            drop(sfu_lock);

            let invite_token = if is_private {
                sfu.write().generate_invite_token(&room_id_out, &msg.sender)
            } else {
                None
            };

            self.state.send_signed(
                &msg.sender,
                MessageType::SfuRoomCreated,
                json!({
                    "room_id": room_id_out,
                    "room_name": room_name_out,
                    "room_type": room_type,
                    "invite_token": invite_token,
                }),
            );

            // If public, persist rooms immediately so they survive a SIGTERM restart,
            // then broadcast updated room list to all connected trusted peers.
            if !is_private {
                let rooms_path = self.state.config.data_dir.join("sfu_rooms.json");
                let _ = sfu.read().save_rooms(&rooms_path);
                self.state.broadcast_room_list();
            }
        }
    }

    fn handle_sfu_room_invite(&self, msg: &SignalingMessage) {
        let Some(ref sfu) = self.state.sfu else {
            return;
        };
        let room_id = msg
            .payload
            .get("room_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let token = msg
            .payload
            .get("invite_token")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let valid = sfu
            .write()
            .validate_room_invite(room_id, token, &msg.sender);
        let room_info = sfu
            .read()
            .get_room(room_id)
            .map(|r| (r.room_name.clone(), r.room_type, r.participant_count()));

        if let Some((name, rtype, count)) = room_info {
            self.state.send_signed(
                &msg.sender,
                MessageType::SfuRoomInviteResult,
                json!({
                    "room_id": room_id,
                    "accepted": valid,
                    "room_name": name,
                    "room_type": rtype,
                    "member_count": count,
                    "reason": if valid { "" } else { "invalid_token" },
                }),
            );

            // Send updated room list so the peer can see the private room
            if valid {
                let rooms = sfu.read().get_rooms_for_peer(&msg.sender);
                self.state.send_signed(
                    &msg.sender,
                    MessageType::SfuRoomList,
                    json!({"rooms": rooms}),
                );
            }
        }
    }

    fn handle_sfu_invite_generate(&self, msg: &SignalingMessage) {
        let Some(ref sfu) = self.state.sfu else {
            return;
        };
        let room_id = msg
            .payload
            .get("room_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if let Some(token) = sfu.write().generate_invite_token(room_id, &msg.sender) {
            info!(
                "[sfu] Generated invite token for room {} requested by {}",
                &room_id[..12.min(room_id.len())],
                &msg.sender[..12.min(msg.sender.len())],
            );
            self.state.send_signed(
                &msg.sender,
                MessageType::SfuRoomInviteResult,
                json!({"room_id": room_id, "accepted": true, "invite_token": token}),
            );
        } else {
            warn!(
                "[sfu] Invite generate failed for room {} — room not found",
                &room_id[..12.min(room_id.len())],
            );
            self.state.send_signed(
                &msg.sender,
                MessageType::SfuRoomInviteResult,
                json!({"room_id": room_id, "accepted": false, "reason": "room_not_found"}),
            );
        }
    }

    fn handle_punch_register(&self, msg: &SignalingMessage) {
        let target = msg
            .payload
            .get("target_peer")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let endpoint = msg
            .payload
            .get("sender_endpoint")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if target.is_empty() || endpoint.is_empty() {
            return;
        }
        self.state
            .handle_punch_register_msg(&msg.sender, target, endpoint);
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Install the ring crypto provider before any TLS/QUIC activity.
    // Required when rustls is built with default-features = false.
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");

    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,conquerd_supernode=debug".parse().unwrap()),
        )
        .init();

    let config = Config::from_env();
    info!(
        "ConquerD Supernode v{} starting (signaling={}, relay={}, web={:?})",
        APP_VERSION, config.signaling_port, config.relay_port, config.web_port
    );

    // Ensure data directory
    std::fs::create_dir_all(&config.data_dir)?;

    // Load or create identity
    let identity = Identity::load_or_create(&config.data_dir)?;
    info!(
        "Identity: {} (peer: {}...)",
        &identity.public_id()[..12],
        &identity.peer_id()[..12]
    );

    // Load peer store
    let peer_store = PeerStore::new(&config.data_dir.join("peers.json"));
    info!("Loaded {} trusted peers", peer_store.trusted_count());

    // Initialize access controller
    let access_controller = create_access_controller(config.access_mode, &config.access_code);

    // Handshake manager
    let listener_host = config.external_host.as_deref().unwrap_or("0.0.0.0");
    let listener_url = format!("ws://{}:{}", listener_host, config.signaling_port);
    if listener_host == "0.0.0.0" {
        warn!("Invite listener_url uses 0.0.0.0 — set supernode_host env var for remote clients");
    }
    let mut handshake =
        HandshakeManager::new(identity.clone(), listener_url, config.invite_ttl_seconds);
    handshake.node_title = config.web_title.clone();
    // Embed TURN relay hint so clients can fall back to the relay port.
    if listener_host != "0.0.0.0" {
        handshake.turn_hints = Some(vec![format!(
            "turn:{}:{}",
            listener_host, config.relay_port
        )]);
    }

    // QUIC relay server
    let relay = {
        let relay = QUICRelayServer::new(identity.public_id());
        let bind = SocketAddr::from(([0, 0, 0, 0], config.relay_port));
        let port = relay.start(bind).await?;
        info!("QUIC relay on port {}", port);
        // Pre-authorize existing trusted peers
        for pid in peer_store.trusted_peer_ids() {
            relay.allow_peer(&pid);
        }
        Some(relay)
    };

    // SFU room manager
    let sfu = if config.sfu_enabled {
        let mut mgr = SFURoomManager::new();
        let rooms_path = config.data_dir.join("sfu_rooms.json");
        let loaded = mgr.load_rooms(&rooms_path);
        if loaded > 0 {
            info!("Loaded {} persisted SFU rooms", loaded);
        }
        Some(RwLock::new(mgr))
    } else {
        None
    };

    // Load endpoint mailbox from disk
    let endpoint_mailbox = SupernodeState::load_endpoint_mailbox(&config.data_dir);

    // Signaling server
    let signaling = SignalingServer::new(identity.public_id());

    // Build shared state using Arc::new_cyclic so feature modules can hold a
    // Weak<SupernodeState> without a circular strong-reference cycle.
    //
    // Generate/reuse the self-signed WebTransport cert before building state
    // so the fingerprint is available for SUPERNODE_INFO immediately.
    let web_cert_fingerprint = if config.web_port.is_some() {
        ensure_web_cert(&config.data_dir)
    } else {
        None
    };

    let state: Arc<SupernodeState> =
        Arc::new_cyclic(|_weak: &std::sync::Weak<SupernodeState>| SupernodeState {
            config: config.clone(),
            identity: identity.clone(),
            peer_store: RwLock::new(peer_store),
            handshake: RwLock::new(handshake),
            relay,
            sfu,
            signaling,
            access_controller,
            start_time: Instant::now(),
            ticket_expiry: RwLock::new(HashMap::new()),
            endpoint_mailbox: RwLock::new(endpoint_mailbox),
            pending_punches: RwLock::new(HashMap::new()),
            features: build_feature_registry(&config),
            web_bridge: BrowserBridge::new(),
            web_cert_fingerprint,
        });

    // Install the `web.host.app.v1` bidi-stream hook on the relay so the
    // embedded Chromium view in the desktop client can fetch
    // `conquerd://<supernode_pub>/<path>` assets from `<data_dir>/web/`
    // and `<data_dir>/games/` over the already-identity-verified QUIC
    // session. The hook is fire-and-forget; the module spawns its own
    // per-stream task with a deadline so a slow client cannot pin us.
    if state.features.get("web.host.app.v1").is_some() {
        if let Some(ref relay) = state.relay {
            let weak = std::sync::Arc::downgrade(&state);
            let data_dir = state.config.data_dir.clone();
            let module =
                std::sync::Arc::new(web_app_module::WebAppHostModule::new(weak, &data_dir));
            let hook: relay::BidiStreamHook = {
                let module = module.clone();
                std::sync::Arc::new(move |peer_id, send, recv| {
                    let module = module.clone();
                    tokio::spawn(async move {
                        module.handle_stream(peer_id, send, recv).await;
                    });
                })
            };
            relay.set_bidi_hook(hook);
            // Ensure the asset roots exist and seed default index pages so
            // the portal has something to show out of the box. Files are
            // only written if they do not already exist so operator
            // customisations are never overwritten.
            seed_web_defaults(&state.config.data_dir);
            info!(
                "[features] web.host.app.v1 serving assets from {}/web/, {}/games/ (example + shared-drawing + brick-breaker) and {}/web-sdk/",
                state.config.data_dir.display(),
                state.config.data_dir.display(),
                state.config.data_dir.display()
            );
        }
    }

    // Start WebTransport listener (stub) when the manifest enabled
    // `web.host.h3.v1`. The bridge is always allocated; the listener
    // task only spins up when the capability is registered.
    if state.features.get("web.host.h3.v1").is_some() {
        let bridge = state.web_bridge.clone();
        let port = state.config.web_port.unwrap_or(8443);
        let data_dir = state.config.data_dir.clone();

        // If the SFU is enabled, register `room.audio.sfu` and
        // `room.chat.v1` as `FeatureModule` instances on the registry,
        // then install `ModuleNativeDispatcher` so browser-originated
        // `room.*` payloads land in the registry exactly once per
        // inbound message. The modules own verify+enumerate+forward.
        if state.sfu.is_some() {
            let weak = std::sync::Arc::downgrade(&state);
            let audio: std::sync::Arc<dyn conquerd_features::FeatureModule> =
                std::sync::Arc::new(sfu_module::SfuRoomModule::audio(weak.clone()));
            let chat: std::sync::Arc<dyn conquerd_features::FeatureModule> =
                std::sync::Arc::new(sfu_module::SfuRoomModule::chat(weak));
            // Manifest-loaded descriptors usually exist already; bind
            // first, fall back to register if absent.
            if !state.features.bind_module("room.audio.sfu", audio.clone()) {
                let _ = state.features.register_module(audio);
            }
            if !state.features.bind_module("room.chat.v1", chat.clone()) {
                let _ = state.features.register_module(chat);
            }

            let native_hook: webtransport::NativeMessageHook = {
                let state_for_hook = std::sync::Arc::downgrade(&state);
                std::sync::Arc::new(move |source, feature_id, payload| {
                    if let Some(s) = state_for_hook.upgrade() {
                        s.features
                            .dispatch_message(feature_id, source.to_string(), payload);
                    }
                })
            };
            bridge.set_dispatcher(std::sync::Arc::new(
                webtransport::ModuleNativeDispatcher::new(native_hook),
            ));
        }

        tokio::spawn(async move {
            webtransport::run_listener(bridge, data_dir, port).await;
        });
    }

    // Start signaling
    let handler = Arc::new(SupernodeHandler {
        state: state.clone(),
    });
    let bind = SocketAddr::from(([0, 0, 0, 0], config.signaling_port));
    let sig_port = state.signaling.start(bind, handler).await?;
    info!("Signaling on port {}", sig_port);

    // Create and print invite (restore from disk if available)
    {
        let mut hs = state.handshake.write();
        if !hs.load_reusable_invite(&config.data_dir) {
            hs.get_or_create_reusable_invite(Some(&config.web_title));
            hs.save_reusable_invite(&config.data_dir);
        }
        let invite = hs.reusable_invite.as_ref().unwrap();
        let uri = invite.to_uri();
        info!("═══════════════════════════════════════");
        info!("Invite URL: {}", uri);
        info!("═══════════════════════════════════════");
    }

    // Ticket renewal timer
    let state_renewal = state.clone();
    tokio::spawn(async move {
        let mut interval =
            tokio::time::interval(std::time::Duration::from_secs(RENEWAL_CHECK_INTERVAL_S));
        loop {
            interval.tick().await;
            check_ticket_renewals(&state_renewal);
        }
    });

    // Wait for shutdown signal (SIGINT or SIGTERM)
    info!("Supernode running. Press Ctrl+C to stop.");
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm = signal(SignalKind::terminate())?;
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = sigterm.recv() => {},
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await?;
    info!("Shutting down...");

    // Cleanup
    if let Some(ref relay) = state.relay {
        relay.shutdown();
    }
    if let Some(ref sfu) = state.sfu {
        let rooms_path = state.config.data_dir.join("sfu_rooms.json");
        let _ = sfu.read().save_rooms(&rooms_path);
    }
    let _ = state.peer_store.read().save();

    info!("Goodbye.");
    Ok(())
}

/// Check and renew expiring relay tickets.
fn check_ticket_renewals(state: &SupernodeState) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();

    let expiries: Vec<(String, f64)> = state
        .ticket_expiry
        .read()
        .iter()
        .map(|(k, v)| (k.clone(), *v))
        .collect();

    for (peer_id, expires_at) in expiries {
        if !state.signaling.is_peer_connected(&peer_id) {
            continue;
        }
        if (expires_at - now) > ticket::RENEWAL_WINDOW_S {
            continue;
        }
        info!(
            "Renewing relay ticket for {}",
            &peer_id[..12.min(peer_id.len())]
        );
        state.issue_relay_ticket(&peer_id);
    }
}

/// Seed default web assets into `data_dir` on first run.
///
/// Files are embedded at compile time with `include_str!` so the binary
/// is self-contained.  Each file is only written if it does not already
/// exist, so operator customisations are never overwritten.
///
/// Directory layout seeded:
/// ```text
/// <data_dir>/
///   web/
///     index.html                     ← portal dashboard (uses window.conquerd bridge)
///   games/
///     example/
///       index.html                   ← cursor-relay demo UI
///       game.js                      ← cursor-relay demo logic
///     shared-drawing/
///       index.html                   ← collaborative canvas demo
///       drawing.js
///     brick-breaker/
///       index.html                   ← multiplayer breakout demo
///       brick-breaker.js
///   web-sdk/
///     conquerd.mjs                   ← browser SDK (imported by all game demos)
/// ```
///
/// The three game demos (`game.relay.v1`) are fully functional browser
/// experiences that connect via WebTransport when `web.host.h3.v1` is enabled.
fn seed_web_defaults(data_dir: &std::path::Path) {
    // Embedded assets — all paths are relative to this source file and
    // live inside the crate so the build works regardless of whether the
    // wider project root (games/, web-sdk/) is present (e.g. on Linux CI).
    const PORTAL_HTML: &str = include_str!("../templates/web_index.html");

    // Cursor relay (original example)
    const CURSOR_HTML: &str = include_str!("../templates/games_example_index.html");
    const CURSOR_JS: &str = include_str!("../templates/games_example_game.js");

    // Shared drawing demo
    const DRAW_HTML: &str = include_str!("../templates/games_shared_drawing_index.html");
    const DRAW_JS: &str = include_str!("../templates/games_shared_drawing_drawing.js");

    // Brick breaker demo
    const BRICK_HTML: &str = include_str!("../templates/games_brick_breaker_index.html");
    const BRICK_JS: &str = include_str!("../templates/games_brick_breaker_brick_breaker.js");

    const CONQUERD_MJS: &str = include_str!("../templates/web_sdk_conquerd.mjs");

    // Directories (always ensure they exist)
    let dirs: &[&[&str]] = &[
        &["web"],
        &["web", "web-sdk"],
        &["games", "example"],
        &["games", "shared-drawing"],
        &["games", "brick-breaker"],
    ];

    // Always-overwrite: all system-owned files must stay current with the
    // binary.  Content is compared first so unchanged files are not touched.
    // Operators add their own games under a different slug — they never
    // customise these first-party files directly.
    let always_update: &[(&[&str], &str)] = &[
        (&["web", "index.html"], PORTAL_HTML),
        // Served at /web-sdk/conquerd.mjs — must live under web/ so that
        // the web_app_module route() function finds it via the web_root.
        (&["web", "web-sdk", "conquerd.mjs"], CONQUERD_MJS),
        (&["games", "example", "index.html"], CURSOR_HTML),
        (&["games", "example", "game.js"], CURSOR_JS),
        (&["games", "shared-drawing", "index.html"], DRAW_HTML),
        (&["games", "shared-drawing", "drawing.js"], DRAW_JS),
        (&["games", "brick-breaker", "index.html"], BRICK_HTML),
        (&["games", "brick-breaker", "brick-breaker.js"], BRICK_JS),
    ];

    // No write-if-missing seeds remain; all built-in files are above.

    for parts in dirs {
        let full = data_dir.join(parts.iter().collect::<std::path::PathBuf>());
        if !full.exists() {
            if let Err(e) = std::fs::create_dir_all(&full) {
                warn!("[seed] could not create {}: {e}", full.display());
            }
        }
    }

    for (parts, content) in always_update {
        let full = data_dir.join(parts.iter().collect::<std::path::PathBuf>());
        if let Some(parent) = full.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::read_to_string(&full) {
            Ok(existing) if existing == *content => {} // unchanged — skip the write
            _ => {
                if let Err(e) = std::fs::write(&full, content.as_bytes()) {
                    warn!("[seed] could not write {}: {e}", full.display());
                } else {
                    info!("[seed] updated {}", full.display());
                }
            }
        }
    }
}

/// Ensure a self-signed TLS cert exists for the WebTransport listener.
///
/// Returns the SHA-256 fingerprint (lowercase hex) of the cert's DER bytes.
/// The fingerprint is the trust anchor: it is delivered to native clients via
/// the already Ed25519-verified `SUPERNODE_INFO` message, so no CA is needed.
///
/// Validity is capped at 13 days to stay within Chromium's 14-day maximum
/// for `serverCertificateHashes`.  The cert is rotated when it is older than
/// 7 days (the fingerprint file's mtime is used as a cheap age check).
///
/// **Always** re-derives the fingerprint from the on-disk cert DER rather
/// than reading the stored `.hex` file, so the advertised hash is never stale
/// (e.g. after a crash between the cert write and the fingerprint write, or
/// after a manual cert replacement).
fn ensure_web_cert(data_dir: &std::path::Path) -> Option<String> {
    use sha2::{Digest, Sha256};

    let cert_path = data_dir.join("web_cert.pem");
    let key_path = data_dir.join("web_key.pem");
    let fp_path = data_dir.join("web_cert_fingerprint.hex");

    // Helper: derive the fingerprint and DER bytes directly from the PEM
    // on disk so both are always authoritative (not dependent on .hex cache).
    let derive_fp_and_der_from_disk = || -> Option<(String, Vec<u8>)> {
        let pem = std::fs::read_to_string(&cert_path).ok()?;
        // Strip the PEM header/footer and decode the base64 body.
        let b64: String = pem
            .lines()
            .filter(|l| !l.starts_with("-----"))
            .collect::<Vec<_>>()
            .join("");
        use base64::Engine;
        let der = base64::engine::general_purpose::STANDARD
            .decode(&b64)
            .ok()?;
        let fp = hex::encode(Sha256::digest(&der));
        Some((fp, der))
    };

    // Reuse existing cert if it is still fresh (< 7 days old) AND it
    // carries the serverAuth EKU that Chrome WebTransport requires.
    // Certs generated before the EKU was added are silently rotated.
    let still_fresh = std::fs::metadata(&fp_path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|mtime| std::time::SystemTime::now().duration_since(mtime).ok())
        .map(|age| age.as_secs() < 7 * 24 * 3600)
        .unwrap_or(false);
    if still_fresh && cert_path.exists() && key_path.exists() {
        // Re-derive the fingerprint from the actual cert DER so the
        // advertised hash is always in sync with what wtransport presents.
        if let Some((fp, der)) = derive_fp_and_der_from_disk() {
            // Check for serverAuth EKU (OID 1.3.6.1.5.5.7.3.1).
            // DER encoding of this OID: 06 08 2b 06 01 05 05 07 03 01
            const SERVER_AUTH_OID: &[u8] =
                &[0x06, 0x08, 0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x01];
            let has_server_auth = der
                .windows(SERVER_AUTH_OID.len())
                .any(|w| w == SERVER_AUTH_OID);
            if has_server_auth {
                // Keep the .hex cache file up to date.
                let _ = std::fs::write(&fp_path, &fp);
                info!(
                    "[web-cert] reusing existing cert (fingerprint {}…)",
                    &fp[..16.min(fp.len())]
                );
                return Some(fp);
            }
            // Old cert missing serverAuth EKU — Chrome TLS rejects it.
            // Remove the files so we fall through to regeneration.
            warn!("[web-cert] existing cert missing serverAuth EKU — regenerating");
            let _ = std::fs::remove_file(&cert_path);
            let _ = std::fs::remove_file(&key_path);
            let _ = std::fs::remove_file(&fp_path);
        } else {
            // Cert PEM unreadable — fall through to regeneration.
            warn!("[web-cert] existing cert unreadable — regenerating");
        }
    }

    // Generate a fresh self-signed ECDSA cert valid for 13 days.
    use rcgen::{CertificateParams, ExtendedKeyUsagePurpose, KeyPair};

    let now = time::OffsetDateTime::now_utc();
    let mut params = CertificateParams::default();
    params.not_before = now;
    params.not_after = now + time::Duration::days(13);
    // Chromium WebTransport with serverCertificateHashes bypasses hostname
    // verification, but it still requires the certificate to be valid for
    // TLS server authentication.  Add the serverAuth EKU explicitly so
    // Chrome's TLS stack accepts the cert at the handshake level.
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];

    let key = match KeyPair::generate() {
        Ok(k) => k,
        Err(e) => {
            warn!("[web-cert] key generation failed: {e}");
            return None;
        }
    };
    let cert = match params.self_signed(&key) {
        Ok(c) => c,
        Err(e) => {
            warn!("[web-cert] cert generation failed: {e}");
            return None;
        }
    };

    let cert_pem = cert.pem();
    let key_pem = key.serialize_pem();
    let fingerprint = hex::encode(Sha256::digest(cert.der().as_ref()));

    if let Err(e) = std::fs::write(&cert_path, &cert_pem) {
        warn!("[web-cert] write cert failed: {e}");
        return None;
    }
    if let Err(e) = std::fs::write(&key_path, &key_pem) {
        warn!("[web-cert] write key failed: {e}");
        return None;
    }
    if let Err(e) = std::fs::write(&fp_path, &fingerprint) {
        warn!("[web-cert] write fingerprint failed: {e}");
        return None;
    }

    info!(
        "[web-cert] generated new self-signed cert (fingerprint {}…)",
        &fingerprint[..16.min(fingerprint.len())]
    );
    Some(fingerprint)
}
