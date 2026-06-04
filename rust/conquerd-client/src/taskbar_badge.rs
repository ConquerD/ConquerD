//! Taskbar badge — unread count and missed call overlays.
//!
//! On Windows: renders a QPixmap badge and sets it as the taskbar overlay via
//! Qt's QIcon. On other platforms updates the system tray icon only.
//!
//! In the Rust client this is a lightweight data-only module; the actual
//! icon rendering happens in QML/Qt (the badge count is a property on
//! AppBridge). This module tracks the badge state and provides helpers
//! consumed by the bridge.

use tracing::debug;

// ---------------------------------------------------------------------------
// Badge state
// ---------------------------------------------------------------------------

/// Aggregate badge state for the taskbar and tray icon.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BadgeState {
    pub unread_messages: u32,
    pub missed_calls: u32,
}

impl BadgeState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_clear(&self) -> bool {
        self.unread_messages == 0 && self.missed_calls == 0
    }

    /// Short label for the badge overlay: number, "!" for calls-only, or "".
    pub fn label(&self) -> String {
        if self.is_clear() {
            String::new()
        } else if self.unread_messages > 0 {
            if self.unread_messages > 99 {
                "99+".to_owned()
            } else {
                self.unread_messages.to_string()
            }
        } else {
            "!".to_owned() // missed call only
        }
    }
}

// ---------------------------------------------------------------------------
// Badge manager
// ---------------------------------------------------------------------------

/// Tracks badge state and emits an update callback when it changes.
///
/// The callback is called on every `set_*` call that causes a state change.
/// In the Qt client the callback queues an `AppBridge::badge_count_changed`
/// signal update.
pub struct TaskbarBadge {
    state: BadgeState,
    on_changed: Box<dyn Fn(&BadgeState) + Send + 'static>,
}

impl TaskbarBadge {
    pub fn new(on_changed: impl Fn(&BadgeState) + Send + 'static) -> Self {
        Self {
            state: BadgeState::default(),
            on_changed: Box::new(on_changed),
        }
    }

    pub fn state(&self) -> &BadgeState {
        &self.state
    }

    pub fn set_unread(&mut self, count: u32) {
        if self.state.unread_messages != count {
            self.state.unread_messages = count;
            debug!("[badge] unread={count}");
            (self.on_changed)(&self.state);
        }
    }

    pub fn set_missed_calls(&mut self, count: u32) {
        if self.state.missed_calls != count {
            self.state.missed_calls = count;
            debug!("[badge] missed_calls={count}");
            (self.on_changed)(&self.state);
        }
    }

    pub fn clear(&mut self) {
        if !self.state.is_clear() {
            self.state = BadgeState::default();
            debug!("[badge] cleared");
            (self.on_changed)(&self.state);
        }
    }

    pub fn increment_missed_calls(&mut self) {
        let new_val = self.state.missed_calls + 1;
        self.set_missed_calls(new_val);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_empty_when_clear() {
        assert_eq!(BadgeState::default().label(), "");
    }

    #[test]
    fn label_shows_unread_count() {
        let s = BadgeState {
            unread_messages: 5,
            missed_calls: 0,
        };
        assert_eq!(s.label(), "5");
    }

    #[test]
    fn label_caps_at_99_plus() {
        let s = BadgeState {
            unread_messages: 150,
            missed_calls: 0,
        };
        assert_eq!(s.label(), "99+");
    }

    #[test]
    fn label_exclamation_for_missed_call_only() {
        let s = BadgeState {
            unread_messages: 0,
            missed_calls: 2,
        };
        assert_eq!(s.label(), "!");
    }

    #[test]
    fn callback_fires_on_change() {
        let fired = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let fired_clone = std::sync::Arc::clone(&fired);

        let mut badge = TaskbarBadge::new(move |_| {
            fired_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });

        badge.set_unread(3);
        badge.set_unread(3); // no change — no fire
        badge.set_unread(0);
        badge.increment_missed_calls();
        badge.clear();

        assert_eq!(fired.load(std::sync::atomic::Ordering::SeqCst), 4);
    }

    #[test]
    fn clear_is_idempotent() {
        let count = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let c = std::sync::Arc::clone(&count);
        let mut badge = TaskbarBadge::new(move |_| {
            c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });

        badge.clear(); // already clear — should not fire
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 0);
    }
}
