//! Chat store — SQLite-backed persistent chat history.
//!
//! Message body and sender_handle columns are stored as AES-256-GCM encrypted
//! blobs keyed by an HKDF subkey of the user's Identity. Existing
//! `chat_history.db` files are readable without migration.

use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;

use crate::crypto::{decrypt_blob, encrypt_blob};
use crate::error::{ClientError, Result};
use crate::identity::Identity;

pub const CHAT_DB_FILENAME: &str = "chat_history.db";
pub const CHAT_STORE_LABEL: &str = "conquerd-store/chat/v1";
pub const PAGE_SIZE: usize = 50;

// ---------------------------------------------------------------------------
// ChatMessage
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageKind {
    Text,
    File,
    Image,
    System,
}

impl MessageKind {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Text => "text",
            Self::File => "file",
            Self::Image => "image",
            Self::System => "system",
        }
    }

    fn from_str(s: &str) -> Self {
        match s {
            "file" => Self::File,
            "image" => Self::Image,
            "system" => Self::System,
            _ => Self::Text,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageStatus {
    Sending,
    Sent,
    Delivered,
    Read,
    Failed,
}

impl MessageStatus {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Sending => "sending",
            Self::Sent => "sent",
            Self::Delivered => "delivered",
            Self::Read => "read",
            Self::Failed => "failed",
        }
    }

    fn from_str(s: &str) -> Self {
        match s {
            "sending" => Self::Sending,
            "sent" => Self::Sent,
            "delivered" => Self::Delivered,
            "read" => Self::Read,
            "failed" => Self::Failed,
            _ => Self::Sending,
        }
    }
}

/// A single chat message as returned from the store.
#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub id: String,
    pub peer_id: String,
    pub sender: String,
    pub recipient: String,
    pub body: String,
    pub timestamp: f64,
    pub is_self: bool,
    pub status: MessageStatus,
    pub kind: MessageKind,
    pub attachment_name: String,
    pub attachment_path: String,
    pub size_str: String,
    pub status_note: String,
    pub sender_handle: String,
}

// ---------------------------------------------------------------------------
// ChatStore
// ---------------------------------------------------------------------------

/// SQLite-backed persistent chat storage with per-row AES-256-GCM encryption.
///
/// Encrypts `body` and `sender_handle` columns; all other columns are
/// stored in plaintext (peer_id, timestamp, status, kind) to enable
/// efficient indexed queries.
pub struct ChatStore {
    conn: Arc<Mutex<Connection>>,
    key: [u8; 32],
}

impl ChatStore {
    /// Open the chat store for the given identity.
    pub fn open(identity: &Identity, db_path: Option<&Path>) -> Result<Self> {
        let default_dir = Identity::default_key_dir();
        let path = db_path
            .map(Path::to_path_buf)
            .unwrap_or_else(|| default_dir.join(CHAT_DB_FILENAME));

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let conn = Connection::open(&path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;

        let key = identity.derive_store_key(CHAT_STORE_LABEL)?;
        let store = Self {
            conn: Arc::new(Mutex::new(conn)),
            key,
        };
        store.migrate()?;
        // At process start nothing is genuinely in flight: any self-authored
        // row still marked `sending` is a leftover from a previous session
        // that was never confirmed delivered or failed. Reconcile it to
        // `failed` so the UI can offer a retry instead of stranding it.
        store.fail_stale_sending()?;
        Ok(store)
    }

    /// Flip any self-authored `sending` messages to `failed`.
    ///
    /// Called once on [`open`]. Inbound messages never carry `sending`, so
    /// this only affects outbound messages that were interrupted (app closed
    /// or crashed) before an ack/failure arrived. Returns rows updated.
    fn fail_stale_sending(&self) -> Result<usize> {
        let conn = self.conn.lock();
        let n = conn.execute(
            "UPDATE messages SET status='failed', \
             status_note='interrupted before delivery' \
             WHERE is_self=1 AND status='sending'",
            [],
        )?;
        Ok(n)
    }

    fn migrate(&self) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS messages (
                id              TEXT PRIMARY KEY,
                peer_id         TEXT NOT NULL,
                sender          TEXT NOT NULL,
                recipient       TEXT NOT NULL,
                body            BLOB NOT NULL,
                timestamp       REAL NOT NULL,
                is_self         INTEGER NOT NULL DEFAULT 0,
                status          TEXT NOT NULL DEFAULT 'sent',
                kind            TEXT NOT NULL DEFAULT 'text',
                attachment_name TEXT NOT NULL DEFAULT '',
                attachment_path TEXT NOT NULL DEFAULT '',
                size_str        TEXT NOT NULL DEFAULT '',
                status_note     TEXT NOT NULL DEFAULT '',
                sender_handle   BLOB NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_messages_peer_ts
                ON messages (peer_id, timestamp);
            "#,
        )?;
        Ok(())
    }

    // -- helpers ------------------------------------------------------------

    fn encrypt(&self, plaintext: &str) -> Result<Vec<u8>> {
        encrypt_blob(&self.key, plaintext.as_bytes())
    }

    fn decrypt(&self, blob: &[u8]) -> Result<String> {
        let plain = decrypt_blob(&self.key, blob)?;
        String::from_utf8(plain).map_err(|e| ClientError::Store(format!("UTF-8 decode error: {e}")))
    }

    // -- write operations ---------------------------------------------------

    /// Insert a new message. Returns an error if `id` already exists.
    pub fn insert(&self, msg: &ChatMessage) -> Result<()> {
        let body_blob = self.encrypt(&msg.body)?;
        let handle_blob = self.encrypt(&msg.sender_handle)?;
        let conn = self.conn.lock();
        conn.execute(
            r#"INSERT INTO messages
               (id, peer_id, sender, recipient, body, timestamp, is_self,
                status, kind, attachment_name, attachment_path, size_str,
                status_note, sender_handle)
               VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)"#,
            params![
                msg.id,
                msg.peer_id,
                msg.sender,
                msg.recipient,
                body_blob,
                msg.timestamp,
                msg.is_self as i64,
                msg.status.as_str(),
                msg.kind.as_str(),
                msg.attachment_name,
                msg.attachment_path,
                msg.size_str,
                msg.status_note,
                handle_blob,
            ],
        )?;
        Ok(())
    }

    /// Upsert (insert or replace) a message.
    pub fn upsert(&self, msg: &ChatMessage) -> Result<()> {
        let body_blob = self.encrypt(&msg.body)?;
        let handle_blob = self.encrypt(&msg.sender_handle)?;
        let conn = self.conn.lock();
        conn.execute(
            r#"INSERT OR REPLACE INTO messages
               (id, peer_id, sender, recipient, body, timestamp, is_self,
                status, kind, attachment_name, attachment_path, size_str,
                status_note, sender_handle)
               VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)"#,
            params![
                msg.id,
                msg.peer_id,
                msg.sender,
                msg.recipient,
                body_blob,
                msg.timestamp,
                msg.is_self as i64,
                msg.status.as_str(),
                msg.kind.as_str(),
                msg.attachment_name,
                msg.attachment_path,
                msg.size_str,
                msg.status_note,
                handle_blob,
            ],
        )?;
        Ok(())
    }

    /// Update the `status` field of a message by id.
    pub fn update_status(&self, msg_id: &str, status: MessageStatus) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE messages SET status = ?1 WHERE id = ?2",
            params![status.as_str(), msg_id],
        )?;
        Ok(())
    }

    /// Update the `status` and `status_note` fields together.
    pub fn update_status_note(
        &self,
        msg_id: &str,
        status: MessageStatus,
        note: &str,
    ) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE messages SET status = ?1, status_note = ?2 WHERE id = ?3",
            params![status.as_str(), note, msg_id],
        )?;
        Ok(())
    }

    // -- read operations ----------------------------------------------------

    /// Fetch the most recent `PAGE_SIZE` messages for a peer conversation.
    ///
    /// Returns messages ordered oldest-first (ascending timestamp).
    pub fn get_history(&self, peer_id: &str, page: usize) -> Result<Vec<ChatMessage>> {
        let offset = page * PAGE_SIZE;
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            r#"SELECT id, peer_id, sender, recipient, body, timestamp, is_self,
                      status, kind, attachment_name, attachment_path,
                      size_str, status_note, sender_handle
               FROM messages
               WHERE peer_id = ?1
               ORDER BY timestamp DESC
               LIMIT ?2 OFFSET ?3"#,
        )?;
        let rows = stmt.query_map(params![peer_id, PAGE_SIZE as i64, offset as i64], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Vec<u8>>(4)?,
                row.get::<_, f64>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, String>(9)?,
                row.get::<_, String>(10)?,
                row.get::<_, String>(11)?,
                row.get::<_, String>(12)?,
                row.get::<_, Vec<u8>>(13)?,
            ))
        })?;

        let mut msgs: Vec<ChatMessage> = Vec::new();
        for row in rows {
            let (
                id,
                peer_id,
                sender,
                recipient,
                body_blob,
                timestamp,
                is_self,
                status_str,
                kind_str,
                att_name,
                att_path,
                size_str,
                status_note,
                handle_blob,
            ) = row?;

            let body = self.decrypt(&body_blob).unwrap_or_default();
            let sender_handle = self.decrypt(&handle_blob).unwrap_or_default();
            msgs.push(ChatMessage {
                id,
                peer_id,
                sender,
                recipient,
                body,
                timestamp,
                is_self: is_self != 0,
                status: MessageStatus::from_str(&status_str),
                kind: MessageKind::from_str(&kind_str),
                attachment_name: att_name,
                attachment_path: att_path,
                size_str,
                status_note,
                sender_handle,
            });
        }
        // Reverse so oldest-first
        msgs.reverse();
        Ok(msgs)
    }

    /// Fetch a single message by id.
    pub fn get_by_id(&self, msg_id: &str) -> Result<Option<ChatMessage>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            r#"SELECT id, peer_id, sender, recipient, body, timestamp, is_self,
                      status, kind, attachment_name, attachment_path,
                      size_str, status_note, sender_handle
               FROM messages WHERE id = ?1"#,
        )?;
        let result = stmt
            .query_row(params![msg_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Vec<u8>>(4)?,
                    row.get::<_, f64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, String>(10)?,
                    row.get::<_, String>(11)?,
                    row.get::<_, String>(12)?,
                    row.get::<_, Vec<u8>>(13)?,
                ))
            })
            .optional()?;

        Ok(result.map(
            |(
                id,
                peer_id,
                sender,
                recipient,
                body_blob,
                timestamp,
                is_self,
                status_str,
                kind_str,
                att_name,
                att_path,
                size_str,
                status_note,
                handle_blob,
            )| {
                ChatMessage {
                    id,
                    peer_id,
                    sender,
                    recipient,
                    body: self.decrypt(&body_blob).unwrap_or_default(),
                    timestamp,
                    is_self: is_self != 0,
                    status: MessageStatus::from_str(&status_str),
                    kind: MessageKind::from_str(&kind_str),
                    attachment_name: att_name,
                    attachment_path: att_path,
                    size_str,
                    status_note,
                    sender_handle: self.decrypt(&handle_blob).unwrap_or_default(),
                }
            },
        ))
    }

    /// Count unread messages (inbound, not yet read) for a peer.
    pub fn unread_count(&self, peer_id: &str) -> Result<usize> {
        let conn = self.conn.lock();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM messages WHERE peer_id=?1 AND is_self=0 AND status!='read'",
            params![peer_id],
            |r| r.get(0),
        )?;
        Ok(count as usize)
    }

    /// Count unread inbound messages across all peers.
    pub fn total_unread_count(&self) -> Result<usize> {
        let conn = self.conn.lock();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM messages WHERE is_self=0 AND status!='read'",
            [],
            |r| r.get(0),
        )?;
        Ok(count as usize)
    }

    /// Mark all inbound messages for a peer as read. Returns rows updated.
    pub fn mark_peer_read(&self, peer_id: &str) -> Result<usize> {
        let conn = self.conn.lock();
        let n = conn.execute(
            "UPDATE messages SET status='read' WHERE peer_id=?1 AND is_self=0 AND status!='read'",
            params![peer_id],
        )?;
        Ok(n)
    }

    /// Delete a single message by its ID.
    pub fn delete_message(&self, msg_id: &str) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute("DELETE FROM messages WHERE id=?1", params![msg_id])?;
        Ok(())
    }

    /// Delete all messages for a peer.
    pub fn clear_history(&self, peer_id: &str) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute("DELETE FROM messages WHERE peer_id=?1", params![peer_id])?;
        Ok(())
    }

    /// Total message count (for stats/testing).
    pub fn total_count(&self) -> Result<usize> {
        let conn = self.conn.lock();
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))?;
        Ok(count as usize)
    }

    /// Delete messages older than `days` days. Returns the number of rows deleted.
    pub fn trim_by_age(&self, days: i32) -> Result<usize> {
        let cutoff = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0)
            - (days as f64) * 86400.0;
        let conn = self.conn.lock();
        let n = conn.execute("DELETE FROM messages WHERE timestamp < ?1", params![cutoff])?;
        Ok(n)
    }

    /// For each peer, keep only the most recent `keep` messages, deleting the rest.
    /// Returns the total number of rows deleted.
    pub fn trim_by_count(&self, keep: i32) -> Result<usize> {
        let conn = self.conn.lock();
        // Collect distinct peer IDs first to avoid holding a statement borrow while deleting.
        let peer_ids: Vec<String> = {
            let mut stmt = conn.prepare("SELECT DISTINCT peer_id FROM messages")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            rows.filter_map(|r| r.ok()).collect()
        };
        let mut total = 0usize;
        for pid in &peer_ids {
            // Delete any row whose rowid is NOT among the most-recent `keep` rowids for this peer.
            let n = conn.execute(
                "DELETE FROM messages WHERE peer_id=?1 AND rowid NOT IN \
                 (SELECT rowid FROM messages WHERE peer_id=?1 \
                  ORDER BY timestamp DESC LIMIT ?2)",
                params![pid, keep],
            )?;
            total += n;
        }
        Ok(total)
    }

    /// Delete all messages across all peers. Returns the number of rows deleted.
    pub fn purge_all(&self) -> Result<usize> {
        let conn = self.conn.lock();
        let n = conn.execute("DELETE FROM messages", [])?;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;
    use tempfile::tempdir;
    use uuid::Uuid;

    fn make_msg(peer_id: &str, body: &str, is_self: bool) -> ChatMessage {
        ChatMessage {
            id: Uuid::new_v4().to_string(),
            peer_id: peer_id.to_owned(),
            sender: if is_self {
                "me".to_owned()
            } else {
                peer_id.to_owned()
            },
            recipient: if is_self {
                peer_id.to_owned()
            } else {
                "me".to_owned()
            },
            body: body.to_owned(),
            timestamp: unix_now(),
            is_self,
            status: MessageStatus::Sent,
            kind: MessageKind::Text,
            attachment_name: String::new(),
            attachment_path: String::new(),
            size_str: String::new(),
            status_note: String::new(),
            sender_handle: "Alice".to_owned(),
        }
    }

    fn unix_now() -> f64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0)
    }

    #[test]
    fn insert_and_retrieve() {
        let dir = tempdir().unwrap();
        let id = Identity::generate();
        let store = ChatStore::open(&id, Some(&dir.path().join(CHAT_DB_FILENAME))).unwrap();

        let msg = make_msg("peer1", "Hello, world!", false);
        store.insert(&msg).unwrap();

        let history = store.get_history("peer1", 0).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].body, "Hello, world!");
        assert_eq!(history[0].sender_handle, "Alice");
    }

    #[test]
    fn room_keyed_history_survives_reopen() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join(CHAT_DB_FILENAME);
        let id = Identity::generate();
        let room_key = "room:supernode-a:room-1";

        {
            let store = ChatStore::open(&id, Some(&db_path)).unwrap();
            let mut msg = make_msg(room_key, "room hello", true);
            msg.sender = "me".to_owned();
            msg.recipient = "room-1".to_owned();
            msg.sender_handle = "Me".to_owned();
            store.insert(&msg).unwrap();
        }

        let store = ChatStore::open(&id, Some(&db_path)).unwrap();
        let history = store.get_history(room_key, 0).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].peer_id, room_key);
        assert_eq!(history[0].recipient, "room-1");
        assert_eq!(history[0].body, "room hello");
        assert_eq!(history[0].sender_handle, "Me");
    }

    #[test]
    fn body_is_encrypted_at_rest() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join(CHAT_DB_FILENAME);
        let id = Identity::generate();
        let store = ChatStore::open(&id, Some(&db_path)).unwrap();
        let msg = make_msg("peer1", "secret-text", false);
        store.insert(&msg).unwrap();
        drop(store);

        // Read the raw SQLite file and verify plaintext does not appear
        let raw = std::fs::read(&db_path).unwrap();
        let raw_str = String::from_utf8_lossy(&raw);
        assert!(
            !raw_str.contains("secret-text"),
            "body leaked to disk unencrypted"
        );
    }

    #[test]
    fn update_status() {
        let dir = tempdir().unwrap();
        let id = Identity::generate();
        let store = ChatStore::open(&id, Some(&dir.path().join(CHAT_DB_FILENAME))).unwrap();
        let msg = make_msg("peer1", "hi", true);
        let id_str = msg.id.clone();
        store.insert(&msg).unwrap();
        store
            .update_status(&id_str, MessageStatus::Delivered)
            .unwrap();
        let loaded = store.get_by_id(&id_str).unwrap().unwrap();
        assert_eq!(loaded.status, MessageStatus::Delivered);
    }

    #[test]
    fn status_from_str_sent_is_distinct_from_sending() {
        assert_eq!(MessageStatus::from_str("sent"), MessageStatus::Sent);
        assert_eq!(MessageStatus::from_str("sending"), MessageStatus::Sending);
    }

    #[test]
    fn mark_peer_read_and_unread_counts() {
        let dir = tempdir().unwrap();
        let id = Identity::generate();
        let store = ChatStore::open(&id, Some(&dir.path().join(CHAT_DB_FILENAME))).unwrap();

        let inbound = make_msg("peer1", "hello", false);
        store.insert(&inbound).unwrap();
        let outbound = make_msg("peer1", "reply", true);
        store.insert(&outbound).unwrap();

        assert_eq!(store.unread_count("peer1").unwrap(), 1);
        assert_eq!(store.total_unread_count().unwrap(), 1);

        store.mark_peer_read("peer1").unwrap();
        assert_eq!(store.unread_count("peer1").unwrap(), 0);
        assert_eq!(store.total_unread_count().unwrap(), 0);

        let loaded = store.get_by_id(&inbound.id).unwrap().unwrap();
        assert_eq!(loaded.status, MessageStatus::Read);
    }

    #[test]
    fn pagination() {
        let dir = tempdir().unwrap();
        let id = Identity::generate();
        let store = ChatStore::open(&id, Some(&dir.path().join(CHAT_DB_FILENAME))).unwrap();
        for i in 0..75 {
            let mut msg = make_msg("peer1", &format!("msg {i}"), i % 2 == 0);
            msg.timestamp = i as f64;
            store.insert(&msg).unwrap();
        }
        let page0 = store.get_history("peer1", 0).unwrap();
        assert_eq!(page0.len(), PAGE_SIZE);
        let page1 = store.get_history("peer1", 1).unwrap();
        assert_eq!(page1.len(), 25);
    }

    #[test]
    fn stale_sending_is_failed_on_reopen() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join(CHAT_DB_FILENAME);
        let id = Identity::generate();

        // First session: persist an outbound message still in `sending`, plus
        // an inbound message (which must be left untouched).
        let outbound_id = {
            let store = ChatStore::open(&id, Some(&db_path)).unwrap();
            let mut outbound = make_msg("peer1", "in flight", true);
            outbound.status = MessageStatus::Sending;
            store.insert(&outbound).unwrap();
            let inbound = make_msg("peer1", "incoming", false);
            store.insert(&inbound).unwrap();
            outbound.id
        };

        // Reopen: the stale outbound `sending` row must become `failed`.
        let store = ChatStore::open(&id, Some(&db_path)).unwrap();
        let reloaded = store.get_by_id(&outbound_id).unwrap().unwrap();
        assert_eq!(reloaded.status, MessageStatus::Failed);
        assert_eq!(reloaded.status_note, "interrupted before delivery");

        // The inbound message is unaffected.
        let history = store.get_history("peer1", 0).unwrap();
        let inbound = history.iter().find(|m| !m.is_self).unwrap();
        assert_ne!(inbound.status, MessageStatus::Failed);
    }
}
