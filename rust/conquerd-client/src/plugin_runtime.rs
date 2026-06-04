//! Plugin runtime — instantiates enabled bespoke `x.*` feature modules.
//!
//! Sits between [`PluginManager`](crate::plugin_manager::PluginManager)
//! (settings-driven enable/config) and the live system: it
//! 1. registers each enabled plugin's [`CapabilityDescriptor`] in the
//!    shared [`FeatureRegistry`] so the descriptor is included in
//!    `CAPABILITY_ANNOUNCE` payloads, and
//! 2. prepares the plugin's owning task future and command/event channels
//!    without spawning — the caller is responsible for spawning the future
//!    inside a tokio runtime context via [`StartedPlugins::spawn_all`].
//!
//! Today only `x.ollama.v1` is supported; new plugin ids are added by
//! extending the `prepare_*` helpers and the `start` dispatcher.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use conquerd_features::FeatureRegistry;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::ollama_module::{
    self, OllamaCommand, OllamaConfig, OllamaEvent, OllamaModule, DEFAULT_BASE_URL, DEFAULT_MODEL,
};
use crate::plugin_manager::PluginManager;

/// Boxed future type used inside [`OllamaHandles`].
type BoxFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// Live handles to a prepared (not yet spawned) `x.ollama.v1` task.
pub struct OllamaHandles {
    pub cmd_tx: mpsc::Sender<OllamaCommand>,
    pub event_rx: mpsc::Receiver<OllamaEvent>,
    /// The background task future. Consume with `tokio::spawn(handles.task)`.
    pub task: BoxFuture,
}

/// Aggregate of every prepared plugin's live handles.
///
/// Returned by [`PluginRuntime::start`] so the application layer can:
/// - extract `cmd_tx` / `event_rx` *before* entering the tokio runtime, then
/// - call [`Self::spawn_all`] inside the runtime to launch the tasks.
#[derive(Default)]
pub struct StartedPlugins {
    pub ollama: Option<OllamaHandles>,
}

impl StartedPlugins {
    /// Spawn every prepared plugin task. Must be called from inside a tokio
    /// runtime (e.g. within `rt.block_on`). Plugin events should be consumed
    /// via the `event_rx` handles before calling this, or they will accumulate.
    pub fn spawn_tasks(self) {
        if let Some(h) = self.ollama {
            tokio::spawn(h.task);
            info!("[plugins] x.ollama.v1 task spawned");
        }
    }
}

/// One-shot starter that walks [`PluginManager::enabled_plugins`] and
/// prepares each enabled plugin (registers descriptor + builds channels).
///
/// **Does not spawn any tasks.** The caller extracts command/event handles,
/// then calls [`StartedPlugins::spawn_tasks`] inside a tokio runtime.
pub struct PluginRuntime;

impl PluginRuntime {
    /// Prepare every plugin currently marked enabled in *manager* and
    /// register its descriptor in *registry*.
    ///
    /// Disabled plugins are skipped silently. Already-registered
    /// descriptors (e.g. when called twice) are left in place — a
    /// duplicate-id error from the registry is treated as success.
    pub fn start(manager: &PluginManager, registry: &Arc<FeatureRegistry>) -> StartedPlugins {
        let mut started = StartedPlugins::default();
        for &id in crate::plugin_manager::REGISTERED_PLUGINS {
            if !manager.is_enabled(id) {
                continue;
            }
            match id {
                "x.ollama.v1" => {
                    started.ollama = Some(Self::prepare_ollama(manager, registry));
                }
                other => warn!("[plugins] no runtime handler for enabled plugin '{other}'"),
            }
        }
        started
    }

    /// Prepare `x.ollama.v1`: register descriptor and build channels + future.
    /// The future is NOT spawned here.
    fn prepare_ollama(manager: &PluginManager, registry: &Arc<FeatureRegistry>) -> OllamaHandles {
        let base_url = manager
            .config_value("x.ollama.v1", "base_url")
            .unwrap_or(DEFAULT_BASE_URL)
            .to_owned();
        let model = manager
            .config_value("x.ollama.v1", "model")
            .unwrap_or(DEFAULT_MODEL)
            .to_owned();
        let cfg = OllamaConfig { base_url, model };

        if let Err(e) = registry.register(ollama_module::descriptor()) {
            tracing::debug!("[plugins] ollama descriptor already registered: {e}");
        }

        let (cmd_tx, event_rx, task) = OllamaModule::split(cfg);
        info!("[plugins] x.ollama.v1 prepared");
        OllamaHandles {
            cmd_tx,
            event_rx,
            task: Box::pin(task),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conquerd_features::FeatureRegistry;

    #[tokio::test]
    async fn disabled_plugins_not_started() {
        let manager = PluginManager::new(); // all disabled by default
        let registry = Arc::new(FeatureRegistry::new());
        let started = PluginRuntime::start(&manager, &registry);
        assert!(started.ollama.is_none());
        assert!(registry.get("x.ollama.v1").is_none());
    }

    #[tokio::test]
    async fn enabled_ollama_registers_descriptor() {
        let mut manager = PluginManager::new();
        manager.set_enabled("x.ollama.v1", true);
        let registry = Arc::new(FeatureRegistry::new());
        let started = PluginRuntime::start(&manager, &registry);
        assert!(started.ollama.is_some());
        let d = registry.get("x.ollama.v1").expect("descriptor registered");
        assert_eq!(d.id, "x.ollama.v1");
        assert_eq!(d.version, "1.0");
        // Spawn the task then shut it down cleanly.
        if let Some(h) = started.ollama {
            tokio::spawn(h.task);
            let _ = h.cmd_tx.send(OllamaCommand::Shutdown).await;
        }
    }

    #[tokio::test]
    async fn double_start_does_not_panic_on_duplicate_descriptor() {
        let mut manager = PluginManager::new();
        manager.set_enabled("x.ollama.v1", true);
        let registry = Arc::new(FeatureRegistry::new());
        let s1 = PluginRuntime::start(&manager, &registry);
        let s2 = PluginRuntime::start(&manager, &registry);
        assert!(s1.ollama.is_some());
        assert!(s2.ollama.is_some());
        if let Some(h) = s1.ollama {
            tokio::spawn(h.task);
            let _ = h.cmd_tx.send(OllamaCommand::Shutdown).await;
        }
        if let Some(h) = s2.ollama {
            tokio::spawn(h.task);
            let _ = h.cmd_tx.send(OllamaCommand::Shutdown).await;
        }
    }
}
