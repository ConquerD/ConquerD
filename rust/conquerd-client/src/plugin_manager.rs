//! Plugin manager — discovery and lifecycle for bespoke `x.*` feature modules.
//!
//! Extends the `conquerd-features` `NativeModuleLoader` consumer to map
//! user-persisted settings onto a concrete list of enabled modules, ready for
//! registration into the `FeatureRegistry`.

use std::collections::HashMap;

use tracing::info;

// ---------------------------------------------------------------------------
// Well-known plugin registry
// ---------------------------------------------------------------------------

/// All bespoke `x.*` module IDs this client knows about.
pub const REGISTERED_PLUGINS: &[&str] = &["x.ollama.v1"];

/// Human-readable metadata per plugin — consumed by the settings UI.
pub fn plugin_meta(plugin_id: &str) -> Option<PluginMeta> {
    match plugin_id {
        "x.ollama.v1" => Some(PluginMeta {
            name: "Ollama AI".into(),
            description: "Query your local Ollama instance from within ConquerD. \
                Responses stream locally; no data is sent to or received from peers."
                .into(),
        }),
        _ => None,
    }
}

/// Human-readable plugin metadata.
#[derive(Debug, Clone)]
pub struct PluginMeta {
    pub name: String,
    pub description: String,
}

// ---------------------------------------------------------------------------
// PluginConfig
// ---------------------------------------------------------------------------

/// Per-plugin key-value configuration (persisted via `SettingsModel`).
#[derive(Debug, Clone, Default)]
pub struct PluginConfig(pub HashMap<String, String>);

impl PluginConfig {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }
}

// ---------------------------------------------------------------------------
// PluginManager
// ---------------------------------------------------------------------------

/// Manages bespoke feature module lifecycle based on persisted settings.
pub struct PluginManager {
    /// plugin_id → enabled flag
    enabled: HashMap<String, bool>,
    /// plugin_id → config key-value map
    configs: HashMap<String, PluginConfig>,
}

impl PluginManager {
    pub fn new() -> Self {
        let mut enabled = HashMap::new();
        for &id in REGISTERED_PLUGINS {
            enabled.insert(id.to_owned(), false);
        }
        Self {
            enabled,
            configs: HashMap::new(),
        }
    }

    /// Load plugin enabled states and configs from a flat settings map.
    ///
    /// Keys are expected in the format `"plugin.<id>.enabled"` (bool as string)
    /// and `"plugin.<id>.config.<key>"` for per-plugin config values.
    pub fn load_from_map(&mut self, map: &HashMap<String, String>) {
        for &id in REGISTERED_PLUGINS {
            let key = format!("plugin.{id}.enabled");
            if let Some(v) = map.get(&key) {
                self.enabled.insert(id.to_owned(), v == "true" || v == "1");
            }
            let prefix = format!("plugin.{id}.config.");
            let cfg: HashMap<String, String> = map
                .iter()
                .filter(|(k, _)| k.starts_with(&prefix))
                .map(|(k, v)| (k[prefix.len()..].to_owned(), v.clone()))
                .collect();
            if !cfg.is_empty() {
                self.configs.insert(id.to_owned(), PluginConfig(cfg));
            }
        }
    }

    /// Load plugin state directly from the desktop `SettingsModel` flat keys
    /// (`ollama_enabled`, `ollama_base_url`, `ollama_model`).
    ///
    /// Used by the Qt bridge so settings authored via the QML settings dialog
    /// flow into the plugin runtime without requiring nested
    /// `plugin.<id>.*` keys on disk.
    pub fn load_from_settings(
        &mut self,
        ollama_enabled: bool,
        ollama_base_url: &str,
        ollama_model: &str,
    ) {
        self.enabled
            .insert("x.ollama.v1".to_owned(), ollama_enabled);
        let mut cfg = HashMap::new();
        if !ollama_base_url.is_empty() {
            cfg.insert("base_url".to_owned(), ollama_base_url.to_owned());
        }
        if !ollama_model.is_empty() {
            cfg.insert("model".to_owned(), ollama_model.to_owned());
        }
        if !cfg.is_empty() {
            self.configs
                .insert("x.ollama.v1".to_owned(), PluginConfig(cfg));
        }
    }

    /// Return `true` if the plugin is enabled.
    pub fn is_enabled(&self, plugin_id: &str) -> bool {
        self.enabled.get(plugin_id).copied().unwrap_or(false)
    }

    /// Enable or disable a plugin.
    pub fn set_enabled(&mut self, plugin_id: &str, enabled: bool) {
        if REGISTERED_PLUGINS.contains(&plugin_id) {
            self.enabled.insert(plugin_id.to_owned(), enabled);
        }
    }

    /// Return a config value for a plugin.
    pub fn config_value(&self, plugin_id: &str, key: &str) -> Option<&str> {
        self.configs.get(plugin_id)?.get(key)
    }

    /// Set a single config value for a plugin.
    pub fn set_config_value(&mut self, plugin_id: &str, key: &str, value: String) {
        self.configs
            .entry(plugin_id.to_owned())
            .or_default()
            .0
            .insert(key.to_owned(), value);
    }

    /// Returns the IDs of all currently enabled plugins.
    pub fn enabled_plugins(&self) -> Vec<&str> {
        self.enabled
            .iter()
            .filter(|(_, &enabled)| enabled)
            .map(|(id, _)| id.as_str())
            .collect()
    }

    /// Log the current plugin state.
    pub fn log_status(&self) {
        for &id in REGISTERED_PLUGINS {
            let enabled = self.is_enabled(id);
            info!(
                "[plugins] {} — {}",
                id,
                if enabled { "enabled" } else { "disabled" }
            );
        }
    }
}

impl Default for PluginManager {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_plugins_known() {
        assert!(REGISTERED_PLUGINS.contains(&"x.ollama.v1"));
    }

    #[test]
    fn disabled_by_default() {
        let pm = PluginManager::new();
        assert!(!pm.is_enabled("x.ollama.v1"));
    }

    #[test]
    fn set_enabled_and_query() {
        let mut pm = PluginManager::new();
        pm.set_enabled("x.ollama.v1", true);
        assert!(pm.is_enabled("x.ollama.v1"));
        assert_eq!(pm.enabled_plugins(), vec!["x.ollama.v1"]);
    }

    #[test]
    fn load_from_map() {
        let mut map = HashMap::new();
        map.insert("plugin.x.ollama.v1.enabled".into(), "true".into());
        map.insert(
            "plugin.x.ollama.v1.config.base_url".into(),
            "http://localhost:11434".into(),
        );
        let mut pm = PluginManager::new();
        pm.load_from_map(&map);
        assert!(pm.is_enabled("x.ollama.v1"));
        assert_eq!(
            pm.config_value("x.ollama.v1", "base_url"),
            Some("http://localhost:11434")
        );
    }

    #[test]
    fn load_from_settings_flat_keys() {
        let mut pm = PluginManager::new();
        pm.load_from_settings(true, "http://localhost:11434", "llama3");
        assert!(pm.is_enabled("x.ollama.v1"));
        assert_eq!(
            pm.config_value("x.ollama.v1", "base_url"),
            Some("http://localhost:11434")
        );
        assert_eq!(pm.config_value("x.ollama.v1", "model"), Some("llama3"));
    }

    #[test]
    fn load_from_settings_disabled() {
        let mut pm = PluginManager::new();
        pm.load_from_settings(false, "http://localhost:11434", "llama3");
        assert!(!pm.is_enabled("x.ollama.v1"));
    }

    #[test]
    fn unknown_plugin_ignored() {
        let mut pm = PluginManager::new();
        pm.set_enabled("x.unknown.v99", true);
        // Should not appear in enabled_plugins (it's not in REGISTERED_PLUGINS)
        assert!(!pm.enabled_plugins().contains(&"x.unknown.v99"));
    }

    #[test]
    fn plugin_meta_known() {
        let m = plugin_meta("x.ollama.v1").unwrap();
        assert_eq!(m.name, "Ollama AI");
    }

    #[test]
    fn plugin_meta_unknown_is_none() {
        assert!(plugin_meta("x.whatever.v1").is_none());
    }
}
