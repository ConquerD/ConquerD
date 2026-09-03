//! Peer and room invite URL generation, parse, and handshake.

use std::time::Instant;

use serde_json::Value;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tracing::{info, warn};

use crate::protocol::{MessageType, SignalingMessage};

use super::super::events::ConnectionEvent;
use super::super::internal::PendingInvite;
use super::ConnectionManager;

use super::{parse_quic_lan_hint, unix_now_f64};

/// breaking change to [`build_room_invite_url`] / [`parse_room_invite`] and add
/// migration handling in the parser.
pub const ROOM_INVITE_SCHEMA: u32 = 1;

/// URL-level freshness guard for a shared room invite (24h). The supernode's
/// own token TTL is authoritative; this just stops stale links from dialing.
pub const ROOM_INVITE_TTL_SECS: u64 = 24 * 60 * 60;

/// Decoded fields of a `conquerd://room#…` invite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomInvitePayload {
    pub supernode_id: String,
    pub supernode_hint: String,
    pub room_id: String,
    pub room_name: String,
    pub room_type: String,
    pub invite_token: String,
    pub expires_at: u64,
    /// Space-tree proof-based admission fields, each a JSON object as text;
    /// empty when the inviter didn't include one. Carried to the joiner, who
    /// forwards them on `SfuJoin` for the supernode to verify.
    pub space_root: String,
    pub space_proof: String,
    pub space_grant: String,
}

/// A pasted room invite awaiting its host supernode's WebSocket to connect.
#[derive(Debug, Clone)]
pub(super) struct RoomInviteEntry {
    pub room_id: String,
    pub room_name: String,
    pub room_type: String,
    pub invite_token: String,
    /// Space-tree parent node id (from the invite's inclusion proof) and the
    /// owning Space id (from its signed root). `""` for legacy/flat invites.
    pub parent_id: String,
    pub space_id: String,
}

/// Build a self-contained room invite URL: `conquerd://room#<base64url(JSON)>`.
///
/// Kept as a free function (separate from the `ConnectionManager` state) so the
/// wire format can be round-trip tested in isolation. See the golden field test
/// in `tests.rs`; any field rename here must update that test in lock-step.
#[allow(clippy::too_many_arguments)]
pub fn build_room_invite_url(
    supernode_id: &str,
    supernode_hint: &str,
    room_id: &str,
    room_name: &str,
    room_type: &str,
    invite_token: &str,
    expires_at: u64,
    space_root: &str,
    space_proof: &str,
    space_grant: &str,
) -> String {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    let mut payload = serde_json::json!({
        "v": ROOM_INVITE_SCHEMA,
        "supernode_id": supernode_id,
        "supernode_hint": supernode_hint,
        "room_id": room_id,
        "room_name": room_name,
        "room_type": room_type,
        "invite_token": invite_token,
        "expires_at": expires_at,
    });
    // Embed the Space fields as nested JSON objects (not strings) when present,
    // so the joiner deserializes them straight into the space types. The owner
    // signatures are over the struct fields, so a JSON round-trip is safe.
    if let Some(obj) = payload.as_object_mut() {
        for (key, text) in [
            ("space_root", space_root),
            ("space_proof", space_proof),
            ("space_grant", space_grant),
        ] {
            if !text.is_empty() {
                if let Ok(v) = serde_json::from_str::<Value>(text) {
                    obj.insert(key.to_owned(), v);
                }
            }
        }
    }
    let encoded = URL_SAFE_NO_PAD.encode(payload.to_string().as_bytes());
    format!("conquerd://room#{encoded}")
}

/// Parse the base64url fragment of a `conquerd://room#…` invite (the part after
/// `room#`). Returns an error string suitable for `emit_invite_failed`.
pub fn parse_room_invite(encoded: &str) -> Result<RoomInvitePayload, String> {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;

    if encoded.len() > 262_144 {
        return Err(format!("room invite too large ({} bytes)", encoded.len()));
    }
    let json_bytes = URL_SAFE_NO_PAD
        .decode(encoded.trim_end_matches('='))
        .map_err(|e| format!("base64 decode error: {e}"))?;
    let payload: serde_json::Value =
        serde_json::from_slice(&json_bytes).map_err(|e| format!("JSON parse error: {e}"))?;

    // Unknown future schema: refuse rather than silently misinterpret.
    if let Some(v) = payload.get("v").and_then(Value::as_u64) {
        if v > ROOM_INVITE_SCHEMA as u64 {
            return Err(format!("unsupported room invite version {v}"));
        }
    }

    let get = |k: &str| {
        payload
            .get(k)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    let supernode_id = get("supernode_id");
    let room_id = get("room_id");
    if supernode_id.is_empty() {
        return Err("room invite missing supernode_id".into());
    }
    if room_id.is_empty() {
        return Err("room invite missing room_id".into());
    }
    // `room_type` is additive within v1; invites minted before it existed were
    // always private, so that's the back-compat default.
    let room_type = match payload.get("room_type").and_then(Value::as_str) {
        Some(t) if !t.is_empty() => t.to_owned(),
        _ => "private".to_owned(),
    };
    // Space fields: extract the nested objects back to JSON text ("" = absent).
    let get_obj = |k: &str| {
        payload
            .get(k)
            .filter(|v| v.is_object())
            .map(|v| v.to_string())
            .unwrap_or_default()
    };
    Ok(RoomInvitePayload {
        supernode_id,
        supernode_hint: get("supernode_hint"),
        room_id,
        room_name: get("room_name"),
        room_type,
        invite_token: get("invite_token"),
        expires_at: payload
            .get("expires_at")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        space_root: get_obj("space_root"),
        space_proof: get_obj("space_proof"),
        space_grant: get_obj("space_grant"),
    })
}

impl ConnectionManager {
    pub(in crate::connection_manager) fn generate_invite_url(&mut self) -> Option<String> {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use base64::Engine;

        // Best-effort local QUIC listener for LAN dials. Invites still work
        // without it when both peers share a supernode (relayed INIT/ACCEPT).
        let _ = self.ensure_quic_endpoint(0);
        let lan_hint = self.local_quic_hint().unwrap_or_default();

        let invite_id = uuid::Uuid::new_v4().to_string();
        let expires_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
            + 900;
        let inviter_handle = super::peer_session::read_local_display_handle();
        // X25519 ephemeral public is required by AcceptInvite (session-key /
        // transcript binding). Peer invites previously omitted it, so every
        // personal invite failed closed with "missing inviter_ephemeral_pub".
        let inviter_eph = crate::crypto::generate_ephemeral_keypair();
        let inviter_ephemeral_pub =
            crate::crypto::b64url_encode_nopad(inviter_eph.public.as_bytes());
        let mut payload = serde_json::json!({
            "inviter_peer_id": self.identity.peer_id(),
            "inviter_identity_pub": self.identity.public_id(),
            "invite_id": invite_id,
            "expires_at": expires_at,
            "inviter_ephemeral_pub": inviter_ephemeral_pub,
            // Peers list label on the joiner before/without a HandleUpdate.
            "inviter_handle": inviter_handle,
        });
        if !lan_hint.is_empty() {
            if let Some(obj) = payload.as_object_mut() {
                obj.insert("lan_hint".to_owned(), serde_json::Value::String(lan_hint));
            }
        }
        let encoded = URL_SAFE_NO_PAD.encode(payload.to_string().as_bytes());
        Some(format!("conquerd://invite#{encoded}"))
    }

    /// Build a self-contained room invite URL for a room hosted on
    /// `supernode_id`. Returns `None` if we don't know a signaling address for
    /// that supernode (so we can fall back to sharing the bare token).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn generate_room_invite_url(
        &self,
        supernode_id: &str,
        room_id: &str,
        room_name: &str,
        room_type: &str,
        invite_token: &str,
        space_root: &str,
        space_proof: &str,
        space_grant: &str,
    ) -> Option<String> {
        if supernode_id.is_empty() || room_id.is_empty() {
            return None;
        }
        // Prefer the live session's ws_url; fall back to a persisted relay hint
        // (e.g. the room was created earlier this session but the socket churned).
        let supernode_hint = self
            .supernodes
            .get(supernode_id)
            .map(|sn| sn.ws_url.clone())
            .or_else(|| {
                self.peer_store
                    .read()
                    .get(supernode_id)
                    .and_then(|r| r.relay_hints.first().cloned())
            })
            .filter(|h| !h.is_empty())?;
        let expires_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
            + ROOM_INVITE_TTL_SECS;
        Some(build_room_invite_url(
            supernode_id,
            &supernode_hint,
            room_id,
            room_name,
            room_type,
            invite_token,
            expires_at,
            space_root,
            space_proof,
            space_grant,
        ))
    }

    pub(super) fn emit_invite_failed(&self, reason: impl Into<String>) {
        let reason = reason.into();
        warn!("AcceptInvite: {reason}");
        self.emit_event(ConnectionEvent::InviteFailed { reason });
    }

    pub(super) fn build_invite_handshake_init(
        &self,
        pending: &PendingInvite,
        target: String,
    ) -> SignalingMessage {
        let sender = self.identity.public_id();
        let joiner_peer_id = self.identity.peer_id();
        let joiner_eph = crate::crypto::generate_ephemeral_keypair();
        let joiner_ephemeral_pub = crate::crypto::b64url_encode_nopad(joiner_eph.public.as_bytes());
        let joiner_quic_port = self
            .quic_endpoint
            .as_ref()
            .and_then(|ep| ep.local_addr().ok())
            .map(|addr| addr.port())
            .unwrap_or(0);

        let joiner_handle = super::peer_session::read_local_display_handle();
        let mut msg = SignalingMessage::new(MessageType::InviteHandshakeInit, sender.clone());
        msg.target = Some(target);
        msg.payload
            .insert("invite_id".into(), Value::String(pending.invite_id.clone()));
        msg.payload
            .insert("joiner_identity_pub".into(), Value::String(sender.clone()));
        msg.payload
            .insert("joiner_peer_id".into(), Value::String(joiner_peer_id));
        msg.payload.insert(
            "joiner_ephemeral_pub".into(),
            Value::String(joiner_ephemeral_pub),
        );
        msg.payload.insert(
            "joiner_quic_port".into(),
            Value::Number(joiner_quic_port.into()),
        );
        if !joiner_handle.is_empty() {
            msg.payload
                .insert("joiner_handle".into(), Value::String(joiner_handle));
        }
        if let Some(hint) = self.local_quic_hint() {
            msg.payload
                .insert("joiner_lan_hint".into(), Value::String(hint));
        }
        msg
    }

    pub(super) async fn send_pending_invite_inits_for_peer(&mut self, peer_id: &str) {
        let invite_ids: Vec<String> = self
            .pending_invites
            .iter()
            .filter(|(_, pending)| !pending.is_supernode && pending.inviter_peer_id == peer_id)
            .map(|(invite_id, _)| invite_id.clone())
            .collect();

        for invite_id in invite_ids {
            let Some(pending) = self.pending_invites.get(&invite_id) else {
                continue;
            };
            let msg = self.build_invite_handshake_init(pending, peer_id.to_owned());
            self.dispatch_outbound(msg).await;
        }
    }

    /// Accept a pasted self-contained room invite: connect to the embedded
    /// host supernode (if not already), then hand the room off to the UI to
    /// join. `encoded` is the base64url fragment after `room#`.
    pub(super) async fn handle_accept_room_invite(&mut self, encoded: &str) {
        let payload = match parse_room_invite(encoded) {
            Ok(p) => p,
            Err(e) => {
                self.emit_invite_failed(e);
                return;
            }
        };

        if payload.expires_at != 0 {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            if payload.expires_at < now {
                self.emit_invite_failed("room invite expired");
                return;
            }
        }

        let RoomInvitePayload {
            supernode_id,
            supernode_hint,
            room_id,
            room_name,
            room_type,
            invite_token,
            space_root,
            space_proof,
            space_grant,
            ..
        } = payload;

        // Pull the Space-tree linkage out of the proof/root before they're moved
        // into the pending join creds, so the joiner's sidebar can nest the room:
        // `parent_id` is the room's parent node in the owner's tree (a room id, or
        // "default"/the Server node for a top-level room); `space_id` names the
        // owning Space. Absent for legacy flat invites → "".
        let space_parent_id = serde_json::from_str::<Value>(&space_proof)
            .ok()
            .and_then(|v| {
                v.get("node")
                    .and_then(|n| n.get("parent_id"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or_default();
        let space_tree_id = serde_json::from_str::<Value>(&space_root)
            .ok()
            .and_then(|v| v.get("space_id").and_then(Value::as_str).map(str::to_owned))
            .unwrap_or_default();

        // Stash any Space proof-based admission creds from the invite; they are
        // attached (single-use) to the SfuJoin for this room so the supernode can
        // admit + materialize it by proof on any cluster member.
        if !space_proof.is_empty() {
            self.pending_join_space_creds
                .insert(room_id.clone(), (space_root, space_proof, space_grant));
        }

        info!(
            "Accepting room invite for room {} on supernode {}",
            &room_id[..12.min(room_id.len())],
            &supernode_id[..8.min(supernode_id.len())]
        );

        // Persist the host supernode so the room-store join path (which resolves
        // the supernode via the peer store) can find it, and so it survives a
        // restart / shows in the Nodes tab. Mirrors the supernode-invite path.
        if !supernode_hint.is_empty() {
            let mut store = self.peer_store.write();
            store.upsert_from_invite(crate::peer_store::PeerRecord {
                peer_id: supernode_id.clone(),
                identity_pub: supernode_id.clone(),
                relay_hints: vec![supernode_hint.clone()],
                is_supernode: true,
                supernode_from_invite: true,
                created_at: unix_now_f64(),
                last_seen_at: unix_now_f64(),
                ..Default::default()
            });
            let _ = store.save();
        }

        let entry = RoomInviteEntry {
            room_id,
            room_name,
            room_type,
            invite_token,
            parent_id: space_parent_id,
            space_id: space_tree_id,
        };

        let connected = self
            .supernodes
            .get(&supernode_id)
            .map(|sn| sn.connected)
            .unwrap_or(false);

        if connected {
            // Link is already up — enter the room immediately.
            self.emit_room_invite_ready(&supernode_id, &entry);
        } else {
            // Stash until WsConnected fires; open the session if we have no
            // task for this supernode yet.
            if !self.supernodes.contains_key(&supernode_id) {
                if supernode_hint.is_empty() {
                    self.emit_invite_failed("room invite missing supernode address");
                    return;
                }
                self.connect_supernode_ws(supernode_id.clone(), vec![supernode_hint.clone()])
                    .await;
            }
            self.pending_room_invite_entries.insert(supernode_id, entry);
        }
    }

    pub(super) fn emit_room_invite_ready(&self, supernode_id: &str, entry: &RoomInviteEntry) {
        self.emit_event(ConnectionEvent::RoomInviteReady {
            supernode_id: supernode_id.to_owned(),
            room_id: entry.room_id.clone(),
            room_name: entry.room_name.clone(),
            room_type: entry.room_type.clone(),
            invite_token: entry.invite_token.clone(),
            parent_id: entry.parent_id.clone(),
            space_id: entry.space_id.clone(),
        });
    }

    pub(in crate::connection_manager) async fn handle_accept_invite(&mut self, invite_url: String) {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use base64::Engine;

        const SCHEME: &str = "conquerd://";
        let Some(rest) = invite_url.strip_prefix(SCHEME) else {
            self.emit_invite_failed(format!("invalid scheme in '{invite_url}'"));
            return;
        };

        // Invite URLs carry an optional `action#` prefix before the base64url
        // fragment: `conquerd://invite#<b64>`, `conquerd://room#<b64>`, or the
        // bare legacy `conquerd://<b64>`. Split it off so the payload decodes.
        let (action, encoded) = match rest.split_once('#') {
            Some((action, payload)) => (action, payload),
            None => ("", rest),
        };

        if action == "room" {
            self.handle_accept_room_invite(encoded).await;
            return;
        }

        if encoded.len() > 262_144 {
            self.emit_invite_failed(format!("invite URL too large ({} bytes)", encoded.len()));
            return;
        }

        let json_bytes = match URL_SAFE_NO_PAD.decode(encoded.trim_end_matches('=')) {
            Ok(b) => b,
            Err(e) => {
                self.emit_invite_failed(format!("base64 decode error: {e}"));
                return;
            }
        };

        let payload: serde_json::Value = match serde_json::from_slice(&json_bytes) {
            Ok(v) => v,
            Err(e) => {
                self.emit_invite_failed(format!("JSON parse error: {e}"));
                return;
            }
        };

        let inviter_identity_pub = match payload.get("inviter_identity_pub").and_then(Value::as_str)
        {
            Some(s) => s.to_owned(),
            None => {
                self.emit_invite_failed("missing inviter_identity_pub");
                return;
            }
        };
        let inviter_peer_id = payload
            .get("inviter_peer_id")
            .and_then(Value::as_str)
            .unwrap_or(&inviter_identity_pub)
            .to_owned();
        let invite_id = payload
            .get("invite_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let relay_hint = payload
            .get("relay_hint")
            .and_then(Value::as_str)
            .map(|s| s.to_owned())
            .unwrap_or_default();
        let lan_hint = payload
            .get("lan_hint")
            .and_then(Value::as_str)
            .map(|s| s.to_owned())
            .unwrap_or_default();
        let supernode_hint = if relay_hint.is_empty() {
            lan_hint.clone()
        } else {
            relay_hint.clone()
        };
        let is_supernode = payload
            .get("is_supernode")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let inviter_ephemeral_pub = payload
            .get("inviter_ephemeral_pub")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();

        if invite_id.is_empty() {
            self.emit_invite_failed("missing invite_id");
            return;
        }
        if inviter_identity_pub == self.identity.public_id() {
            self.emit_invite_failed("cannot use own invite");
            return;
        }

        if let Some(expires_at) = payload.get("expires_at").and_then(Value::as_i64) {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            if expires_at < now {
                self.emit_invite_failed("invite expired");
                return;
            }
        }

        let inviter_handle = payload
            .get("inviter_handle")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();

        info!(
            "Accepting invite from {} (id={})",
            &inviter_identity_pub[..8.min(inviter_identity_pub.len())],
            &invite_id[..8.min(invite_id.len())]
        );

        // Supernode invites: trust + persist immediately from the signed URL
        // payload so the Rooms sidebar updates even if the WS handshake is slow
        // or the supernode no longer has this invite_id in its pending map.
        if is_supernode {
            {
                let mut store = self.peer_store.write();
                let relay_hints = if supernode_hint.is_empty() {
                    vec![]
                } else {
                    vec![supernode_hint.clone()]
                };
                store.upsert_from_invite(crate::peer_store::PeerRecord {
                    peer_id: inviter_peer_id.clone(),
                    identity_pub: inviter_identity_pub.clone(),
                    handle: inviter_handle.clone(),
                    relay_hints,
                    is_supernode: true,
                    supernode_from_invite: true,
                    created_at: unix_now_f64(),
                    last_seen_at: unix_now_f64(),
                    ..Default::default()
                });
                let _ = store.save();
            }
            self.emit_event(ConnectionEvent::InviteAccepted {
                peer_id: inviter_peer_id.clone(),
                handle: inviter_handle.clone(),
            });
        }

        // Store pending invite (matched when INVITE_HANDSHAKE_ACCEPT arrives)
        self.pending_invites.insert(
            invite_id.clone(),
            PendingInvite {
                inviter_peer_id: inviter_peer_id.clone(),
                inviter_identity_pub: inviter_identity_pub.clone(),
                invite_id: invite_id.clone(),
                relay_hint: supernode_hint.clone(),
                lan_hint: lan_hint.clone(),
                is_supernode,
                created_at: Instant::now(),
            },
        );

        // Open a signaling session only for supernode invites. Ordinary peers
        // may carry a ws relay hint for NAT traversal — that must not register
        // them in the Rooms sidebar or key a WS session under their identity.
        if is_supernode && !supernode_hint.is_empty() {
            if let Some(sn) = self.supernodes.remove(&inviter_identity_pub) {
                sn.ws_task.abort();
            }
            self.connect_supernode_ws(inviter_identity_pub.clone(), vec![supernode_hint.clone()])
                .await;
        }

        if !is_supernode {
            if inviter_ephemeral_pub.is_empty() {
                self.emit_invite_failed(
                    "invite missing inviter_ephemeral_pub; generate a fresh invite",
                );
                return;
            }

            // Personal peer invites prefer direct QUIC (LAN hint on the invite).
            // When both peers already share a supernode (common for room users),
            // also send INVITE_HANDSHAKE_INIT over the supernode WS relay so trust
            // completes even if LAN QUIC is firewalled, wrong, or missing. The
            // supernode indexes sockets by identity public_id, so the target is
            // inviter_identity_pub — not the hex peer_id used for QUIC sessions.
            let mut attempted = false;
            if let Some((host, port)) = parse_quic_lan_hint(&lan_hint) {
                self.connect_direct_quic(&inviter_peer_id, &host, port)
                    .await;
                attempted = true;
            }

            let supernode_available = self.supernodes.values().any(|sn| sn.connected);
            if supernode_available {
                if let Some(pending) = self.pending_invites.get(&invite_id).cloned() {
                    // Same INIT shape as the post-QUIC path, but targeted at the
                    // inviter's identity public_id so the supernode can relay it.
                    let msg =
                        self.build_invite_handshake_init(&pending, inviter_identity_pub.clone());
                    info!(
                        "Peer invite: sending InviteHandshakeInit via supernode relay to {}",
                        &inviter_identity_pub[..8.min(inviter_identity_pub.len())]
                    );
                    self.dispatch_outbound(msg).await;
                    attempted = true;
                }
            }

            if !attempted {
                self.emit_invite_failed(
                    "invite has no reachable path (no local QUIC hint and no shared supernode online); \
                     generate a fresh invite while both are online on the same supernode, or on the same LAN",
                );
            }
            return;
        }

        // Build + sign INVITE_HANDSHAKE_INIT and queue directly on the WS send
        // channel (the message will be delivered once the WS connection is up).
        let sender = self.identity.public_id();
        let joiner_peer_id = self.identity.peer_id();
        let joiner_eph = crate::crypto::generate_ephemeral_keypair();
        let joiner_ephemeral_pub = crate::crypto::b64url_encode_nopad(joiner_eph.public.as_bytes());
        if inviter_ephemeral_pub.is_empty() {
            self.emit_invite_failed(
                "invite missing inviter_ephemeral_pub; generate a fresh invite",
            );
            return;
        }
        if let Err(e) = crate::crypto::derive_invite_session_key(
            &joiner_eph.secret,
            &inviter_ephemeral_pub,
            &invite_id,
            &inviter_identity_pub,
            &sender,
            &joiner_ephemeral_pub,
        ) {
            warn!("AcceptInvite: session key derivation failed: {e}");
        }
        let joiner_quic_port = self
            .quic_endpoint
            .as_ref()
            .and_then(|ep| ep.local_addr().ok())
            .map(|addr| addr.port())
            .unwrap_or(0);
        let joiner_handle = super::peer_session::read_local_display_handle();
        let mut msg = SignalingMessage::new(MessageType::InviteHandshakeInit, sender.clone());
        msg.target = Some(inviter_identity_pub.clone());
        msg.payload
            .insert("invite_id".into(), Value::String(invite_id));
        msg.payload
            .insert("joiner_identity_pub".into(), Value::String(sender.clone()));
        msg.payload
            .insert("joiner_peer_id".into(), Value::String(joiner_peer_id));
        msg.payload.insert(
            "joiner_ephemeral_pub".into(),
            Value::String(joiner_ephemeral_pub),
        );
        msg.payload.insert(
            "joiner_quic_port".into(),
            Value::Number(joiner_quic_port.into()),
        );
        if !joiner_handle.is_empty() {
            msg.payload
                .insert("joiner_handle".into(), Value::String(joiner_handle));
        }
        if let Some(hint) = self.local_quic_hint() {
            msg.payload
                .insert("joiner_lan_hint".into(), Value::String(hint));
        }

        if let Ok(canonical) = msg.canonical_bytes() {
            let sig = self.identity.sign(&canonical);
            use base64::Engine;
            msg.signature = Some(base64::engine::general_purpose::URL_SAFE.encode(sig));
        }

        if let Ok(json) = msg.to_json() {
            if let Some(sn) = self.supernodes.get(&inviter_identity_pub) {
                if sn.send_tx.try_send(WsMessage::Text(json)).is_err() {
                    self.note_ws_outbound_drop("invite handshake accept");
                }
            } else {
                warn!("AcceptInvite: no WS session for inviter — message dropped");
            }
        }
    }
}
