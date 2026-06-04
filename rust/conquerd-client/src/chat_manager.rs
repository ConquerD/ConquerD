//! Chat manager — in-memory conversation state coordinator.
//!
//! The manager tracks per-peer conversation state (unread counts, typing
//! indicators, call state, in-memory message list) on top of the persistent
//! [`ChatStore`].  It is intentionally free of async I/O and Qt bindings:
//! all persistence is delegated to `ChatStore` and all UI events are returned
//! as `Vec<ChatManagerEvent>` for the caller to dispatch.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use tracing::debug;

use crate::chat_store::{ChatMessage, ChatStore, MessageKind, MessageStatus};

// ---------------------------------------------------------------------------
// Preview helper
// ---------------------------------------------------------------------------

const PREVIEW_MAX: usize = 45;

pub fn format_preview(msg: &ChatMessage) -> String {
    let text = match msg.kind {
        MessageKind::File => format!(
            "📎 {}",
            if msg.attachment_name.is_empty() {
                "file"
            } else {
                &msg.attachment_name
            }
        ),
        MessageKind::Image => format!(
            "🖼️ {}",
            if msg.attachment_name.is_empty() {
                "image"
            } else {
                &msg.attachment_name
            }
        ),
        _ => msg.body.replace('\n', " "),
    };
    let prefix = if msg.is_self { "You: " } else { "" };
    let full = format!("{prefix}{text}");
    if full.chars().count() > PREVIEW_MAX {
        let truncated: String = full.chars().take(PREVIEW_MAX - 1).collect();
        format!("{truncated}…")
    } else {
        full
    }
}

// ---------------------------------------------------------------------------
// Call state
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum CallState {
    #[default]
    None,
    Requesting, // we sent a call request
    Incoming,   // peer sent us a call request
    Active,     // call in progress
    Ended,
}

impl CallState {
    pub fn as_str(&self) -> &str {
        match self {
            Self::None => "none",
            Self::Requesting => "requesting",
            Self::Incoming => "incoming",
            Self::Active => "active",
            Self::Ended => "ended",
        }
    }
}

// ---------------------------------------------------------------------------
// Conversation
// ---------------------------------------------------------------------------

/// In-memory state for a single peer conversation.
pub struct Conversation {
    pub peer_id: String,
    pub display_name: String,
    /// Most-recent few messages (in-memory cache; store holds full history).
    pub messages: Vec<ChatMessage>,
    pub unread_count: u32,
    pub typing: bool,
    pub call_state: CallState,
}

impl Conversation {
    fn new(peer_id: impl Into<String>) -> Self {
        Self {
            peer_id: peer_id.into(),
            display_name: String::new(),
            messages: Vec::new(),
            unread_count: 0,
            typing: false,
            call_state: CallState::None,
        }
    }
}

// ---------------------------------------------------------------------------
// Events produced by ChatManager
// ---------------------------------------------------------------------------

/// Events emitted by [`ChatManager`] methods for the caller to dispatch.
#[derive(Debug, Clone)]
pub enum ChatManagerEvent {
    MessageReceived {
        peer_id: String,
        msg: ChatMessage,
    },
    MessageSent {
        peer_id: String,
        msg: ChatMessage,
    },
    MessageAck {
        peer_id: String,
        message_id: String,
    },
    MessageFailed {
        peer_id: String,
        message_id: String,
    },
    TypingReceived {
        peer_id: String,
        is_typing: bool,
    },
    ConversationUpdated {
        peer_id: String,
    },
    /// Inbound call request (incoming ring).
    CallRequested {
        peer_id: String,
    },
    CallAccepted {
        peer_id: String,
    },
    CallRejected {
        peer_id: String,
    },
    CallEnded {
        peer_id: String,
    },
}

// ---------------------------------------------------------------------------
// ChatManager
// ---------------------------------------------------------------------------

pub struct ChatManager {
    my_id: String,
    conversations: HashMap<String, Conversation>,
    active_call_peer: Option<String>,
    /// Optional persistent store. `None` → in-memory only (tests).
    pub store: Option<ChatStore>,
}

impl ChatManager {
    pub fn new(my_id: impl Into<String>, store: Option<ChatStore>) -> Self {
        Self {
            my_id: my_id.into(),
            conversations: HashMap::new(),
            active_call_peer: None,
            store,
        }
    }

    // -- Conversation access ------------------------------------------------

    pub fn get_or_create(&mut self, peer_id: &str) -> &mut Conversation {
        self.conversations
            .entry(peer_id.to_owned())
            .or_insert_with(|| Conversation::new(peer_id))
    }

    pub fn get(&self, peer_id: &str) -> Option<&Conversation> {
        self.conversations.get(peer_id)
    }

    pub fn total_unread(&self) -> u32 {
        self.conversations.values().map(|c| c.unread_count).sum()
    }

    pub fn mark_read(&mut self, peer_id: &str) -> Vec<ChatManagerEvent> {
        if let Some(c) = self.conversations.get_mut(peer_id) {
            c.unread_count = 0;
            vec![ChatManagerEvent::ConversationUpdated {
                peer_id: peer_id.to_owned(),
            }]
        } else {
            vec![]
        }
    }

    pub fn active_call_peer(&self) -> Option<&str> {
        self.active_call_peer.as_deref()
    }

    pub fn is_in_call(&self, peer_id: &str) -> bool {
        self.conversations
            .get(peer_id)
            .map(|c| c.call_state == CallState::Active)
            .unwrap_or(false)
    }

    // -- Outbound messages --------------------------------------------------

    pub fn create_outbound(
        &mut self,
        peer_id: &str,
        body: &str,
    ) -> (ChatMessage, Vec<ChatManagerEvent>) {
        let msg = ChatMessage {
            id: short_uuid(),
            peer_id: peer_id.to_owned(),
            sender: self.my_id.clone(),
            recipient: peer_id.to_owned(),
            body: body.to_owned(),
            timestamp: unix_now_f64(),
            is_self: true,
            status: MessageStatus::Sending,
            kind: MessageKind::Text,
            attachment_name: String::new(),
            attachment_path: String::new(),
            size_str: String::new(),
            status_note: String::new(),
            sender_handle: String::new(),
        };
        let conv = self.get_or_create(peer_id);
        conv.messages.push(msg.clone());
        if let Some(s) = &self.store {
            let _ = s.upsert(&msg);
        }
        let peer = peer_id.to_owned();
        let evs = vec![
            ChatManagerEvent::MessageSent {
                peer_id: peer.clone(),
                msg: msg.clone(),
            },
            ChatManagerEvent::ConversationUpdated { peer_id: peer },
        ];
        (msg, evs)
    }

    pub fn mark_delivered(&mut self, peer_id: &str, message_id: &str) -> Vec<ChatManagerEvent> {
        if let Some(conv) = self.conversations.get_mut(peer_id) {
            if let Some(m) = conv.messages.iter_mut().rev().find(|m| m.id == message_id) {
                m.status = MessageStatus::Delivered;
            }
        }
        if let Some(s) = &self.store {
            let _ = s.update_status(message_id, MessageStatus::Delivered);
        }
        vec![ChatManagerEvent::MessageAck {
            peer_id: peer_id.to_owned(),
            message_id: message_id.to_owned(),
        }]
    }

    pub fn mark_failed(&mut self, peer_id: &str, message_id: &str) -> Vec<ChatManagerEvent> {
        if let Some(conv) = self.conversations.get_mut(peer_id) {
            if let Some(m) = conv.messages.iter_mut().rev().find(|m| m.id == message_id) {
                m.status = MessageStatus::Failed;
            }
        }
        if let Some(s) = &self.store {
            let _ = s.update_status(message_id, MessageStatus::Failed);
        }
        vec![ChatManagerEvent::MessageFailed {
            peer_id: peer_id.to_owned(),
            message_id: message_id.to_owned(),
        }]
    }

    // -- Inbound messages ---------------------------------------------------

    pub fn receive_message(
        &mut self,
        peer_id: &str,
        message_id: &str,
        body: &str,
        timestamp: f64,
        sender_handle: &str,
    ) -> Vec<ChatManagerEvent> {
        // Dedup
        if let Some(conv) = self.conversations.get(peer_id) {
            if conv.messages.iter().any(|m| m.id == message_id) {
                return vec![];
            }
        }
        let msg = ChatMessage {
            id: message_id.to_owned(),
            peer_id: peer_id.to_owned(),
            sender: peer_id.to_owned(),
            recipient: self.my_id.clone(),
            body: body.to_owned(),
            timestamp: if timestamp > 0.0 {
                timestamp
            } else {
                unix_now_f64()
            },
            is_self: false,
            status: MessageStatus::Delivered,
            kind: MessageKind::Text,
            attachment_name: String::new(),
            attachment_path: String::new(),
            size_str: String::new(),
            status_note: String::new(),
            sender_handle: sender_handle.to_owned(),
        };
        let conv = self.get_or_create(peer_id);
        conv.messages.push(msg.clone());
        conv.unread_count += 1;
        conv.typing = false;
        if let Some(s) = &self.store {
            let _ = s.upsert(&msg);
        }
        let peer = peer_id.to_owned();
        vec![
            ChatManagerEvent::MessageReceived {
                peer_id: peer.clone(),
                msg,
            },
            ChatManagerEvent::ConversationUpdated { peer_id: peer },
        ]
    }

    pub fn receive_typing(&mut self, peer_id: &str, is_typing: bool) -> Vec<ChatManagerEvent> {
        let conv = self.get_or_create(peer_id);
        conv.typing = is_typing;
        vec![ChatManagerEvent::TypingReceived {
            peer_id: peer_id.to_owned(),
            is_typing,
        }]
    }

    /// Record a media/file bubble in conversation history.
    #[allow(clippy::too_many_arguments)]
    pub fn record_media(
        &mut self,
        peer_id: &str,
        kind: MessageKind,
        attachment_name: &str,
        is_self: bool,
        size_str: &str,
        status_note: &str,
        msg_id: Option<&str>,
    ) -> (ChatMessage, Vec<ChatManagerEvent>) {
        let msg = ChatMessage {
            id: msg_id.map(str::to_owned).unwrap_or_else(short_uuid),
            peer_id: peer_id.to_owned(),
            sender: if is_self {
                self.my_id.clone()
            } else {
                peer_id.to_owned()
            },
            recipient: if is_self {
                peer_id.to_owned()
            } else {
                self.my_id.clone()
            },
            body: String::new(),
            timestamp: unix_now_f64(),
            is_self,
            status: MessageStatus::Sent,
            kind,
            attachment_name: attachment_name.to_owned(),
            attachment_path: String::new(),
            size_str: size_str.to_owned(),
            status_note: status_note.to_owned(),
            sender_handle: String::new(),
        };
        let conv = self.get_or_create(peer_id);
        conv.messages.push(msg.clone());
        if let Some(s) = &self.store {
            let _ = s.upsert(&msg);
        }
        let evs = vec![ChatManagerEvent::ConversationUpdated {
            peer_id: peer_id.to_owned(),
        }];
        (msg, evs)
    }

    // -- Room chat ----------------------------------------------------------

    pub fn receive_room_message(
        &mut self,
        room_id: &str,
        sender_id: &str,
        sender_handle: &str,
        body: &str,
        msg_id: &str,
        timestamp: f64,
    ) -> Vec<ChatManagerEvent> {
        let actual_id = if msg_id.is_empty() {
            short_uuid()
        } else {
            msg_id.to_owned()
        };
        let msg = ChatMessage {
            id: actual_id,
            peer_id: room_id.to_owned(),
            sender: sender_id.to_owned(),
            recipient: room_id.to_owned(),
            body: body.to_owned(),
            timestamp: if timestamp > 0.0 {
                timestamp
            } else {
                unix_now_f64()
            },
            is_self: sender_id == self.my_id,
            status: MessageStatus::Delivered,
            kind: MessageKind::Text,
            attachment_name: String::new(),
            attachment_path: String::new(),
            size_str: String::new(),
            status_note: String::new(),
            sender_handle: sender_handle.to_owned(),
        };
        let conv = self.get_or_create(room_id);
        conv.messages.push(msg.clone());
        conv.unread_count += 1;
        if let Some(s) = &self.store {
            let _ = s.upsert(&msg);
        }
        vec![
            ChatManagerEvent::MessageReceived {
                peer_id: room_id.to_owned(),
                msg,
            },
            ChatManagerEvent::ConversationUpdated {
                peer_id: room_id.to_owned(),
            },
        ]
    }

    // -- Call control -------------------------------------------------------

    pub fn request_call(&mut self, peer_id: &str) -> Vec<ChatManagerEvent> {
        let conv = self.get_or_create(peer_id);
        conv.call_state = CallState::Requesting;
        self.active_call_peer = Some(peer_id.to_owned());
        vec![ChatManagerEvent::ConversationUpdated {
            peer_id: peer_id.to_owned(),
        }]
    }

    pub fn receive_call_request(&mut self, peer_id: &str) -> Vec<ChatManagerEvent> {
        let conv = self.get_or_create(peer_id);
        conv.call_state = CallState::Incoming;
        vec![
            ChatManagerEvent::CallRequested {
                peer_id: peer_id.to_owned(),
            },
            ChatManagerEvent::ConversationUpdated {
                peer_id: peer_id.to_owned(),
            },
        ]
    }

    pub fn accept_call(&mut self, peer_id: &str) -> Vec<ChatManagerEvent> {
        let conv = self.get_or_create(peer_id);
        conv.call_state = CallState::Active;
        self.active_call_peer = Some(peer_id.to_owned());
        vec![
            ChatManagerEvent::CallAccepted {
                peer_id: peer_id.to_owned(),
            },
            ChatManagerEvent::ConversationUpdated {
                peer_id: peer_id.to_owned(),
            },
        ]
    }

    pub fn receive_call_accepted(&mut self, peer_id: &str) -> Vec<ChatManagerEvent> {
        let conv = self.get_or_create(peer_id);
        conv.call_state = CallState::Active;
        self.active_call_peer = Some(peer_id.to_owned());
        vec![
            ChatManagerEvent::CallAccepted {
                peer_id: peer_id.to_owned(),
            },
            ChatManagerEvent::ConversationUpdated {
                peer_id: peer_id.to_owned(),
            },
        ]
    }

    pub fn reject_call(&mut self, peer_id: &str) -> Vec<ChatManagerEvent> {
        let conv = self.get_or_create(peer_id);
        conv.call_state = CallState::None;
        vec![
            ChatManagerEvent::CallRejected {
                peer_id: peer_id.to_owned(),
            },
            ChatManagerEvent::ConversationUpdated {
                peer_id: peer_id.to_owned(),
            },
        ]
    }

    pub fn receive_call_rejected(&mut self, peer_id: &str) -> Vec<ChatManagerEvent> {
        let conv = self.get_or_create(peer_id);
        conv.call_state = CallState::None;
        if self.active_call_peer.as_deref() == Some(peer_id) {
            self.active_call_peer = None;
        }
        vec![
            ChatManagerEvent::CallRejected {
                peer_id: peer_id.to_owned(),
            },
            ChatManagerEvent::ConversationUpdated {
                peer_id: peer_id.to_owned(),
            },
        ]
    }

    pub fn end_call(&mut self, peer_id: &str) -> Vec<ChatManagerEvent> {
        let conv = self.get_or_create(peer_id);
        conv.call_state = CallState::Ended;
        if self.active_call_peer.as_deref() == Some(peer_id) {
            self.active_call_peer = None;
        }
        vec![
            ChatManagerEvent::CallEnded {
                peer_id: peer_id.to_owned(),
            },
            ChatManagerEvent::ConversationUpdated {
                peer_id: peer_id.to_owned(),
            },
        ]
    }

    // -- History loading ----------------------------------------------------

    /// Load the first page of messages from the store into the conversation cache.
    /// Returns the loaded messages (oldest-first).
    pub fn load_history(&mut self, peer_id: &str, _limit: usize) -> Vec<ChatMessage> {
        let msgs = match &self.store {
            Some(s) => s.get_history(peer_id, 0).unwrap_or_default(),
            None => {
                return self
                    .conversations
                    .get(peer_id)
                    .map(|c| c.messages.clone())
                    .unwrap_or_default()
            }
        };
        // Populate cache (replace — store is authoritative).
        let conv = self.get_or_create(peer_id);
        conv.messages = msgs.clone();
        debug!(
            "Loaded {} messages for {}",
            msgs.len(),
            &peer_id[..8.min(peer_id.len())]
        );
        msgs
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn short_uuid() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..16].to_owned()
}

fn unix_now_f64() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn manager() -> ChatManager {
        ChatManager::new("selfpeer", None)
    }

    // -- Conversation creation ----------------------------------------------

    #[test]
    fn get_or_create_idempotent() {
        let mut m = manager();
        m.get_or_create("alice");
        m.get_or_create("alice");
        assert_eq!(m.conversations.len(), 1);
    }

    // -- Outbound messages --------------------------------------------------

    #[test]
    fn create_outbound_returns_sending_status() {
        let mut m = manager();
        let (msg, evs) = m.create_outbound("bob", "hello");
        assert!(msg.is_self);
        assert_eq!(msg.body, "hello");
        assert_eq!(msg.status, MessageStatus::Sending);
        assert!(!evs.is_empty());
        let conv = m.get("bob").unwrap();
        assert_eq!(conv.messages.len(), 1);
    }

    #[test]
    fn mark_delivered_updates_status() {
        let mut m = manager();
        let (msg, _) = m.create_outbound("bob", "hi");
        let evs = m.mark_delivered("bob", &msg.id);
        assert!(matches!(&evs[0], ChatManagerEvent::MessageAck { .. }));
        let status = &m.get("bob").unwrap().messages[0].status;
        assert_eq!(*status, MessageStatus::Delivered);
    }

    #[test]
    fn mark_failed_updates_status() {
        let mut m = manager();
        let (msg, _) = m.create_outbound("bob", "hi");
        m.mark_failed("bob", &msg.id);
        let status = &m.get("bob").unwrap().messages[0].status;
        assert_eq!(*status, MessageStatus::Failed);
    }

    // -- Inbound messages ---------------------------------------------------

    #[test]
    fn receive_message_increments_unread() {
        let mut m = manager();
        m.receive_message("alice", "msg-001", "hey", 0.0, "Alice");
        m.receive_message("alice", "msg-002", "you there?", 0.0, "Alice");
        let conv = m.get("alice").unwrap();
        assert_eq!(conv.unread_count, 2);
        assert_eq!(conv.messages.len(), 2);
    }

    #[test]
    fn receive_message_deduplicates() {
        let mut m = manager();
        m.receive_message("alice", "dup-1", "hello", 0.0, "");
        let evs = m.receive_message("alice", "dup-1", "hello again", 0.0, "");
        // Second call should return empty vec (duplicate)
        assert!(evs.is_empty());
        assert_eq!(m.get("alice").unwrap().messages.len(), 1);
    }

    #[test]
    fn mark_read_clears_unread() {
        let mut m = manager();
        m.receive_message("alice", "m1", "hey", 0.0, "");
        m.receive_message("alice", "m2", "hey2", 0.0, "");
        m.mark_read("alice");
        assert_eq!(m.get("alice").unwrap().unread_count, 0);
        assert_eq!(m.total_unread(), 0);
    }

    // -- Typing indicator ---------------------------------------------------

    #[test]
    fn receive_typing_sets_flag() {
        let mut m = manager();
        m.receive_typing("carol", true);
        assert!(m.get("carol").unwrap().typing);
        m.receive_typing("carol", false);
        assert!(!m.get("carol").unwrap().typing);
    }

    // -- Room chat ----------------------------------------------------------

    #[test]
    fn receive_room_message_is_not_self_for_other_sender() {
        let mut m = manager();
        let evs = m.receive_room_message("room-1", "other_peer", "Other", "room hello", "r1", 0.0);
        assert!(!evs.is_empty());
        let msg = &m.get("room-1").unwrap().messages[0];
        assert!(!msg.is_self);
        assert_eq!(msg.sender, "other_peer");
    }

    #[test]
    fn receive_room_message_is_self_for_my_id() {
        let mut m = manager();
        m.receive_room_message("room-1", "selfpeer", "Me", "my msg", "s1", 0.0);
        let msg = &m.get("room-1").unwrap().messages[0];
        assert!(msg.is_self);
    }

    // -- Call control -------------------------------------------------------

    #[test]
    fn call_state_transitions() {
        let mut m = manager();
        // Request a call
        m.request_call("dave");
        assert_eq!(m.get("dave").unwrap().call_state, CallState::Requesting);
        assert_eq!(m.active_call_peer(), Some("dave"));

        // Remote accepts
        m.receive_call_accepted("dave");
        assert_eq!(m.get("dave").unwrap().call_state, CallState::Active);
        assert!(m.is_in_call("dave"));

        // End call
        m.end_call("dave");
        assert_eq!(m.get("dave").unwrap().call_state, CallState::Ended);
        assert_eq!(m.active_call_peer(), None);
    }

    #[test]
    fn incoming_call_then_reject() {
        let mut m = manager();
        m.receive_call_request("eve");
        assert_eq!(m.get("eve").unwrap().call_state, CallState::Incoming);

        m.reject_call("eve");
        assert_eq!(m.get("eve").unwrap().call_state, CallState::None);
    }

    // -- Preview formatting -------------------------------------------------

    #[test]
    fn format_preview_truncates_long_body() {
        let msg = ChatMessage {
            id: "x".into(),
            peer_id: "p".into(),
            sender: "p".into(),
            recipient: "me".into(),
            body: "a".repeat(60),
            timestamp: 0.0,
            is_self: false,
            status: MessageStatus::Sent,
            kind: MessageKind::Text,
            attachment_name: String::new(),
            attachment_path: String::new(),
            size_str: String::new(),
            status_note: String::new(),
            sender_handle: String::new(),
        };
        let preview = format_preview(&msg);
        assert!(preview.chars().count() <= PREVIEW_MAX + 1); // +1 for ellipsis
        assert!(preview.ends_with('…'));
    }

    #[test]
    fn format_preview_file_bubble() {
        let msg = ChatMessage {
            id: "x".into(),
            peer_id: "p".into(),
            sender: "p".into(),
            recipient: "me".into(),
            body: String::new(),
            timestamp: 0.0,
            is_self: false,
            status: MessageStatus::Sent,
            kind: MessageKind::File,
            attachment_name: "photo.png".into(),
            attachment_path: String::new(),
            size_str: String::new(),
            status_note: String::new(),
            sender_handle: String::new(),
        };
        let preview = format_preview(&msg);
        assert!(preview.contains("photo.png"));
    }
}
