//! Supernode WebSocket background tasks.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, Notify};
use tokio::time::sleep;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tracing::{debug, info, warn};

use serde_json::Value;

use crate::identity::Identity;
use crate::protocol::{MessageType, SignalingMessage};

use super::internal::InternalEvent;

const PING_INTERVAL_S: u64 = 30;
/// Base reconnect delay after a successful session ends (clean close or mid-session error).
const RECONNECT_BASE: Duration = Duration::from_secs(1);
/// Cap on exponential backoff after repeated connect failures.
const RECONNECT_MAX: Duration = Duration::from_secs(60);
/// Cap on a single dial (TCP + TLS + HTTP upgrade).
///
/// Without it a connect issued against a half-up interface — the normal state
/// for a second or two after a phone changes network — inherits the OS connect
/// timeout, which is minutes. The task would sit in `connect_async` with no
/// retry and no `WsDisconnected`, so the client looks connected while nothing
/// flows.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// How long a session may receive *nothing* before we treat the link as dead.
///
/// The manager pings every connected supernode every 30 s and the supernode
/// answers with a signed `Pong`, so a healthy link is never silent for longer
/// than one ping period plus RTT — idle or not. Three silent periods therefore
/// mean the path is gone rather than quiet.
///
/// This is what makes a network change recoverable. A TCP socket whose source
/// address has vanished (Wi-Fi to cellular) neither errors nor delivers: reads
/// park forever and writes disappear into the send buffer. With no deadline the
/// task waits in `ws_stream.next()` indefinitely, never emits `WsDisconnected`,
/// and — because relay teardown and re-dial both hang off that event — the
/// whole client stays dark until it is restarted.
#[cfg(not(test))]
const READ_IDLE_TIMEOUT: Duration = Duration::from_secs(95);
/// Same deadline, shortened so the tests exercise the mechanism in real time.
/// The production value is a policy choice about how quiet a healthy supernode
/// may be; what the tests need to pin down is that the deadline fires at all,
/// that inbound traffic defers it, and that the task recovers afterwards.
#[cfg(test)]
const READ_IDLE_TIMEOUT: Duration = Duration::from_millis(300);
/// Settle delay before re-dialing on a network change, so the dial lands after
/// the new route is installed rather than racing it.
const NETWORK_CHANGE_SETTLE: Duration = Duration::from_millis(250);

// ---------------------------------------------------------------------------
// Supernode WebSocket task
// ---------------------------------------------------------------------------

/// Why `connect_and_run_ws` returned.
#[derive(Debug, PartialEq, Eq)]
enum WsSessionEnd {
    /// Manager dropped the outbound channel — stop the task (no reconnect).
    ChannelClosed,
    /// Remote closed the socket (or EOF). Reconnect with a short delay.
    RemoteClosed,
    /// The platform reported the network changed under this socket. Re-dial the
    /// *same* candidate at once: the endpoint is fine, only our local address
    /// moved.
    NetworkChanged,
}

/// Long-running tokio task that maintains a WebSocket connection to a
/// supernode, rotating through ordered `candidates` on failure.
///
/// Reconnects on both clean remote close and I/O errors until the manager
/// drops the outbound send channel (intentional teardown) or the task is
/// aborted.
///
/// `reconnect_now` is the platform's "the network moved" signal, raised by
/// `ConnectionCommand::NetworkChanged`. It cuts short both a live session and a
/// backoff sleep, so a phone that has just landed on a working interface
/// re-dials immediately instead of sitting out a delay earned on a dead one.
pub(super) async fn supernode_ws_task(
    identity: Arc<Identity>,
    peer_id: String,
    candidates: Vec<String>,
    mut send_rx: mpsc::Receiver<WsMessage>,
    internal_tx: mpsc::Sender<InternalEvent>,
    reconnect_now: Arc<Notify>,
) {
    if candidates.is_empty() {
        warn!(
            "Supernode {} has no WebSocket candidates — task exiting",
            peer_id
        );
        return;
    }

    let mut backoff = RECONNECT_BASE;
    let mut candidate_idx: usize = 0;

    loop {
        let ws_url = &candidates[candidate_idx % candidates.len()];
        info!(
            "Connecting to supernode {} at {} (candidate {}/{})",
            peer_id,
            ws_url,
            candidate_idx % candidates.len() + 1,
            candidates.len()
        );
        // Whether this attempt reached a live session. Separates "this endpoint
        // is wrong" from "the link died under a working endpoint".
        let mut established = false;
        let outcome = connect_and_run_ws(
            &identity,
            &peer_id,
            ws_url,
            &mut send_rx,
            &internal_tx,
            &reconnect_now,
            &mut established,
        )
        .await;
        match outcome {
            Ok(WsSessionEnd::ChannelClosed) => {
                info!(
                    "Supernode {} WebSocket task stopping (outbound channel closed)",
                    peer_id
                );
                let _ = internal_tx
                    .send(InternalEvent::WsDisconnected {
                        peer_id: peer_id.clone(),
                    })
                    .await;
                break;
            }
            Ok(WsSessionEnd::NetworkChanged) => {
                info!(
                    "Supernode {} network changed; re-dialing {} immediately",
                    peer_id, ws_url
                );
                let _ = internal_tx
                    .send(InternalEvent::WsDisconnected {
                        peer_id: peer_id.clone(),
                    })
                    .await;
                // Same candidate, no backoff: nothing is wrong with the
                // endpoint, so rotating away from it would be a downgrade.
                backoff = RECONNECT_BASE;
                sleep(NETWORK_CHANGE_SETTLE).await;
            }
            Ok(WsSessionEnd::RemoteClosed) => {
                info!(
                    "Supernode {} WebSocket closed; reconnecting in {:?}",
                    peer_id, RECONNECT_BASE
                );
                let _ = internal_tx
                    .send(InternalEvent::WsDisconnected {
                        peer_id: peer_id.clone(),
                    })
                    .await;
                // Rotate candidate after a successful session ends so we probe
                // alternate endpoints if the primary is flapping.
                candidate_idx = candidate_idx.wrapping_add(1);
                backoff = RECONNECT_BASE;
                wait_before_retry(RECONNECT_BASE, &reconnect_now).await;
            }
            Err(e) => {
                let (wait, next) = backoff_after_error(backoff, established);
                backoff = next;
                warn!(
                    "Supernode {} WebSocket error on {}: {}; retry in {:?}",
                    peer_id, ws_url, e, wait
                );
                let _ = internal_tx
                    .send(InternalEvent::WsDisconnected {
                        peer_id: peer_id.clone(),
                    })
                    .await;
                // Prefer the next candidate after a connect/runtime failure.
                candidate_idx = candidate_idx.wrapping_add(1);
                wait_before_retry(wait, &reconnect_now).await;
            }
        }
    }
}

/// How long to wait before re-dialing after an errored attempt, and the backoff
/// to carry into the attempt after that.
///
/// `established` means the attempt reached a live session before it failed,
/// which proves the candidate itself is good — what died was the link under it.
/// Without that reset the backoff is only ever cleared by a *clean* close, so a
/// mobile client that changes network a few times — each change ending its
/// session with an error, not a close — climbs to the 60 s cap and stays there
/// for the rest of the process's life.
fn backoff_after_error(current: Duration, established: bool) -> (Duration, Duration) {
    let wait = if established { RECONNECT_BASE } else { current };
    (wait, (wait * 2).min(RECONNECT_MAX))
}

/// Sleep before the next dial, cut short by a network-change signal.
async fn wait_before_retry(delay: Duration, reconnect_now: &Notify) {
    tokio::select! {
        _ = sleep(delay) => {}
        _ = reconnect_now.notified() => {
            debug!("Network changed during backoff — dialing now");
        }
    }
}

async fn connect_and_run_ws(
    identity: &Identity,
    peer_id: &str,
    ws_url: &str,
    send_rx: &mut mpsc::Receiver<WsMessage>,
    internal_tx: &mpsc::Sender<InternalEvent>,
    reconnect_now: &Notify,
    established: &mut bool,
) -> std::result::Result<WsSessionEnd, Box<dyn std::error::Error + Send + Sync>> {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::connect_async;

    let (ws_stream, _) = tokio::time::timeout(CONNECT_TIMEOUT, connect_async(ws_url))
        .await
        .map_err(|_| format!("connect to {ws_url} timed out after {CONNECT_TIMEOUT:?}"))??;
    info!("WebSocket connected to supernode {}", peer_id);
    let (mut ws_sink, mut ws_stream) = ws_stream.split();

    // Send HELLO
    let hello = build_hello(identity)?;
    ws_sink.send(WsMessage::Text(hello)).await?;

    *established = true;
    let _ = internal_tx
        .send(InternalEvent::WsConnected {
            peer_id: peer_id.to_owned(),
        })
        .await;

    // I/O loop
    let mut ping_interval = tokio::time::interval(Duration::from_secs(PING_INTERVAL_S));
    // Delay, not Burst: after a stall we want one ping, not a catch-up volley.
    ping_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let idle_deadline = tokio::time::sleep(READ_IDLE_TIMEOUT);
    tokio::pin!(idle_deadline);

    loop {
        tokio::select! {
            // Outbound — `None` means the manager dropped send_tx (teardown).
            msg = send_rx.recv() => {
                match msg {
                    Some(msg) => {
                        ws_sink.send(msg).await?;
                    }
                    None => {
                        let _ = ws_sink.send(WsMessage::Close(None)).await;
                        return Ok(WsSessionEnd::ChannelClosed);
                    }
                }
            }

            // Inbound
            msg = ws_stream.next() => {
                // Any frame at all — Text, Pong, or the supernode's own Ping —
                // proves the path still carries traffic in our direction.
                idle_deadline
                    .as_mut()
                    .reset(tokio::time::Instant::now() + READ_IDLE_TIMEOUT);
                match msg {
                    Some(Ok(WsMessage::Text(text))) => {
                        match serde_json::from_str::<SignalingMessage>(&text) {
                            Ok(sm) => {
                                // Awaited send: inbound signaling must not be
                                // silently dropped when the manager is busy —
                                // backpressure the socket read instead.
                                let _ = internal_tx
                                    .send(InternalEvent::WsSignalingMessage {
                                        supernode_id: peer_id.to_owned(),
                                        msg: sm,
                                    })
                                    .await;
                            }
                            Err(e) => {
                                debug!("Ignoring non-signaling WS message: {}", e);
                            }
                        }
                    }
                    Some(Ok(WsMessage::Close(_))) | None => {
                        return Ok(WsSessionEnd::RemoteClosed);
                    }
                    Some(Ok(_)) => {} // binary, ping, pong — ignore
                    Some(Err(e)) => {
                        return Err(Box::new(e));
                    }
                }
            }

            // Keepalive
            _ = ping_interval.tick() => {
                ws_sink.send(WsMessage::Ping(vec![])).await?;
            }

            // Liveness. See READ_IDLE_TIMEOUT: this is the only thing that
            // rescues a socket stranded on a network we no longer have.
            _ = &mut idle_deadline => {
                return Err(format!(
                    "no traffic from supernode {peer_id} for {READ_IDLE_TIMEOUT:?} — link presumed dead"
                )
                .into());
            }

            // The platform told us the network moved. Do not wait for the
            // deadline above; the current socket is already worthless.
            _ = reconnect_now.notified() => {
                return Ok(WsSessionEnd::NetworkChanged);
            }
        }
    }
}

pub(super) fn build_hello(identity: &Identity) -> std::result::Result<String, serde_json::Error> {
    let sender = identity.public_id();
    let mut msg = SignalingMessage::new(MessageType::Hello, sender.clone());
    msg.payload
        .insert("public_id".to_owned(), Value::String(sender.clone()));
    msg.payload
        .insert("peer_id".to_owned(), Value::String(identity.peer_id()));
    // Sign
    if let Ok(canonical) = msg.canonical_bytes() {
        let sig = identity.sign(&canonical);
        use base64::Engine;
        msg.signature = Some(base64::engine::general_purpose::URL_SAFE.encode(sig));
    }
    msg.to_json()
}

#[cfg(test)]
mod tests {
    use super::*;

    use futures_util::{SinkExt, StreamExt};
    use tokio::net::TcpListener;
    use tokio_tungstenite::accept_async;

    /// What a test server does with each connection after the handshake.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum ServerBehaviour {
        /// Accept, then say nothing at all — the shape of a link stranded on a
        /// network the device no longer has. The socket is open, the peer is
        /// gone, and nothing will ever arrive.
        Silent,
        /// Accept, then send a frame well inside the idle deadline, the way a
        /// live supernode answers the periodic Ping with a Pong.
        Chatty,
    }

    /// Serve `behaviour` forever on loopback, reporting each accepted
    /// WebSocket handshake on the returned channel.
    async fn serve(behaviour: ServerBehaviour) -> (String, mpsc::Receiver<()>) {
        let (accepted_tx, accepted_rx) = mpsc::channel::<()>(16);
        // Bind before spawning so the caller has the address without having to
        // wait on the server task. Blocking the test thread for it would
        // deadlock the current-thread runtime the task needs to start on.
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");

        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let accepted_tx = accepted_tx.clone();
                tokio::spawn(async move {
                    let Ok(ws) = accept_async(stream).await else {
                        return;
                    };
                    let (mut sink, mut source) = ws.split();
                    let _ = accepted_tx.send(()).await;

                    if behaviour == ServerBehaviour::Chatty {
                        tokio::spawn(async move {
                            loop {
                                sleep(READ_IDLE_TIMEOUT / 3).await;
                                if sink.send(WsMessage::Text("{}".to_owned())).await.is_err() {
                                    return;
                                }
                            }
                        });
                    }
                    // Drain whatever the client sends (HELLO, pings) and never
                    // act on it. Dropping the read half instead would close the
                    // socket and hand the client the very disconnect these
                    // tests exist to prove it can find on its own.
                    while let Some(Ok(_)) = source.next().await {}
                });
            }
        });

        (format!("ws://{addr}"), accepted_rx)
    }

    /// Spawn the task under test against `url`, returning its event stream and
    /// the handle that signals a network change.
    fn spawn_task(
        url: &str,
    ) -> (
        mpsc::Receiver<InternalEvent>,
        Arc<Notify>,
        mpsc::Sender<WsMessage>,
        tokio::task::JoinHandle<()>,
    ) {
        let (internal_tx, internal_rx) = mpsc::channel::<InternalEvent>(64);
        let (send_tx, send_rx) = mpsc::channel::<WsMessage>(16);
        let reconnect_now = Arc::new(Notify::new());
        let task = tokio::spawn(supernode_ws_task(
            Arc::new(Identity::generate()),
            "supernode-under-test".to_owned(),
            vec![url.to_owned()],
            send_rx,
            internal_tx,
            Arc::clone(&reconnect_now),
        ));
        (internal_rx, reconnect_now, send_tx, task)
    }

    /// Await the next connect/disconnect transition, ignoring inbound traffic.
    async fn next_transition(rx: &mut mpsc::Receiver<InternalEvent>, within: Duration) -> String {
        let deadline = tokio::time::Instant::now() + within;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            match tokio::time::timeout(remaining, rx.recv()).await {
                Err(_) => return "<nothing>".to_owned(),
                Ok(None) => return "<closed>".to_owned(),
                Ok(Some(InternalEvent::WsConnected { .. })) => return "connected".to_owned(),
                Ok(Some(InternalEvent::WsDisconnected { .. })) => return "disconnected".to_owned(),
                Ok(Some(_)) => continue,
            }
        }
    }

    /// The regression this whole change exists for.
    ///
    /// A socket whose network has gone away does not error and does not
    /// deliver. Before the read deadline the task parked in `ws_stream.next()`
    /// forever: no `WsDisconnected`, so the manager never tore down the relay,
    /// never re-dialed, and never told the UI anything was wrong. The client
    /// was simply offline until it was restarted.
    #[tokio::test]
    async fn a_link_that_goes_silent_is_dropped_and_redialed() {
        let (url, mut accepted) = serve(ServerBehaviour::Silent).await;
        let (mut events, _reconnect, _send, task) = spawn_task(&url);

        assert!(accepted.recv().await.is_some(), "server saw the first dial");
        assert_eq!(
            next_transition(&mut events, Duration::from_secs(5)).await,
            "connected"
        );

        // Nothing ever arrives on this socket, so only the deadline can save it.
        assert_eq!(
            next_transition(&mut events, READ_IDLE_TIMEOUT * 4).await,
            "disconnected",
            "the idle deadline must give up on a silent link"
        );

        // And having given up, it must actually come back.
        assert!(
            accepted.recv().await.is_some(),
            "the task must re-dial after abandoning a dead link"
        );
        assert_eq!(
            next_transition(&mut events, Duration::from_secs(5)).await,
            "connected"
        );

        task.abort();
    }

    /// The guard on the fix above: the deadline must not mistake a quiet
    /// supernode for a dead one. Any inbound frame proves the path carries
    /// traffic and defers the deadline.
    #[tokio::test]
    async fn inbound_traffic_keeps_a_quiet_link_alive() {
        let (url, mut accepted) = serve(ServerBehaviour::Chatty).await;
        let (mut events, _reconnect, _send, task) = spawn_task(&url);

        assert!(accepted.recv().await.is_some());
        assert_eq!(
            next_transition(&mut events, Duration::from_secs(5)).await,
            "connected"
        );

        // Several deadlines worth of time, with only the periodic frames from
        // the server holding it open.
        assert_eq!(
            next_transition(&mut events, READ_IDLE_TIMEOUT * 4).await,
            "<nothing>",
            "a link that keeps delivering must not be torn down"
        );

        task.abort();
    }

    /// The platform fast path. Without it recovery is bounded by the idle
    /// deadline, which is deliberately generous — a minute and a half offline
    /// after every walk out of Wi-Fi range.
    #[tokio::test]
    async fn a_network_change_redials_without_waiting_for_the_deadline() {
        let (url, mut accepted) = serve(ServerBehaviour::Silent).await;
        let (mut events, reconnect, _send, task) = spawn_task(&url);

        assert!(accepted.recv().await.is_some());
        assert_eq!(
            next_transition(&mut events, Duration::from_secs(5)).await,
            "connected"
        );

        let signalled = tokio::time::Instant::now();
        reconnect.notify_one();

        assert_eq!(
            next_transition(&mut events, Duration::from_secs(5)).await,
            "disconnected"
        );
        assert!(
            accepted.recv().await.is_some(),
            "the signal must produce a fresh dial"
        );
        assert!(
            signalled.elapsed() < READ_IDLE_TIMEOUT,
            "the re-dial must not wait out the idle deadline (took {:?})",
            signalled.elapsed()
        );

        task.abort();
    }

    #[test]
    fn an_established_session_resets_the_backoff() {
        // Repeated failures against an endpoint that never connects escalate.
        let (wait, next) = backoff_after_error(Duration::from_secs(8), false);
        assert_eq!(wait, Duration::from_secs(8));
        assert_eq!(next, Duration::from_secs(16));

        // But a session that actually ran proves the endpoint is fine, so the
        // next attempt starts over rather than inheriting a delay earned
        // against a network that no longer exists.
        let (wait, next) = backoff_after_error(Duration::from_secs(60), true);
        assert_eq!(wait, RECONNECT_BASE);
        assert_eq!(next, RECONNECT_BASE * 2);
    }

    #[test]
    fn backoff_is_capped() {
        let (wait, next) = backoff_after_error(RECONNECT_MAX, false);
        assert_eq!(wait, RECONNECT_MAX);
        assert_eq!(next, RECONNECT_MAX, "backoff must not grow past its cap");
    }
}
