//! Translating [`ConnectionEvent`] into the JSON the Kotlin layer consumes.
//!
//! Every event becomes `{"event": "<snake_case_name>", ...fields}`. The names
//! are written out literally here rather than derived from the Rust variant
//! names, so that renaming a variant cannot silently reshape the wire the app
//! is built against.

use conquerd_client::connection_manager::ConnectionEvent;
use serde_json::{json, Value};

/// Render a core event as JSON, or `None` when it must not cross the JNI
/// boundary at all.
///
/// The `None` cases are the real-time media frames. They arrive hundreds of
/// times a second carrying raw payload bytes, and the pipelines that consume
/// them (audio playout, video decode) live on the Rust side — base64-encoding
/// them into JSON for a UI with no use for them would burn CPU on every frame
/// of every call.
pub fn to_json(event: &ConnectionEvent) -> Option<Value> {
    use ConnectionEvent as E;

    let value = match event {
        // ── Real-time media: handled inside Rust, never forwarded ──────────
        E::SfuAudioReceived { .. }
        | E::DirectAudioReceived { .. }
        | E::VideoFrameReceived { .. }
        | E::ContentAudioReceived { .. }
        | E::PortalGameDatagram { .. } => return None,

        // ── Peers and sessions ────────────────────────────────────────────
        E::PeerConnected(peer_id) => json!({ "event": "peer_connected", "peer_id": peer_id }),
        E::PeerDisconnected(peer_id) => {
            json!({ "event": "peer_disconnected", "peer_id": peer_id })
        }
        E::PresenceUpdated { peer_id, status } => {
            json!({ "event": "presence_updated", "peer_id": peer_id, "status": status })
        }
        E::HandleUpdated { peer_id, handle } => {
            json!({ "event": "handle_updated", "peer_id": peer_id, "handle": handle })
        }
        E::AvatarConfigUpdated { peer_id } => {
            json!({ "event": "avatar_config_updated", "peer_id": peer_id })
        }
        E::EndpointUpdated { peer_id, endpoints } => {
            json!({ "event": "endpoint_updated", "peer_id": peer_id, "endpoints": endpoints })
        }
        E::SessionStateUpdate(state) => json!({
            "event": "session_state",
            "peer_id": state.peer_id,
            // These enums carry no Serialize impl and no string form, so the
            // Debug rendering is the name. Lowercased to match the casing of
            // every other string field on the wire.
            "chat_path": debug_name(&state.chat_path),
            "chat_health": debug_name(&state.chat_health),
            "voice_mode": debug_name(&state.voice_mode),
            "voice_quality": debug_name(&state.voice_quality),
            "in_call": state.in_call,
            "muted": state.muted,
            "speaking": state.speaking,
            "rtt_ms": state.rtt_ms,
            "packet_loss": state.packet_loss,
            "jitter_ms": state.jitter_ms,
            "relay_url": state.relay_url,
            "relay_index": state.relay_index,
        }),
        E::ConnectionStats { peer_id, json } => json!({
            "event": "connection_stats",
            "peer_id": peer_id,
            // Already-encoded JSON from the manager; reparse so the UI gets a
            // real object instead of a string holding one.
            "stats": parse_or_string(json),
        }),
        E::SignalingMessage(msg) => json!({
            "event": "signaling",
            "msg_type": format!("{:?}", msg.msg_type),
            "sender": msg.sender,
        }),

        // ── Direct chat ───────────────────────────────────────────────────
        E::ChatMessage {
            peer_id,
            message_id,
            body,
            timestamp,
            sender_handle,
        } => json!({
            "event": "chat_message",
            "peer_id": peer_id,
            "message_id": message_id,
            "body": body,
            "timestamp": timestamp,
            "sender_handle": sender_handle,
        }),
        E::ChatAck {
            peer_id,
            message_id,
        } => json!({ "event": "chat_ack", "peer_id": peer_id, "message_id": message_id }),
        E::ChatSendFailed {
            peer_id,
            message_id,
            reason,
        } => json!({
            "event": "chat_send_failed",
            "peer_id": peer_id,
            "message_id": message_id,
            "reason": reason,
        }),
        E::TypingIndicator { peer_id, is_typing } => {
            json!({ "event": "typing", "peer_id": peer_id, "is_typing": is_typing })
        }

        // ── Calls ─────────────────────────────────────────────────────────
        E::CallRequest {
            peer_id,
            fallback_supernode_id,
            fallback_room_id,
            fallback_invite_token,
        } => json!({
            "event": "call_request",
            "peer_id": peer_id,
            "fallback_supernode_id": fallback_supernode_id,
            "fallback_room_id": fallback_room_id,
            "fallback_invite_token": fallback_invite_token,
        }),
        E::CallFallbackRoomReady {
            peer_id,
            supernode_id,
            room_id,
        } => json!({
            "event": "call_fallback_room_ready",
            "peer_id": peer_id,
            "supernode_id": supernode_id,
            "room_id": room_id,
        }),
        E::CallAccepted { peer_id } => json!({ "event": "call_accepted", "peer_id": peer_id }),
        E::CallEnded { peer_id } => json!({ "event": "call_ended", "peer_id": peer_id }),
        E::PeerVideoStateChanged { peer_id, active } => {
            json!({ "event": "peer_video_state", "peer_id": peer_id, "active": active })
        }
        E::VideoKeyframeRequested { peer_id } => {
            json!({ "event": "video_keyframe_requested", "peer_id": peer_id })
        }

        // ── Supernodes and relay ──────────────────────────────────────────
        E::SupernodeConnected(id) => json!({ "event": "supernode_connected", "supernode_id": id }),
        E::SupernodeDisconnected(id) => {
            json!({ "event": "supernode_disconnected", "supernode_id": id })
        }
        E::RelayGranted {
            supernode_id,
            relay_host,
            relay_port,
            portal_only,
            // The ticket is a bearer credential for the relay. Rust holds it;
            // there is no reason for it to sit in a Kotlin string.
            ticket: _,
        } => json!({
            "event": "relay_granted",
            "supernode_id": supernode_id,
            "relay_host": relay_host,
            "relay_port": relay_port,
            "portal_only": portal_only,
        }),
        E::RelayPaymentRequired {
            supernode_id,
            portal_url,
        } => json!({
            "event": "relay_payment_required",
            "supernode_id": supernode_id,
            "portal_url": portal_url,
        }),
        E::SupernodeInfoReceived {
            supernode_id,
            homepage_url,
            title,
            sfu_enabled,
            public_rooms_enabled,
        } => json!({
            "event": "supernode_info",
            "supernode_id": supernode_id,
            "homepage_url": homepage_url,
            "title": title,
            "sfu_enabled": sfu_enabled,
            "public_rooms_enabled": public_rooms_enabled,
        }),
        E::ClusterMembersUpdated {
            supernode_id,
            members,
        } => json!({
            "event": "cluster_members",
            "supernode_id": supernode_id,
            "members": members,
        }),

        // ── Rooms ─────────────────────────────────────────────────────────
        E::RoomFailedOver {
            supernode_id,
            room_id,
        } => json!({
            "event": "room_failed_over",
            "supernode_id": supernode_id,
            "room_id": room_id,
        }),
        E::RoomMembersChanged {
            supernode_id,
            room_id,
            members,
            chat_members,
        } => json!({
            "event": "room_members_changed",
            "supernode_id": supernode_id,
            "room_id": room_id,
            "members": members,
            "chat_members": chat_members,
        }),
        E::RoomJoinRejected {
            supernode_id,
            room_id,
            reason,
        } => json!({
            "event": "room_join_rejected",
            "supernode_id": supernode_id,
            "room_id": room_id,
            "reason": reason,
        }),
        E::RoomPeerJoined {
            supernode_id,
            room_id,
            peer_id,
        } => json!({
            "event": "room_peer_joined",
            "supernode_id": supernode_id,
            "room_id": room_id,
            "peer_id": peer_id,
        }),
        E::RoomPeerLeft {
            supernode_id,
            room_id,
            peer_id,
        } => json!({
            "event": "room_peer_left",
            "supernode_id": supernode_id,
            "room_id": room_id,
            "peer_id": peer_id,
        }),
        E::RoomChatMessage {
            supernode_id,
            room_id,
            sender_id,
            sender_handle,
            body,
            timestamp,
            message_id,
        } => json!({
            "event": "room_chat_message",
            "supernode_id": supernode_id,
            "room_id": room_id,
            "sender_id": sender_id,
            "sender_handle": sender_handle,
            "body": body,
            "timestamp": timestamp,
            "message_id": message_id,
        }),
        E::RoomListReceived {
            supernode_id,
            rooms_json,
        } => json!({
            "event": "room_list",
            "supernode_id": supernode_id,
            "rooms": parse_or_string(rooms_json),
        }),
        E::RoomCreated {
            supernode_id,
            room_id,
            room_name,
            room_type,
            invite_token,
        } => json!({
            "event": "room_created",
            "supernode_id": supernode_id,
            "room_id": room_id,
            "room_name": room_name,
            "room_type": room_type,
            "invite_token": invite_token,
        }),
        E::RoomInviteReady {
            supernode_id,
            room_id,
            room_name,
            room_type,
            invite_token,
            parent_id,
            space_id,
        } => json!({
            "event": "room_invite_ready",
            "supernode_id": supernode_id,
            "room_id": room_id,
            "room_name": room_name,
            "room_type": room_type,
            "invite_token": invite_token,
            "parent_id": parent_id,
            "space_id": space_id,
        }),

        // ── Invites ───────────────────────────────────────────────────────
        E::InviteAccepted { peer_id, handle } => {
            json!({ "event": "invite_accepted", "peer_id": peer_id, "handle": handle })
        }
        E::InviteFailed { reason } => json!({ "event": "invite_failed", "reason": reason }),

        // ── Capabilities ──────────────────────────────────────────────────
        E::CapabilityAnnounced { peer_id, caps_json } => json!({
            "event": "capability_announced",
            "peer_id": peer_id,
            "capabilities": parse_or_string(caps_json),
        }),
        E::CapabilityInvoked {
            peer_id,
            feature_id,
            params,
        } => json!({
            "event": "capability_invoked",
            "peer_id": peer_id,
            "feature_id": feature_id,
            "params": params,
        }),
        E::CapabilityInvokePending {
            peer_id,
            feature_id,
            params,
        } => json!({
            "event": "capability_invoke_pending",
            "peer_id": peer_id,
            "feature_id": feature_id,
            "params": params,
        }),

        // ── File transfer ─────────────────────────────────────────────────
        E::FileOffered {
            transfer_id,
            peer_id,
            rel_path,
            size,
            purpose,
            is_self,
            origin_id,
            supernode_id,
        } => json!({
            "event": "file_offered",
            "transfer_id": transfer_id,
            "peer_id": peer_id,
            "rel_path": rel_path,
            "size": size,
            "purpose": purpose,
            "is_self": is_self,
            "origin_id": origin_id,
            "supernode_id": supernode_id,
        }),
        E::FileProgress {
            transfer_id,
            progress,
        } => json!({
            "event": "file_progress",
            "transfer_id": transfer_id,
            "progress": progress,
        }),
        E::FileComplete {
            transfer_id,
            peer_id,
            room_id,
            supernode_id,
            purpose,
            rel_path,
            // The payload is the file bytes or an on-disk handle; the UI is
            // told the transfer finished and where, not handed the contents.
            payload: _,
        } => json!({
            "event": "file_complete",
            "transfer_id": transfer_id,
            "peer_id": peer_id,
            "room_id": room_id,
            "supernode_id": supernode_id,
            "purpose": purpose,
            "rel_path": rel_path,
        }),
        E::FileFailed {
            transfer_id,
            reason,
        } => json!({ "event": "file_failed", "transfer_id": transfer_id, "reason": reason }),
    };

    Some(value)
}

/// Lowercased `Debug` name of a fieldless enum value.
fn debug_name<T: std::fmt::Debug>(value: &T) -> String {
    format!("{value:?}").to_lowercase()
}

/// Reparse a string that already holds JSON, falling back to the raw string
/// when it does not — a malformed payload should reach the UI as data rather
/// than collapse the whole event.
fn parse_or_string(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_owned()))
}
