//! SFU client — client-side handler for SFU room membership.
//!
//! Audio delivery uses QUIC datagrams; this module only tracks join/leave
//! state and the room membership list. The QUIC peer manager handles actual
//! audio encoding and transport.

use std::collections::HashSet;
use tracing::{info, warn};

use crate::connection_manager::ConnectionCommand;
use tokio::sync::mpsc;

// ---------------------------------------------------------------------------
// Events / Commands
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum SfuEvent {
    Connected { room_id: String },
    Disconnected,
    MembersChanged(Vec<String>),
    Error(String),
}

#[derive(Debug)]
pub enum SfuCommand {
    Join {
        supernode_id: String,
        room_id: String,
    },
    Leave,
    UpdateMembers(Vec<String>),
    Shutdown,
}

// ---------------------------------------------------------------------------
// SfuClient
// ---------------------------------------------------------------------------

/// Client-side SFU room membership tracker.
///
/// Create with [`SfuClient::split`], spawn the future, then drive via commands.
pub struct SfuClient {
    supernode_id: Option<String>,
    room_id: String,
    members: HashSet<String>,
    connected: bool,

    /// Connection manager channel for signaling SFU_ROOM_JOIN / SFU_ROOM_LEAVE.
    conn_cmd_tx: Option<mpsc::Sender<ConnectionCommand>>,

    event_tx: mpsc::Sender<SfuEvent>,
    cmd_rx: mpsc::Receiver<SfuCommand>,
}

impl SfuClient {
    /// Create an SFU client and split into channels + a runnable future.
    ///
    /// Pass `conn_cmd_tx` to wire signaling through the `ConnectionManager`.
    pub fn split(
        conn_cmd_tx: Option<mpsc::Sender<ConnectionCommand>>,
    ) -> (
        mpsc::Sender<SfuCommand>,
        mpsc::Receiver<SfuEvent>,
        impl std::future::Future<Output = ()>,
    ) {
        let (event_tx, event_rx) = mpsc::channel::<SfuEvent>(64);
        let (cmd_tx, cmd_rx) = mpsc::channel::<SfuCommand>(32);
        let client = Self {
            supernode_id: None,
            room_id: "default".to_string(),
            members: HashSet::new(),
            connected: false,
            conn_cmd_tx,
            event_tx,
            cmd_rx,
        };
        (cmd_tx, event_rx, client.run())
    }

    // -- Accessors ----------------------------------------------------------

    pub fn is_connected(&self) -> bool {
        self.connected
    }

    pub fn room_id(&self) -> &str {
        &self.room_id
    }

    pub fn members(&self) -> Vec<&str> {
        self.members.iter().map(String::as_str).collect()
    }

    // -- Command handlers ---------------------------------------------------

    fn handle_join(&mut self, supernode_id: String, room_id: String) {
        info!(
            "SFU joining room '{}' on supernode {}",
            room_id,
            &supernode_id[..supernode_id.len().min(12)]
        );
        // Send SfuJoin via connection manager
        if let Some(ref tx) = self.conn_cmd_tx {
            let _ = tx.try_send(ConnectionCommand::JoinRoom {
                supernode_id: supernode_id.clone(),
                room_id: room_id.clone(),
            });
        }
        self.supernode_id = Some(supernode_id);
        self.room_id = room_id.clone();
        self.connected = true;
        let _ = self.event_tx.try_send(SfuEvent::Connected { room_id });
    }

    fn handle_leave(&mut self) {
        if !self.connected {
            return;
        }
        info!("SFU leaving room '{}'", self.room_id);
        // Send SfuLeave via connection manager
        if let (Some(ref tx), Some(ref sn_id)) = (&self.conn_cmd_tx, &self.supernode_id) {
            let _ = tx.try_send(ConnectionCommand::LeaveRoom {
                supernode_id: sn_id.clone(),
                room_id: self.room_id.clone(),
            });
        }
        self.connected = false;
        self.members.clear();
        self.supernode_id = None;
        let _ = self.event_tx.try_send(SfuEvent::Disconnected);
    }

    fn handle_update_members(&mut self, members: Vec<String>) {
        self.members = members.into_iter().collect();
        let list: Vec<String> = self.members.iter().cloned().collect();
        let _ = self.event_tx.try_send(SfuEvent::MembersChanged(list));
    }

    // -- Event loop ---------------------------------------------------------

    async fn run(mut self) {
        info!("SfuClient started");
        loop {
            match self.cmd_rx.recv().await {
                Some(SfuCommand::Join {
                    supernode_id,
                    room_id,
                }) => {
                    self.handle_join(supernode_id, room_id);
                }
                Some(SfuCommand::Leave) => {
                    self.handle_leave();
                }
                Some(SfuCommand::UpdateMembers(members)) => {
                    self.handle_update_members(members);
                }
                Some(SfuCommand::Shutdown) | None => {
                    self.handle_leave();
                    break;
                }
            }
        }
        info!("SfuClient stopped");
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn join_and_leave() {
        let (cmd_tx, mut event_rx, fut) = SfuClient::split(None);
        let handle = tokio::spawn(fut);

        cmd_tx
            .send(SfuCommand::Join {
                supernode_id: "sn-001".to_string(),
                room_id: "room-abc".to_string(),
            })
            .await
            .unwrap();
        let ev = event_rx.recv().await.unwrap();
        assert!(matches!(ev, SfuEvent::Connected { .. }));

        cmd_tx.send(SfuCommand::Leave).await.unwrap();
        let ev = event_rx.recv().await.unwrap();
        assert!(matches!(ev, SfuEvent::Disconnected));

        cmd_tx.send(SfuCommand::Shutdown).await.unwrap();
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn members_updated() {
        let (cmd_tx, mut event_rx, fut) = SfuClient::split(None);
        let handle = tokio::spawn(fut);

        cmd_tx
            .send(SfuCommand::Join {
                supernode_id: "sn".to_string(),
                room_id: "r".to_string(),
            })
            .await
            .unwrap();
        event_rx.recv().await.unwrap(); // Connected

        cmd_tx
            .send(SfuCommand::UpdateMembers(vec!["p1".into(), "p2".into()]))
            .await
            .unwrap();
        let ev = event_rx.recv().await.unwrap();
        if let SfuEvent::MembersChanged(members) = ev {
            assert_eq!(members.len(), 2);
        } else {
            panic!("expected MembersChanged");
        }

        cmd_tx.send(SfuCommand::Shutdown).await.unwrap();
        handle.await.unwrap();
    }
}
