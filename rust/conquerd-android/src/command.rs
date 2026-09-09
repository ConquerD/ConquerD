//! The JSON command surface Kotlin calls into.
//!
//! One entry point — [`dispatch`] — takes `{"cmd": "<name>", ...args}` and
//! returns `{"ok": true, ...}` or `{"ok": false, "error": "..."}`. A single
//! channel rather than one JNI method per action: the desktop bridge exposes
//! close to a hundred invokables, and mirroring each as its own `native`
//! declaration would mean regenerating and re-checking signatures on both
//! sides of the boundary every time one changes.

use std::sync::mpsc as std_mpsc;
use std::sync::Arc;
use std::time::Duration;

use conquerd_client::call_controller::CallCommand;
use conquerd_client::chat_store::{ChatMessage, MessageKind, MessageStatus};
use conquerd_client::connection_manager::ConnectionCommand;
use conquerd_client::protocol::{MessageType, SignalingMessage};
use serde_json::{json, Value};
use tracing::{info, warn};

use crate::session::{attachment_label, Session};

/// Every command [`dispatch`] answers, for the unknown-command error.
///
/// Kept next to the dispatcher so the two are edited together; it is a
/// diagnostic aid, not a source of truth - the match arms are.
const KNOWN_COMMANDS: &[&str] = &[
    "identity.info",
    "identity.export_key",
    "identity.set_handle",
    "avatar.svg",
    "avatar.set_config",
    "peer.list",
    "peer.block",
    "peer.unblock",
    "peer.remove",
    "chat.history",
    "chat.send",
    "chat.mark_read",
    "chat.unread_total",
    "chat.typing",
    "chat.delete",
    "chat.retry",
    "chat.purge_all",
    "chat.trim",
    "invite.generate",
    "invite.accept",
    "room.list",
    "room.create",
    "room.hide",
    "room.unhide",
    "room.join",
    "room.leave",
    "room.chat.subscribe",
    "room.chat.unsubscribe",
    "room.chat.send",
    "room.voice.join",
    "room.voice.leave",
    "room.history",
    "room.request_list",
    "call.start",
    "call.accept",
    "call.reject",
    "call.end",
    "video.start",
    "video.stop",
    "audio.start",
    "audio.stop",
    "audio.set_muted",
    "file.send",
    "file.accept",
    "file.reject",
    "file.cancel",
];

/// How long a command that waits on the core may block the calling thread.
///
/// Kotlin calls `nativeCommand` off the main thread, but a hung core must
/// still not pin that thread forever.
const REPLY_TIMEOUT: Duration = Duration::from_secs(10);

/// Split a raw request into its command name and body.
///
/// Separate from [`dispatch`] so the validation half can be tested without a
/// running core behind it.
fn parse_request(request: &str) -> Result<(String, Value), Value> {
    let parsed: Value =
        serde_json::from_str(request).map_err(|e| err(format!("malformed command JSON: {e}")))?;

    match parsed.get("cmd").and_then(Value::as_str) {
        Some(cmd) => Ok((cmd.to_owned(), parsed)),
        None => Err(err("command is missing a \"cmd\" field")),
    }
}

/// Handle one command, always returning a JSON object (never an error).
pub fn dispatch(session: &Session, request: &str) -> Value {
    let (cmd, parsed) = match parse_request(request) {
        Ok(pair) => pair,
        Err(reply) => return reply,
    };

    match cmd.as_str() {
        // ── Identity ──────────────────────────────────────────────────────
        "identity.info" => json!({
            "ok": true,
            "public_id": session.my_public_id,
            "peer_id": session.identity.peer_id(),
            "fingerprint": session.identity.fingerprint(),
            "handle": session
                .peer_store
                .read()
                .get(&session.identity.peer_id())
                .map(|r| r.handle.clone())
                .unwrap_or_default(),
        }),

        // Hand the identity file key to Kotlin so it can be sealed in the
        // Android Keystore. Only called when the user ticked "stay unlocked":
        // nothing here decides that policy, and nothing stores the key on the
        // Rust side. The reply crosses one in-process JNI boundary inside the
        // app's own sandbox - it never touches a log, an event, or the disk.
        "identity.export_key" => json!({
            "ok": true,
            "key": conquerd_client::crypto::b64url_encode(&session.identity_key),
        }),

        // Set the name peers see for us.
        //
        // Stored on our own record in the peer store rather than a settings
        // file: that is where every outbound message already reads the sender
        // handle from, so one write covers chat, invites and the peer list.
        "identity.set_handle" => {
            let handle = arg_str(&parsed, "handle").unwrap_or_default().trim().to_owned();
            if handle.chars().count() > 64 {
                return err("that name is too long");
            }

            let my_peer_id = session.identity.peer_id();
            {
                let mut store = session.peer_store.write();
                match store.get_mut(&my_peer_id) {
                    Some(record) => record.handle = handle.clone(),
                    None => {
                        // First run on this device: our own record does not
                        // exist until something writes it.
                        let mut record = conquerd_client::peer_store::PeerRecord {
                            peer_id: my_peer_id.clone(),
                            identity_pub: session.my_public_id.clone(),
                            handle: handle.clone(),
                            ..Default::default()
                        };
                        record.created_at = now_secs();
                        store.upsert(record);
                    }
                }
                if let Err(e) = store.save() {
                    return err(format!("could not save the name: {e}"));
                }
            }

            // Peers keep their own copy of our handle, so a rename that is not
            // announced leaves everyone else showing the old one.
            if !handle.is_empty() {
                let _ = session.send(ConnectionCommand::BroadcastHandleUpdateToAll {
                    handle: handle.clone(),
                });
            }
            json!({ "ok": true, "handle": handle })
        }

        // Change how our own avatar looks, and tell peers.
        //
        // Stored on our own peer record, the same place the handle lives and
        // the same place `avatar.svg` already reads a peer's config from, so
        // one write covers the peer list, chat and our own preview. Peers
        // cache it, so a change that is not announced leaves them rendering
        // the old one.
        "avatar.set_config" => {
            let Some(config_json) = arg_str(&parsed, "config") else {
                return err("avatar.set_config requires \"config\"");
            };

            // Parse before storing: a config the renderer cannot read would
            // leave every avatar of ours blank until it was set again.
            let config: conquerd_client::avatar_config::AvatarConfig =
                match serde_json::from_str(config_json) {
                    Ok(c) => c,
                    Err(e) => return err(format!("that avatar config is not valid: {e}")),
                };

            let my_peer_id = session.identity.peer_id();
            {
                let mut store = session.peer_store.write();
                match store.get_mut(&my_peer_id) {
                    Some(record) => record.avatar_config = Some(config),
                    None => {
                        let mut record = conquerd_client::peer_store::PeerRecord {
                            peer_id: my_peer_id.clone(),
                            identity_pub: session.my_public_id.clone(),
                            avatar_config: Some(config),
                            ..Default::default()
                        };
                        record.created_at = now_secs();
                        store.upsert(record);
                    }
                }
                if let Err(e) = store.save() {
                    return err(format!("could not save the avatar: {e}"));
                }
            }

            let _ = session.send(ConnectionCommand::BroadcastAvatarConfigToAll {
                config_json: config_json.to_owned(),
            });
            json!({ "ok": true })
        }

        // Render a peer's identicon.
        //
        // The SVG is built by the shared core, not reimplemented here: the
        // colour rules (islands, dual-hue modes, shade modes) are intricate
        // enough that a second implementation would drift, and an avatar that
        // differs between a peer's desktop and phone is worse than none.
        "avatar.svg" => {
            let Some(raw_id) = arg_str(&parsed, "peer_id") else {
                return err("avatar.svg requires \"peer_id\"");
            };

            let my_public_id = session.my_public_id.clone();
            let my_peer_id = session.identity.peer_id();
            let store = session.peer_store.read();

            // Avatars are seeded from identity_pub; the UI passes whatever id
            // it has, which for a peer row is the hex peer_id.
            let seed = conquerd_client::avatar_config::avatar_seed_id(
                raw_id,
                &my_public_id,
                &my_peer_id,
                |raw| {
                    store
                        .get(raw)
                        .or_else(|| store.get_by_identity(raw))
                        .map(|rec| rec.identity_pub.clone())
                        .filter(|pub_id| !pub_id.is_empty())
                },
            );

            // An explicit config previews an edit before it is saved; without
            // one, a peer who advertised their own look gets it and everyone
            // else the factory default, matching the desktop's fallback.
            let config = match arg_str(&parsed, "config").filter(|c| !c.is_empty()) {
                Some(json) => match serde_json::from_str(json) {
                    Ok(c) => c,
                    Err(e) => return err(format!("that avatar config is not valid: {e}")),
                },
                None => store
                    .get(raw_id)
                    .or_else(|| store.get_by_identity(raw_id))
                    .and_then(|rec| rec.avatar_config.clone())
                    .unwrap_or_default(),
            };

            json!({
                "ok": true,
                "svg": conquerd_client::avatar_config::build_avatar_svg(&seed, &config),
                "tint": conquerd_client::avatar_config::avatar_tint_hex(&seed, &config),
            })
        }

        // ── Peers ─────────────────────────────────────────────────────────
        "peer.list" => {
            let store = session.peer_store.read();
            let peers: Vec<Value> = store
                .list_peers()
                .into_iter()
                .map(|p| {
                    let mut v = serde_json::to_value(p).unwrap_or_else(|_| json!({}));
                    // `display_name` is a method, not a field, so it is not in
                    // the serialised record — but it is what the UI shows.
                    if let Some(obj) = v.as_object_mut() {
                        obj.insert("display_name".into(), json!(p.display_name()));
                    }
                    v
                })
                .collect();
            json!({ "ok": true, "peers": peers })
        }
        "peer.block" | "peer.unblock" => {
            let Some(peer_id) = arg_str(&parsed, "peer_id") else {
                return err("peer.block requires \"peer_id\"");
            };
            let command = if cmd == "peer.block" {
                ConnectionCommand::BlockPeer {
                    peer_id: peer_id.to_owned(),
                }
            } else {
                ConnectionCommand::UnblockPeer {
                    peer_id: peer_id.to_owned(),
                }
            };
            queued(session.send(command))
        }
        "peer.remove" => {
            let Some(peer_id) = arg_str(&parsed, "peer_id") else {
                return err("peer.remove requires \"peer_id\"");
            };

            // Same order as the desktop's removePeer: forget the record first,
            // then tear down any call with them - a peer that is gone from the
            // store but still holding a live audio session is the one state
            // the UI cannot represent.
            let removed = {
                let mut store = session.peer_store.write();
                let removed = store.remove_by_any_id(peer_id).is_some();
                if removed {
                    if let Err(e) = store.save() {
                        return err(format!("peer removed but the store would not save: {e}"));
                    }
                }
                removed
            };

            if !removed {
                return err("no such peer");
            }

            let _ = session.call_tx.try_send(CallCommand::RemovePeer {
                peer_id: peer_id.to_owned(),
            });
            json!({ "ok": true })
        }

        // ── Direct chat ───────────────────────────────────────────────────
        "chat.history" => {
            let Some(peer_id) = arg_str(&parsed, "peer_id") else {
                return err("chat.history requires \"peer_id\"");
            };
            let page = parsed.get("page").and_then(Value::as_u64).unwrap_or(0) as usize;
            match session.chat_store.get_history(peer_id, page) {
                Ok(messages) => json!({
                    "ok": true,
                    "messages": serde_json::to_value(messages).unwrap_or_else(|_| json!([])),
                }),
                Err(e) => err(format!("could not read history: {e}")),
            }
        }
        "chat.send" => send_chat(session, &parsed),
        "chat.delete" => {
            let Some(msg_id) = arg_str(&parsed, "message_id") else {
                return err("chat.delete requires \"message_id\"");
            };
            match session.chat_store.delete_message(msg_id) {
                Ok(()) => json!({ "ok": true }),
                Err(e) => err(format!("could not delete the message: {e}")),
            }
        }
        // Local retention. None of this reaches the peer: they keep their own
        // copy, and the protocol has no "delete for everyone".
        "chat.purge_all" => match session.chat_store.purge_all() {
            Ok(removed) => json!({ "ok": true, "removed": removed }),
            Err(e) => err(format!("could not purge history: {e}")),
        },
        "chat.trim" => {
            // One command with two modes rather than two commands: they are
            // the same user intent - "keep less" - and a caller that sends
            // neither bound has asked for nothing.
            let days = parsed.get("days").and_then(Value::as_i64);
            let keep = parsed.get("keep_per_peer").and_then(Value::as_i64);
            if days.is_none() && keep.is_none() {
                return err("chat.trim needs \"days\" or \"keep_per_peer\"");
            }

            let mut removed = 0usize;
            if let Some(days) = days {
                match session.chat_store.trim_by_age(days as i32) {
                    Ok(n) => removed += n,
                    Err(e) => return err(format!("could not trim by age: {e}")),
                }
            }
            if let Some(keep) = keep {
                match session.chat_store.trim_by_count(keep as i32) {
                    Ok(n) => removed += n,
                    Err(e) => return err(format!("could not trim by count: {e}")),
                }
            }
            json!({ "ok": true, "removed": removed })
        }
        "chat.retry" => {
            let Some(msg_id) = arg_str(&parsed, "message_id") else {
                return err("chat.retry requires \"message_id\"");
            };
            retry_chat(session, msg_id)
        }
        "chat.mark_read" => {
            let Some(peer_id) = arg_str(&parsed, "peer_id") else {
                return err("chat.mark_read requires \"peer_id\"");
            };
            match session.chat_store.mark_peer_read(peer_id) {
                Ok(count) => json!({ "ok": true, "marked": count }),
                Err(e) => err(format!("could not mark read: {e}")),
            }
        }
        "chat.unread_total" => match session.chat_store.total_unread_count() {
            Ok(count) => json!({ "ok": true, "unread": count }),
            Err(e) => err(format!("could not count unread: {e}")),
        },
        "chat.typing" => {
            let Some(peer_id) = arg_str(&parsed, "peer_id") else {
                return err("chat.typing requires \"peer_id\"");
            };
            let is_typing = parsed
                .get("is_typing")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            queued(session.send(ConnectionCommand::SendTyping {
                peer_id: peer_id.to_owned(),
                is_typing,
            }))
        }

        // ── Invites ───────────────────────────────────────────────────────
        "invite.generate" => generate_invite(session),
        "invite.accept" => {
            let Some(invite_url) = arg_str(&parsed, "invite_url") else {
                return err("invite.accept requires \"invite_url\"");
            };
            queued(session.send(ConnectionCommand::AcceptInvite {
                invite_url: invite_url.to_owned(),
            }))
        }

        // ── Rooms ─────────────────────────────────────────────────────────
        "room.list" => {
            // A cluster presents as one logical supernode, so the same room is
            // routinely filed under several member ids after a failover.
            // `list_for_cluster_members` is the store's own answer to that: it
            // resolves ids through the peer store, normalises base64 padding,
            // and dedupes by room_id. Using the raw `list()` shows one row per
            // member instead of one per room.
            let members = session.known_supernode_ids();
            let store = session.room_store.read();
            let peers = session.peer_store.read();

            let entries: Vec<conquerd_client::room_store::RoomEntry> = if members.is_empty() {
                // Before any supernode has reported a roster there is nothing
                // to resolve against, so fall back to the flat list.
                store.list().into_iter().cloned().collect()
            } else {
                store.list_for_cluster_members(&peers, &members)
            };

            let rooms: Vec<Value> = entries
                .into_iter()
                .map(|entry| {
                    // Hide state is recorded against whichever member was
                    // hosting when the user hid it, so a single-key check
                    // misses it once the room moves.
                    let hidden = store.is_hidden_from_sidebar(&entry.supernode_id, &entry.room_id)
                        || members
                            .iter()
                            .any(|id| store.is_hidden_from_sidebar(id, &entry.room_id));

                    let mut value = serde_json::to_value(&entry).unwrap_or_else(|_| json!({}));
                    if let Some(object) = value.as_object_mut() {
                        object.insert("hidden".into(), json!(hidden));
                    }
                    value
                })
                .collect();

            // Low-frequency (a list refresh), and the three numbers together
            // are what distinguishes "the filter is wrong" from "the store
            // really holds that many".
            info!(
                "room.list: {} stored -> {} after cluster dedupe, {} hidden, {} shown",
                store.len(),
                rooms.len(),
                rooms.iter().filter(|r| r["hidden"] == json!(true)).count(),
                rooms.iter().filter(|r| r["hidden"] != json!(true)).count(),
            );

            json!({ "ok": true, "rooms": rooms })
        }
        "room.hide" | "room.unhide" => {
            let (Some(supernode_id), Some(room_id)) = (
                arg_str(&parsed, "supernode_id"),
                arg_str(&parsed, "room_id"),
            ) else {
                return err("hiding a room requires \"supernode_id\" and \"room_id\"");
            };

            // Both directions sweep every cluster member, not just the node
            // hosting right now: a failover re-keys the room to a sibling, so
            // a tombstone written against one member would still be matching
            // after the room moved. Hiding writes them all; un-hiding has to
            // clear them all or the room reappears hidden after failover.
            let members = session.known_supernode_ids();
            let mut store = session.room_store.write();
            let hiding = cmd == "room.hide";

            // The room's own host first, so a failure is reported before the
            // sweep rather than buried in it.
            let primary = if hiding {
                store.hide_from_sidebar(supernode_id, room_id)
            } else {
                store.unhide_from_sidebar(supernode_id, room_id)
            };
            if let Err(e) = primary {
                return err(format!("could not update the room: {e}"));
            }

            for id in members.iter().filter(|id| id.as_str() != supernode_id) {
                let _ = if hiding {
                    store.hide_from_sidebar(id, room_id)
                } else {
                    store.unhide_from_sidebar(id, room_id)
                };
            }
            json!({ "ok": true })
        }
        "room.join" => {
            let (Some(supernode_id), Some(room_id)) = (
                arg_str(&parsed, "supernode_id"),
                arg_str(&parsed, "room_id"),
            ) else {
                return err("room.join requires \"supernode_id\" and \"room_id\"");
            };
            // Whether to spend an invite is not "do we have a token" - the
            // desktop decides with `should_use_private_room_invite`, and using
            // anything else here re-spends a single-use token on rooms that do
            // not need one (a public room, or one we created), which is what
            // previously blocked re-entry to private rooms.
            let entry = session
                .room_store
                .read()
                .list()
                .into_iter()
                .find(|e| e.room_id == room_id)
                .cloned();

            let token = entry
                .as_ref()
                .map(|e| e.invite_token.clone())
                .unwrap_or_default();
            let use_invite = conquerd_client::connection_manager::should_use_private_room_invite(
                false,
                entry.as_ref().is_some_and(|e| e.room_type == "private"),
                entry.as_ref().is_some_and(|e| e.is_creator),
                !token.is_empty(),
            );

            let command = if use_invite {
                ConnectionCommand::JoinRoomWithInvite {
                    supernode_id: supernode_id.to_owned(),
                    room_id: room_id.to_owned(),
                    invite_token: token,
                }
            } else {
                ConnectionCommand::JoinRoom {
                    supernode_id: supernode_id.to_owned(),
                    room_id: room_id.to_owned(),
                }
            };
            queued(session.send(command))
        }
        "room.leave" => {
            let (Some(supernode_id), Some(room_id)) = (
                arg_str(&parsed, "supernode_id"),
                arg_str(&parsed, "room_id"),
            ) else {
                return err("room.leave requires \"supernode_id\" and \"room_id\"");
            };
            queued(session.send(ConnectionCommand::LeaveRoom {
                supernode_id: supernode_id.to_owned(),
                room_id: room_id.to_owned(),
            }))
        }
        "room.chat.subscribe" | "room.chat.unsubscribe" => {
            let (Some(supernode_id), Some(room_id)) = (
                arg_str(&parsed, "supernode_id"),
                arg_str(&parsed, "room_id"),
            ) else {
                return err("room chat subscription requires \"supernode_id\" and \"room_id\"");
            };
            let command = if cmd == "room.chat.subscribe" {
                ConnectionCommand::SubscribeRoomChat {
                    supernode_id: supernode_id.to_owned(),
                    room_id: room_id.to_owned(),
                }
            } else {
                ConnectionCommand::UnsubscribeRoomChat {
                    supernode_id: supernode_id.to_owned(),
                    room_id: room_id.to_owned(),
                }
            };
            queued(session.send(command))
        }
        "room.chat.send" => send_room_chat(session, &parsed),

        // ── Room voice ────────────────────────────────────────────────────
        //
        // Joining a room's *chat* and joining its *voice* are separate acts:
        // room mode redirects outbound Opus through the supernode instead of
        // to individual QUIC peers, so it has to be set before capture starts
        // or the first frames go to the wrong place.
        "room.voice.join" => {
            let (Some(supernode_id), Some(room_id)) = (
                arg_str(&parsed, "supernode_id"),
                arg_str(&parsed, "room_id"),
            ) else {
                return err("room.voice.join requires \"supernode_id\" and \"room_id\"");
            };
            let voice_activation = parsed
                .get("voice_activation")
                .and_then(Value::as_bool)
                .unwrap_or(true);

            let mode_set = session
                .call_tx
                .try_send(CallCommand::SetRoomMode {
                    supernode_id: supernode_id.to_owned(),
                    room_id: room_id.to_owned(),
                })
                .is_ok();
            let audio_started = session
                .call_tx
                .try_send(CallCommand::StartAudio { voice_activation })
                .is_ok();

            if mode_set && audio_started {
                json!({ "ok": true })
            } else {
                err("could not start room audio")
            }
        }
        "room.voice.leave" => {
            // Clear room mode before stopping audio so no stray frame is
            // routed to a peer on the way down.
            let cleared = session.call_tx.try_send(CallCommand::ClearRoomMode).is_ok();
            let _ = session.call_tx.try_send(CallCommand::StopAudio);
            queued(cleared)
        }
        "room.history" => {
            let Some(room_id) = arg_str(&parsed, "room_id") else {
                return err("room.history requires \"room_id\"");
            };
            let page = parsed.get("page").and_then(Value::as_u64).unwrap_or(0) as usize;
            // Same conversation key the desktop writes, so a room's history is
            // one thread across both clients. No supernode in it: the room id
            // already identifies the room independently of its host.
            let key = conquerd_client::chat_store::room_conversation_id(room_id);
            match session.chat_store.get_history(&key, page) {
                Ok(messages) => json!({
                    "ok": true,
                    "messages": serde_json::to_value(messages).unwrap_or_else(|_| json!([])),
                }),
                Err(e) => err(format!("could not read room history: {e}")),
            }
        }
        "room.create" => {
            let Some(supernode_id) = arg_str(&parsed, "supernode_id") else {
                return err("room.create requires \"supernode_id\"");
            };
            let room_name = arg_str(&parsed, "room_name").unwrap_or_default().trim();
            if room_name.is_empty() {
                return err("a room needs a name");
            }

            // Same normalisation as the desktop: anything that is not
            // explicitly private is public, so a typo cannot silently produce
            // a room with weaker access than the user asked for.
            let room_type = match arg_str(&parsed, "room_type")
                .unwrap_or("public")
                .trim()
                .to_ascii_lowercase()
                .as_str()
            {
                "private" => "private",
                _ => "public",
            };

            // The room is persisted when the supernode answers with
            // `RoomCreated` - see `persist_if_room_created`. Nothing is written
            // here, so a create that never lands leaves no phantom room behind.
            queued(session.send(ConnectionCommand::CreateRoom {
                supernode_id: supernode_id.to_owned(),
                room_name: room_name.to_owned(),
                room_type: room_type.to_owned(),
                room_id: None,
                creator_id: None,
                materialize_only: false,
                invite_policy: "owner".to_owned(),
                invite_token: String::new(),
            }))
        }
        "room.request_list" => {
            let Some(supernode_id) = arg_str(&parsed, "supernode_id") else {
                return err("room.request_list requires \"supernode_id\"");
            };
            queued(session.send(ConnectionCommand::RequestRoomList {
                supernode_id: supernode_id.to_owned(),
            }))
        }

        // ── File transfer ─────────────────────────────────────────────────
        //
        // Kotlin hands over a path inside the app sandbox, not the SAF
        // `content://` uri it was given: the core streams the file lazily from
        // disk over the whole transfer, and a content uri's lifetime is the
        // picker's, not ours.
        "file.send" => send_file(session, &parsed),
        "file.accept" | "file.reject" | "file.cancel" => {
            let Some(transfer_id) = arg_str(&parsed, "transfer_id") else {
                return err("a file action requires \"transfer_id\"");
            };
            let transfer_id = transfer_id.to_owned();
            let command = match cmd.as_str() {
                "file.accept" => ConnectionCommand::AcceptFile { transfer_id },
                "file.reject" => ConnectionCommand::RejectFile { transfer_id },
                _ => ConnectionCommand::CancelFile { transfer_id },
            };
            queued(session.send(command))
        }

        // ── Calls ─────────────────────────────────────────────────────────
        //
        // These mirror the desktop bridge's start/accept/reject/end exactly:
        // a signed signaling message to the peer, plus the local audio
        // pipeline commands. Diverging here would make a phone-to-desktop call
        // behave differently from a desktop-to-desktop one.
        "call.start" | "call.accept" => {
            let Some(peer_id) = arg_str(&parsed, "peer_id") else {
                return err("a call needs \"peer_id\"");
            };
            let voice_activation = parsed
                .get("voice_activation")
                .and_then(Value::as_bool)
                .unwrap_or(true);

            let kind = if cmd == "call.start" {
                MessageType::CallRequest
            } else {
                MessageType::CallAccept
            };
            if !send_signal(session, kind, peer_id) {
                return err("could not reach the connection manager");
            }

            let audio_started = session
                .call_tx
                .try_send(CallCommand::StartAudio { voice_activation })
                .is_ok();
            let peer_added = session
                .call_tx
                .try_send(CallCommand::InitiatePeer {
                    peer_id: peer_id.to_owned(),
                    host: None,
                    port: None,
                })
                .is_ok();

            // The signal is already gone, so report partial failure rather
            // than pretending the call is up: the peer will be ringing.
            if audio_started && peer_added {
                json!({ "ok": true })
            } else {
                err("the call was signalled but local audio did not start")
            }
        }
        "call.reject" => {
            let Some(peer_id) = arg_str(&parsed, "peer_id") else {
                return err("call.reject requires \"peer_id\"");
            };
            queued(send_signal(session, MessageType::CallReject, peer_id))
        }
        "call.end" => {
            let Some(peer_id) = arg_str(&parsed, "peer_id") else {
                return err("call.end requires \"peer_id\"");
            };
            // Tell the peer first: a hang-up that only stops local audio
            // leaves the other side ringing or listening to silence.
            let signalled = send_signal(session, MessageType::CallEnd, peer_id);
            let _ = session.call_tx.try_send(CallCommand::RemovePeer {
                peer_id: peer_id.to_owned(),
            });
            let _ = session.call_tx.try_send(CallCommand::StopAudio);
            queued(signalled)
        }

        // ── Video ─────────────────────────────────────────────────────────
        //
        // Capture is driven by CameraX on the Kotlin side; this opens the
        // consumer end. `AndroidCamera::open` waits for the first frame, so
        // the camera must already be bound when this is called.
        "video.start" => {
            if session.video.read().is_some() {
                return err("video capture is already running");
            }

            let peer_id = arg_str(&parsed, "peer_id")
                .filter(|id| !id.is_empty())
                .map(str::to_owned);
            let device_id = arg_str(&parsed, "device_id").unwrap_or("android:front");
            let quality = arg_str(&parsed, "quality").unwrap_or("balanced");

            match crate::video::start(
                session.cmd_tx.clone(),
                peer_id.clone(),
                device_id,
                quality,
                Arc::clone(&session.video),
                session.sink.clone(),
            ) {
                Ok(sender) => {
                    *session.video.write() = Some(sender);
                    // Tell the far end to expect frames. Without this a peer
                    // shows no tile at all, because a video stream that was
                    // never announced is indistinguishable from none.
                    // `direct_peer` None means "announce to the room".
                    let _ = session.send(ConnectionCommand::SendVideoState {
                        active: true,
                        direct_peer: peer_id,
                    });
                    json!({ "ok": true })
                }
                Err(e) => err(format!("could not start video: {e}")),
            }
        }
        "video.stop" => {
            let stopped = session.video.write().take();
            let announced = session.send(ConnectionCommand::SendVideoState {
                active: false,
                direct_peer: arg_str(&parsed, "peer_id")
                    .filter(|id| !id.is_empty())
                    .map(str::to_owned),
            });
            match stopped {
                Some(sender) => {
                    sender.stop();
                    queued(announced)
                }
                // Idempotent: a UI that lost track of state should be able to
                // ask for "off" without getting an error.
                None => json!({ "ok": true }),
            }
        }

        // ── Audio ─────────────────────────────────────────────────────────
        "audio.start" => {
            let voice_activation = parsed
                .get("voice_activation")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            queued(
                session
                    .call_tx
                    .try_send(CallCommand::StartAudio { voice_activation })
                    .is_ok(),
            )
        }
        "audio.stop" => queued(session.call_tx.try_send(CallCommand::StopAudio).is_ok()),
        "audio.set_muted" => {
            let muted = parsed
                .get("muted")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            queued(
                session
                    .call_tx
                    .try_send(conquerd_client::call_controller::CallCommand::SetMuted(
                        muted,
                    ))
                    .is_ok(),
            )
        }

        // Listing the surface here turns a missing match arm - which is
        // otherwise indistinguishable from a client-side typo - into a
        // one-glance diagnosis.
        other => err(format!(
            "unknown command: {other} (known: {})",
            KNOWN_COMMANDS.join(", ")
        )),
    }
}

/// Send a bare signed signaling message to one peer.
///
/// Call control carries no payload beyond its type and target, so every one of
/// them is this same shape.
fn send_signal(session: &Session, kind: MessageType, peer_id: &str) -> bool {
    let mut msg = SignalingMessage::new(kind, session.my_public_id.clone());
    msg.target = Some(peer_id.to_owned());
    session.send(ConnectionCommand::SendMessage(msg))
}

/// Author, persist, and send a direct chat message.
///
/// The message is written to the store as `Sending` before it goes out, so it
/// appears in history immediately and a later ack or failure updates the row
/// that is already there.
/// Offer a file to a peer, echoing it into our own chat history.
///
/// Mirrors the desktop's `sendFile`: the path is handed over rather than the
/// bytes, so a 250 MB file is streamed from disk instead of held in memory,
/// and the local bubble is keyed `xfer-{transfer_id}` so the offer can be
/// found again later.
fn send_file(session: &Session, parsed: &Value) -> Value {
    let (Some(peer_id), Some(path)) = (arg_str(parsed, "peer_id"), arg_str(parsed, "path")) else {
        return err("file.send requires \"peer_id\" and \"path\"");
    };

    // The display name is the picked document's name, which need not match the
    // sandbox file Kotlin copied it into.
    let rel_path = arg_str(parsed, "rel_path")
        .filter(|n| !n.is_empty())
        .unwrap_or("file")
        .to_owned();

    let byte_len = match std::fs::metadata(path) {
        Ok(m) => m.len(),
        Err(e) => return err(format!("cannot read that file: {e}")),
    };
    if byte_len > conquerd_client::file_transfer::MAX_TRANSFER_SIZE as u64 {
        return err(format!(
            "{} is over the {} limit",
            conquerd_client::chat_store::format_byte_size(byte_len),
            conquerd_client::chat_store::format_byte_size(
                conquerd_client::file_transfer::MAX_TRANSFER_SIZE as u64
            ),
        ));
    }

    let transfer_id = uuid::Uuid::new_v4().simple().to_string()[..16].to_owned();
    let sent = session.send(ConnectionCommand::SendFile {
        peer_id: peer_id.to_owned(),
        rel_path: rel_path.clone(),
        path: path.to_owned(),
        transfer_id: transfer_id.clone(),
        purpose: "file".to_owned(),
    });

    let kind = conquerd_client::chat_store::message_kind_for_path(&rel_path);
    let record = ChatMessage {
        id: format!("xfer-{transfer_id}"),
        peer_id: peer_id.to_owned(),
        sender: session.my_public_id.clone(),
        recipient: peer_id.to_owned(),
        body: attachment_label(&kind, &rel_path),
        timestamp: now_secs(),
        is_self: true,
        status: if sent {
            MessageStatus::Sent
        } else {
            MessageStatus::Failed
        },
        kind,
        attachment_name: rel_path.clone(),
        attachment_path: path.to_owned(),
        size_str: conquerd_client::chat_store::format_byte_size(byte_len),
        status_note: String::new(),
        sender_handle: String::new(),
    };
    if let Err(e) = session.chat_store.upsert(&record) {
        warn!("could not echo the file offer into history: {e}");
    }

    json!({ "ok": sent, "transfer_id": transfer_id })
}

/// Re-send a message that failed, keeping its original id.
///
/// Same shape as the desktop's `retryMessage`: the id is reused so the peer
/// deduplicates a message that did arrive, and the stored status is moved to
/// whatever the second attempt achieved rather than being left on "failed".
fn retry_chat(session: &Session, msg_id: &str) -> Value {
    let stored = match session.chat_store.get_by_id(msg_id) {
        Ok(Some(msg)) => msg,
        Ok(None) => return err("no such message"),
        Err(e) => return err(format!("could not read the message: {e}")),
    };

    // Only our own text messages can be re-sent: an inbound message has no
    // outbound form, and a file or system entry is not a chat body.
    if !stored.is_self || stored.peer_id.is_empty() || stored.kind != MessageKind::Text {
        return err("only your own text messages can be retried");
    }

    let mut outbound =
        SignalingMessage::new(MessageType::ChatMessage, session.my_public_id.clone());
    outbound.target = Some(stored.peer_id.clone());
    outbound
        .payload
        .insert("body".into(), Value::String(stored.body.clone()));
    outbound
        .payload
        .insert("message_id".into(), Value::String(stored.id.clone()));
    outbound.payload.insert(
        "sender_handle".into(),
        Value::String(stored.sender_handle.clone()),
    );

    let sent = session.send(ConnectionCommand::SendMessage(outbound));
    let status = if sent {
        MessageStatus::Sending
    } else {
        MessageStatus::Failed
    };

    if let Err(e) = session
        .chat_store
        .update_status_note(msg_id, status.clone(), "")
    {
        return err(format!("resent, but the status would not save: {e}"));
    }

    json!({ "ok": sent, "status": status.as_str() })
}

fn send_chat(session: &Session, parsed: &Value) -> Value {
    let (Some(peer_id), Some(body)) = (arg_str(parsed, "peer_id"), arg_str(parsed, "body")) else {
        return err("chat.send requires \"peer_id\" and \"body\"");
    };

    let message_id = uuid::Uuid::new_v4().to_string();
    let timestamp = now_secs();
    let handle = session
        .peer_store
        .read()
        .get(&session.identity.peer_id())
        .map(|rec| rec.display_name())
        .unwrap_or_default();

    let mut msg = SignalingMessage::new(MessageType::ChatMessage, session.my_public_id.clone());
    msg.target = Some(peer_id.to_owned());
    msg.payload
        .insert("body".into(), Value::String(body.to_owned()));
    msg.payload
        .insert("message_id".into(), Value::String(message_id.clone()));
    msg.payload
        .insert("sender_handle".into(), Value::String(handle.clone()));

    let sent = session.send(ConnectionCommand::SendMessage(msg));

    let record = ChatMessage {
        id: message_id.clone(),
        peer_id: peer_id.to_owned(),
        sender: session.my_public_id.clone(),
        recipient: peer_id.to_owned(),
        body: body.to_owned(),
        timestamp,
        is_self: true,
        // Never leave a message reading "sending" when the command channel
        // already refused it — the user needs to know to retry.
        status: if sent {
            MessageStatus::Sending
        } else {
            MessageStatus::Failed
        },
        kind: MessageKind::Text,
        attachment_name: String::new(),
        attachment_path: String::new(),
        size_str: String::new(),
        status_note: if sent {
            String::new()
        } else {
            "could not reach the connection manager".to_owned()
        },
        sender_handle: handle,
    };
    if let Err(e) = session.chat_store.insert(&record) {
        warn!("could not persist outbound chat: {e}");
    }

    json!({ "ok": sent, "message_id": message_id, "timestamp": timestamp })
}

/// Send a message to a room's chat.
///
/// Unlike direct chat this is not persisted locally first: room history is
/// replayed from the room itself, and the sender sees its own message when it
/// comes back through `room_chat_message`.
fn send_room_chat(session: &Session, parsed: &Value) -> Value {
    let (Some(supernode_id), Some(room_id), Some(body)) = (
        arg_str(parsed, "supernode_id"),
        arg_str(parsed, "room_id"),
        arg_str(parsed, "body"),
    ) else {
        return err("room.chat.send requires \"supernode_id\", \"room_id\" and \"body\"");
    };

    let message_id = uuid::Uuid::new_v4().to_string();
    let sender_handle = session
        .peer_store
        .read()
        .get(&session.identity.peer_id())
        .map(|rec| rec.display_name())
        .unwrap_or_default();

    let sent = session.send(ConnectionCommand::SendSfuChat {
        supernode_id: supernode_id.to_owned(),
        room_id: room_id.to_owned(),
        body: body.to_owned(),
        sender_handle,
        message_id: message_id.clone(),
    });

    json!({ "ok": sent, "message_id": message_id })
}

/// Ask the core to mint an invite URL.
///
/// `GenerateInvite` answers on a reply channel rather than as an event, so
/// this is the one command that waits.
fn generate_invite(session: &Session) -> Value {
    let (reply_tx, reply_rx) = std_mpsc::channel();
    if !session.send(ConnectionCommand::GenerateInvite { reply_tx }) {
        return err("could not reach the connection manager");
    }

    match reply_rx.recv_timeout(REPLY_TIMEOUT) {
        Ok(Some(url)) => json!({ "ok": true, "invite_url": url }),
        Ok(None) => err("the core declined to generate an invite"),
        Err(e) => err(format!("invite generation timed out: {e}")),
    }
}

/// Read a non-empty string argument.
fn arg_str<'a>(parsed: &'a Value, key: &str) -> Option<&'a str> {
    parsed.get(key).and_then(Value::as_str)
}

/// Seconds since the Unix epoch, matching the core's timestamp convention.
fn now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Result for a fire-and-forget command.
fn queued(sent: bool) -> Value {
    if sent {
        json!({ "ok": true })
    } else {
        err("the command channel is full or closed")
    }
}

/// Build an error reply.
fn err(message: impl Into<String>) -> Value {
    json!({ "ok": false, "error": message.into() })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Guards against a match arm being deleted while still advertised.
    ///
    /// This is not hypothetical: a range edit that spanned two neighbouring
    /// arms removed `room.join` and `room.leave` outright, and the only
    /// symptom was the app reporting "unknown command" at runtime. Reading our
    /// own source is crude, but it is the cheapest check that the dispatcher
    /// actually handles everything it claims to - `dispatch` needs a live
    /// `Session` (tokio runtime, QUIC endpoint, three stores) so it cannot be
    /// called from a unit test.
    #[test]
    fn every_known_command_has_a_match_arm() {
        let source = include_str!("command.rs");
        for command in KNOWN_COMMANDS {
            let quoted = format!("\"{command}\"");
            let occurrences = source.matches(&quoted).count();
            assert!(
                occurrences >= 2,
                "{command} is listed in KNOWN_COMMANDS but has no match arm                  (found {occurrences} occurrence(s); expected the list entry plus an arm)",
            );
        }
    }

    #[test]
    fn known_commands_are_unique() {
        let mut seen = KNOWN_COMMANDS.to_vec();
        seen.sort_unstable();
        let before = seen.len();
        seen.dedup();
        assert_eq!(before, seen.len(), "duplicate entry in KNOWN_COMMANDS");
    }

    #[test]
    fn rejects_malformed_json() {
        let reply = parse_request("{not json").expect_err("should not parse");
        assert_eq!(reply["ok"], json!(false));
        assert!(reply["error"]
            .as_str()
            .unwrap_or_default()
            .contains("malformed"));
    }

    #[test]
    fn rejects_a_request_with_no_cmd() {
        let reply = parse_request(r#"{"peer_id":"abc"}"#).expect_err("should be rejected");
        assert_eq!(reply["ok"], json!(false));
    }

    #[test]
    fn parses_a_well_formed_request() {
        let (cmd, body) = parse_request(r#"{"cmd":"chat.send","body":"hi"}"#)
            .unwrap_or_else(|e| panic!("should parse: {e}"));
        assert_eq!(cmd, "chat.send");
        assert_eq!(arg_str(&body, "body"), Some("hi"));
    }

    #[test]
    fn queued_reports_channel_failure() {
        assert_eq!(queued(true)["ok"], json!(true));
        assert_eq!(queued(false)["ok"], json!(false));
    }

    #[test]
    fn arg_str_reads_only_strings() {
        let v = json!({ "a": "x", "b": 3 });
        assert_eq!(arg_str(&v, "a"), Some("x"));
        assert_eq!(arg_str(&v, "b"), None);
        assert_eq!(arg_str(&v, "missing"), None);
    }
}
