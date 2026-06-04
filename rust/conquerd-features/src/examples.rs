//! Reference bespoke `x.*` feature modules.
//!
//! These are **example** modules in the bespoke `x.<vendor>.*` namespace.
//! They are not part of the first-party `core.*` / `room.*` / `web.*` /
//! `game.*` catalogue and are **not** auto-registered by
//! [`register_client_modules`](crate::register_client_modules); a consumer must
//! opt in explicitly via [`register_example_modules`]. They exist to:
//!
//! * give third-party authors a complete, idiomatic template for the
//!   [`FeatureModule`] contract (descriptor + auth tier + explicit quota +
//!   stateful `on_invoke`), and
//! * demonstrate how a coordination feature composes with the opaque
//!   [`game.relay.v1`](crate::wellknown::game_relay_v1) datagram relay.
//!
//! ## `x.conquerd.matchmaker.v1`
//!
//! A minimal lobby/matchmaking control feature. Peers invoke it to create,
//! join, or leave a named game lobby. When a lobby fills to its
//! `max_players`, the module fires a "ready" hook carrying the final roster —
//! exactly the set of peers the host then wires together over
//! `game.relay.v1` for opaque, low-latency game-state datagrams.
//!
//! The framework already enforces the auth tier and per-feature quota before
//! `on_invoke` runs, so this module focuses purely on lobby state.

use std::collections::BTreeMap;
use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::{json, Value};

use crate::{
    descriptor::{AuthTier, CapabilityDescriptor, ChannelKind},
    module::{FeatureModule, InvocationContext, ModuleError, ModuleResult, PeerId},
    registry::FeatureRegistry,
    FeatureError,
};

/// Capability id for the reference matchmaker module.
pub const MATCHMAKER_ID: &str = "x.conquerd.matchmaker.v1";

/// Default lobby capacity when an invoker omits `max_players`.
const DEFAULT_MAX_PLAYERS: usize = 2;
/// Upper bound on lobby capacity to keep rosters (and game.relay fan-out) sane.
const MAX_LOBBY_CAPACITY: usize = 16;

/// Descriptor for [`Matchmaker`].
///
/// `kind = Request` (single-shot invoke), `auth = TrustedPeer` (you matchmake
/// with peers you already trust; the resulting session then moves to the
/// `room-member` tier of `game.relay.v1`). Quotas are modest — this is a
/// control-plane feature, not a data path.
pub fn x_conquerd_matchmaker_v1() -> CapabilityDescriptor {
    CapabilityDescriptor::new(MATCHMAKER_ID, "1.0", ChannelKind::Request)
        .with_auth(AuthTier::TrustedPeer)
        .with_params(json!({
            "actions": ["create", "join", "leave"],
            "default_max_players": DEFAULT_MAX_PLAYERS,
            "max_players_cap": MAX_LOBBY_CAPACITY,
            "pairs_with": "game.relay.v1",
            "quota_bytes_per_sec": 16 * 1024,
            "quota_datagrams_per_sec": 32,
        }))
}

/// Hook fired when a lobby fills to capacity. Receives `(game_id, roster)`.
type ReadyHook = Arc<dyn Fn(&str, &[PeerId]) + Send + Sync>;

#[derive(Clone)]
struct Lobby {
    max_players: usize,
    members: Vec<PeerId>,
}

/// Reference matchmaking module (`x.conquerd.matchmaker.v1`).
#[derive(Default)]
pub struct Matchmaker {
    lobbies: Mutex<BTreeMap<String, Lobby>>,
    on_ready: Option<ReadyHook>,
}

impl Matchmaker {
    /// Create a matchmaker with no ready-hook.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a matchmaker that calls `hook(game_id, roster)` whenever a lobby
    /// fills to its `max_players`. The roster is exactly the set of peers to
    /// connect over `game.relay.v1`.
    pub fn with_ready_hook(hook: impl Fn(&str, &[PeerId]) + Send + Sync + 'static) -> Self {
        Self {
            lobbies: Mutex::new(BTreeMap::new()),
            on_ready: Some(Arc::new(hook)),
        }
    }

    /// Current members of `game`'s lobby (empty if no such lobby). This is the
    /// roster a host hands to `game.relay.v1` once the lobby is ready.
    pub fn lobby_members(&self, game: &str) -> Vec<PeerId> {
        self.lobbies
            .lock()
            .get(game)
            .map(|l| l.members.clone())
            .unwrap_or_default()
    }

    /// Number of open lobbies.
    pub fn lobby_count(&self) -> usize {
        self.lobbies.lock().len()
    }

    /// True when `game`'s lobby exists and is full.
    pub fn is_ready(&self, game: &str) -> bool {
        self.lobbies
            .lock()
            .get(game)
            .is_some_and(|l| l.members.len() >= l.max_players)
    }

    fn parse_game(params: &Value) -> ModuleResult<String> {
        let game = params
            .get("game")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if game.is_empty() {
            return Err(ModuleError::InvalidParams("missing `game`".into()));
        }
        if game.len() > 64 {
            return Err(ModuleError::InvalidParams("`game` too long".into()));
        }
        Ok(game.to_owned())
    }

    fn handle(&self, caller: &PeerId, params: &Value) -> ModuleResult<()> {
        let action = params
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        let game = Self::parse_game(params)?;

        // Mutate under the lock; capture the roster to fire the ready hook
        // *after* releasing it so the callback can re-enter the module.
        let ready_roster: Option<Vec<PeerId>> = {
            let mut lobbies = self.lobbies.lock();
            match action {
                "create" => {
                    if lobbies.contains_key(&game) {
                        return Err(ModuleError::InvalidParams(format!(
                            "lobby `{game}` already exists"
                        )));
                    }
                    let max_players = params
                        .get("max_players")
                        .and_then(Value::as_u64)
                        .map(|n| n as usize)
                        .unwrap_or(DEFAULT_MAX_PLAYERS)
                        .clamp(1, MAX_LOBBY_CAPACITY);
                    let lobby = Lobby {
                        max_players,
                        members: vec![caller.clone()],
                    };
                    let ready = lobby.members.len() >= lobby.max_players;
                    let roster = lobby.members.clone();
                    lobbies.insert(game.clone(), lobby);
                    ready.then_some(roster)
                }
                "join" => {
                    let lobby = lobbies
                        .get_mut(&game)
                        .ok_or_else(|| ModuleError::InvalidParams(format!("no lobby `{game}`")))?;
                    if lobby.members.contains(caller) {
                        // Idempotent re-join.
                        None
                    } else if lobby.members.len() >= lobby.max_players {
                        return Err(ModuleError::PermissionDenied(format!(
                            "lobby `{game}` is full"
                        )));
                    } else {
                        lobby.members.push(caller.clone());
                        (lobby.members.len() >= lobby.max_players).then(|| lobby.members.clone())
                    }
                }
                "leave" => {
                    if let Some(lobby) = lobbies.get_mut(&game) {
                        lobby.members.retain(|p| p != caller);
                        if lobby.members.is_empty() {
                            lobbies.remove(&game);
                        }
                    }
                    None
                }
                other => {
                    return Err(ModuleError::InvalidParams(format!(
                        "unknown action `{other}`"
                    )));
                }
            }
        };

        if let (Some(roster), Some(hook)) = (ready_roster, self.on_ready.as_ref()) {
            hook(&game, &roster);
        }
        Ok(())
    }
}

impl FeatureModule for Matchmaker {
    fn descriptor(&self) -> CapabilityDescriptor {
        x_conquerd_matchmaker_v1()
    }

    fn on_invoke(&self, ctx: InvocationContext) -> ModuleResult<()> {
        self.handle(&ctx.peer, &ctx.params)
    }
}

/// Register the reference bespoke example modules on `registry`.
///
/// This is **opt-in** — bespoke `x.*` features are never auto-advertised. Call
/// it only from a consumer that actually wants to expose the examples (e.g. a
/// demo build or an integration test).
pub fn register_example_modules(registry: &FeatureRegistry) -> Result<(), FeatureError> {
    registry.register_module(Arc::new(Matchmaker::new()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn invoke(m: &Matchmaker, peer: &str, params: Value) -> ModuleResult<()> {
        m.on_invoke(InvocationContext {
            peer: peer.to_string(),
            params,
            channel_tag: None,
        })
    }

    #[test]
    fn descriptor_is_valid_bespoke_request() {
        let d = x_conquerd_matchmaker_v1();
        d.validate().unwrap();
        assert_eq!(d.namespace(), "x");
        assert_eq!(d.kind, ChannelKind::Request);
        assert_eq!(d.auth, AuthTier::TrustedPeer);
    }

    #[test]
    fn create_then_join_fills_lobby() {
        let m = Matchmaker::new();
        invoke(&m, "alice", json!({"action": "create", "game": "pong"})).unwrap();
        assert_eq!(m.lobby_members("pong"), vec!["alice".to_string()]);
        assert!(!m.is_ready("pong"));
        invoke(&m, "bob", json!({"action": "join", "game": "pong"})).unwrap();
        assert!(
            m.is_ready("pong"),
            "default capacity 2 → full after 2 joins"
        );
        assert_eq!(
            m.lobby_members("pong"),
            vec!["alice".to_string(), "bob".to_string()]
        );
    }

    #[test]
    fn ready_hook_fires_with_roster() {
        let fired = Arc::new(AtomicUsize::new(0));
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let f2 = fired.clone();
        let s2 = seen.clone();
        let m = Matchmaker::with_ready_hook(move |game, roster| {
            f2.fetch_add(1, Ordering::SeqCst);
            assert_eq!(game, "pong");
            *s2.lock() = roster.to_vec();
        });
        invoke(&m, "alice", json!({"action": "create", "game": "pong"})).unwrap();
        assert_eq!(fired.load(Ordering::SeqCst), 0, "not ready with 1/2");
        invoke(&m, "bob", json!({"action": "join", "game": "pong"})).unwrap();
        assert_eq!(fired.load(Ordering::SeqCst), 1, "fires once full");
        assert_eq!(*seen.lock(), vec!["alice".to_string(), "bob".to_string()]);
    }

    #[test]
    fn join_full_lobby_is_denied() {
        let m = Matchmaker::new();
        invoke(
            &m,
            "alice",
            json!({"action": "create", "game": "duel", "max_players": 1}),
        )
        .unwrap();
        assert!(m.is_ready("duel"), "capacity 1 → ready immediately");
        let err = invoke(&m, "bob", json!({"action": "join", "game": "duel"})).unwrap_err();
        assert!(matches!(err, ModuleError::PermissionDenied(_)));
    }

    #[test]
    fn rejoin_is_idempotent() {
        let m = Matchmaker::new();
        invoke(
            &m,
            "alice",
            json!({"action": "create", "game": "g", "max_players": 3}),
        )
        .unwrap();
        invoke(&m, "alice", json!({"action": "join", "game": "g"})).unwrap();
        assert_eq!(m.lobby_members("g"), vec!["alice".to_string()]);
    }

    #[test]
    fn leave_removes_member_and_empty_lobby() {
        let m = Matchmaker::new();
        invoke(
            &m,
            "alice",
            json!({"action": "create", "game": "g", "max_players": 2}),
        )
        .unwrap();
        invoke(&m, "bob", json!({"action": "join", "game": "g"})).unwrap();
        invoke(&m, "bob", json!({"action": "leave", "game": "g"})).unwrap();
        assert_eq!(m.lobby_members("g"), vec!["alice".to_string()]);
        invoke(&m, "alice", json!({"action": "leave", "game": "g"})).unwrap();
        assert_eq!(m.lobby_count(), 0, "empty lobby is removed");
    }

    #[test]
    fn duplicate_create_rejected() {
        let m = Matchmaker::new();
        invoke(&m, "alice", json!({"action": "create", "game": "g"})).unwrap();
        let err = invoke(&m, "bob", json!({"action": "create", "game": "g"})).unwrap_err();
        assert!(matches!(err, ModuleError::InvalidParams(_)));
    }

    #[test]
    fn unknown_action_and_missing_game_rejected() {
        let m = Matchmaker::new();
        let e1 = invoke(&m, "a", json!({"action": "frobnicate", "game": "g"})).unwrap_err();
        assert!(matches!(e1, ModuleError::InvalidParams(_)));
        let e2 = invoke(&m, "a", json!({"action": "create"})).unwrap_err();
        assert!(matches!(e2, ModuleError::InvalidParams(_)));
    }

    #[test]
    fn lobbies_are_isolated_per_game() {
        let m = Matchmaker::new();
        invoke(&m, "alice", json!({"action": "create", "game": "a"})).unwrap();
        invoke(&m, "bob", json!({"action": "create", "game": "b"})).unwrap();
        assert_eq!(m.lobby_members("a"), vec!["alice".to_string()]);
        assert_eq!(m.lobby_members("b"), vec!["bob".to_string()]);
        assert_eq!(m.lobby_count(), 2);
    }

    #[test]
    fn max_players_is_clamped() {
        let m = Matchmaker::new();
        invoke(
            &m,
            "alice",
            json!({"action": "create", "game": "huge", "max_players": 9999}),
        )
        .unwrap();
        // Fill up to the cap and confirm join past the cap is denied.
        for i in 0..(MAX_LOBBY_CAPACITY - 1) {
            invoke(
                &m,
                &format!("p{i}"),
                json!({"action": "join", "game": "huge"}),
            )
            .unwrap();
        }
        assert!(m.is_ready("huge"));
        let err = invoke(&m, "overflow", json!({"action": "join", "game": "huge"})).unwrap_err();
        assert!(matches!(err, ModuleError::PermissionDenied(_)));
    }

    #[test]
    fn registers_via_helper_and_dispatches_invoke() {
        let reg = FeatureRegistry::new();
        register_example_modules(&reg).unwrap();
        let tags = crate::ChannelTagRegistry::new();
        // Request-kind features are not datagram; invoke directly instead.
        let ctx = InvocationContext {
            peer: "alice".into(),
            params: json!({"action": "create", "game": "x"}),
            channel_tag: None,
        };
        reg.dispatch_invoke(MATCHMAKER_ID, ctx).unwrap();
        // Tag registry unused for Request kind — just assert it constructs.
        let _ = tags;
    }
}
