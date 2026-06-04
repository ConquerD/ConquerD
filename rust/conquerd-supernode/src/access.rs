// ConquerD Supernode — access.rs
// Access control: trait + built-in implementations (open, TOS, ad, code).

use std::collections::HashSet;

use crate::config::AccessMode;

/// Access controller determines whether a peer gets relay access immediately
/// or must visit the web portal first.
pub trait AccessController: Send + Sync {
    /// Return true → grant relay immediately. False → redirect to portal.
    fn check_access(&self, peer_id: &str) -> bool;

    /// Called after access is granted via portal.
    #[allow(dead_code)]
    fn on_peer_granted(&self, _peer_id: &str) {}

    /// The portal entry path for this access mode.
    #[allow(dead_code)]
    fn portal_entry_path(&self) -> &str {
        "/portal"
    }

    /// Display name for stats.
    fn mode_name(&self) -> &str;
}

/// Always grants immediately.
pub struct OpenAccessController;

impl AccessController for OpenAccessController {
    fn check_access(&self, _peer_id: &str) -> bool {
        true
    }

    fn mode_name(&self) -> &str {
        "open"
    }
}

/// Requires TOS acceptance via web portal.
pub struct TOSAccessController {
    accepted: parking_lot::RwLock<HashSet<String>>,
}

impl TOSAccessController {
    pub fn new() -> Self {
        Self {
            accepted: parking_lot::RwLock::new(HashSet::new()),
        }
    }
}

impl AccessController for TOSAccessController {
    fn check_access(&self, peer_id: &str) -> bool {
        self.accepted.read().contains(peer_id)
    }

    fn on_peer_granted(&self, peer_id: &str) {
        self.accepted.write().insert(peer_id.to_string());
    }

    fn portal_entry_path(&self) -> &str {
        "/tos"
    }

    fn mode_name(&self) -> &str {
        "tos"
    }
}

/// Requires watching an ad/timer via web portal.
pub struct AdGateAccessController {
    granted: parking_lot::RwLock<HashSet<String>>,
}

impl AdGateAccessController {
    pub fn new() -> Self {
        Self {
            granted: parking_lot::RwLock::new(HashSet::new()),
        }
    }
}

impl AccessController for AdGateAccessController {
    fn check_access(&self, peer_id: &str) -> bool {
        self.granted.read().contains(peer_id)
    }

    fn on_peer_granted(&self, peer_id: &str) {
        self.granted.write().insert(peer_id.to_string());
    }

    fn portal_entry_path(&self) -> &str {
        "/ad"
    }

    fn mode_name(&self) -> &str {
        "ad"
    }
}

/// Requires entering an access code via web portal.
pub struct CodeGateAccessController {
    #[allow(dead_code)]
    code: String,
    granted: parking_lot::RwLock<HashSet<String>>,
}

impl CodeGateAccessController {
    pub fn new(code: String) -> Self {
        Self {
            code,
            granted: parking_lot::RwLock::new(HashSet::new()),
        }
    }

    #[allow(dead_code)]
    pub fn check_code(&self, submitted: &str) -> bool {
        submitted == self.code
    }
}

impl AccessController for CodeGateAccessController {
    fn check_access(&self, peer_id: &str) -> bool {
        self.granted.read().contains(peer_id)
    }

    fn on_peer_granted(&self, peer_id: &str) {
        self.granted.write().insert(peer_id.to_string());
    }

    fn portal_entry_path(&self) -> &str {
        "/code"
    }

    fn mode_name(&self) -> &str {
        "code"
    }
}

/// Create the appropriate access controller from config.
pub fn create_access_controller(mode: AccessMode, code: &str) -> Box<dyn AccessController> {
    match mode {
        AccessMode::Open => Box::new(OpenAccessController),
        AccessMode::Tos => Box::new(TOSAccessController::new()),
        AccessMode::Ad => Box::new(AdGateAccessController::new()),
        AccessMode::Code => Box::new(CodeGateAccessController::new(code.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── OpenAccessController ────────────────────────────────────────────────

    #[test]
    fn open_grants_any_peer() {
        let ac = OpenAccessController;
        assert!(ac.check_access("peer-abc"));
        assert!(ac.check_access(""));
    }

    #[test]
    fn open_mode_name() {
        assert_eq!(OpenAccessController.mode_name(), "open");
    }

    #[test]
    fn open_portal_entry_path_is_default() {
        assert_eq!(OpenAccessController.portal_entry_path(), "/portal");
    }

    // ── TOSAccessController ─────────────────────────────────────────────────

    #[test]
    fn tos_denies_before_grant() {
        let ac = TOSAccessController::new();
        assert!(!ac.check_access("peer-1"));
    }

    #[test]
    fn tos_grants_after_on_peer_granted() {
        let ac = TOSAccessController::new();
        ac.on_peer_granted("peer-1");
        assert!(ac.check_access("peer-1"));
        assert!(!ac.check_access("peer-2"));
    }

    #[test]
    fn tos_portal_path_and_mode_name() {
        let ac = TOSAccessController::new();
        assert_eq!(ac.portal_entry_path(), "/tos");
        assert_eq!(ac.mode_name(), "tos");
    }

    // ── AdGateAccessController ──────────────────────────────────────────────

    #[test]
    fn ad_gate_denies_before_grant() {
        let ac = AdGateAccessController::new();
        assert!(!ac.check_access("peer-x"));
    }

    #[test]
    fn ad_gate_grants_after_on_peer_granted() {
        let ac = AdGateAccessController::new();
        ac.on_peer_granted("peer-x");
        assert!(ac.check_access("peer-x"));
        assert!(!ac.check_access("peer-y"));
    }

    #[test]
    fn ad_gate_portal_path_and_mode_name() {
        let ac = AdGateAccessController::new();
        assert_eq!(ac.portal_entry_path(), "/ad");
        assert_eq!(ac.mode_name(), "ad");
    }

    // ── CodeGateAccessController ────────────────────────────────────────────

    #[test]
    fn code_gate_denies_before_grant() {
        let ac = CodeGateAccessController::new("secret".into());
        assert!(!ac.check_access("peer-z"));
    }

    #[test]
    fn code_gate_check_code_correct() {
        let ac = CodeGateAccessController::new("secret".into());
        assert!(ac.check_code("secret"));
        assert!(!ac.check_code("wrong"));
        assert!(!ac.check_code(""));
    }

    #[test]
    fn code_gate_grants_after_on_peer_granted() {
        let ac = CodeGateAccessController::new("secret".into());
        ac.on_peer_granted("peer-z");
        assert!(ac.check_access("peer-z"));
        assert!(!ac.check_access("peer-w"));
    }

    #[test]
    fn code_gate_portal_path_and_mode_name() {
        let ac = CodeGateAccessController::new("x".into());
        assert_eq!(ac.portal_entry_path(), "/code");
        assert_eq!(ac.mode_name(), "code");
    }

    // ── factory ─────────────────────────────────────────────────────────────

    #[test]
    fn factory_open_grants_immediately() {
        let ac = create_access_controller(AccessMode::Open, "irrelevant");
        assert!(ac.check_access("anyone"));
        assert_eq!(ac.mode_name(), "open");
    }

    #[test]
    fn factory_tos_denies_initially() {
        let ac = create_access_controller(AccessMode::Tos, "irrelevant");
        assert!(!ac.check_access("anyone"));
        assert_eq!(ac.mode_name(), "tos");
    }

    #[test]
    fn factory_ad_denies_initially() {
        let ac = create_access_controller(AccessMode::Ad, "irrelevant");
        assert!(!ac.check_access("anyone"));
        assert_eq!(ac.mode_name(), "ad");
    }

    #[test]
    fn factory_code_denies_initially() {
        let ac = create_access_controller(AccessMode::Code, "mycode");
        assert!(!ac.check_access("anyone"));
        assert_eq!(ac.mode_name(), "code");
    }

    #[test]
    fn concurrent_grant_and_check_tos() {
        use std::sync::Arc;
        let ac = Arc::new(TOSAccessController::new());
        let ac2 = ac.clone();
        let handle = std::thread::spawn(move || {
            ac2.on_peer_granted("peer-concurrent");
        });
        handle.join().unwrap();
        assert!(ac.check_access("peer-concurrent"));
    }
}
