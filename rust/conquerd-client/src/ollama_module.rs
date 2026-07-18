//! x.ollama.v1 — local Ollama AI integration as a FeatureModule.
//!
//! Local-only: queries go to the user's own Ollama instance over HTTP streaming.
//! The capability is announced to peers as a presence signal (auth=public) so
//! they can see AI assistance is available; no data is sent to/from peers.
//!
//! `on_invoke` / `on_message` are intentional no-ops.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use conquerd_features::{AuthTier, CapabilityDescriptor, ChannelKind};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::{mpsc, oneshot};
use tracing::debug;

/// Default Ollama base URL. Uses `127.0.0.1` (not `localhost`) so Windows
/// never resolves to IPv6 `::1` while the daemon is IPv4-only.
pub const DEFAULT_BASE_URL: &str = "http://127.0.0.1:11434";
pub const DEFAULT_MODEL: &str = "llama3";

/// Capability id advertised by this module.
pub const CAPABILITY_ID: &str = "x.ollama.v1";

/// Full assistant settings snapshot (shared by Qt bridge + headless).
#[derive(Debug, Clone)]
pub struct OllamaAssistantSettings {
    pub enabled: bool,
    pub base_url: String,
    pub model: String,
    pub system_prompt: String,
    pub auto_respond_direct: bool,
    pub auto_respond_room: bool,
}

impl Default for OllamaAssistantSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: DEFAULT_BASE_URL.to_owned(),
            model: DEFAULT_MODEL.to_owned(),
            system_prompt: "You are a helpful assistant.".to_owned(),
            auto_respond_direct: false,
            auto_respond_room: false,
        }
    }
}

impl OllamaAssistantSettings {
    /// Config consumed by the background Ollama task.
    pub fn to_config(&self) -> OllamaConfig {
        OllamaConfig {
            base_url: normalize_ollama_base_url(&self.base_url),
            model: self.model.clone(),
        }
    }
}

/// Read Ollama assistant fields from `$CONQUERD_HOME/settings.json`.
pub fn read_assistant_settings() -> OllamaAssistantSettings {
    let path = crate::identity::Identity::default_key_dir().join("settings.json");
    let Ok(txt) = std::fs::read_to_string(&path) else {
        return OllamaAssistantSettings::default();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) else {
        return OllamaAssistantSettings::default();
    };
    OllamaAssistantSettings {
        enabled: v
            .get("ollama_enabled")
            .and_then(|x| x.as_bool())
            .unwrap_or(false),
        base_url: v
            .get("ollama_base_url")
            .and_then(|x| x.as_str())
            .unwrap_or(DEFAULT_BASE_URL)
            .to_owned(),
        model: v
            .get("ollama_model")
            .and_then(|x| x.as_str())
            .unwrap_or(DEFAULT_MODEL)
            .to_owned(),
        system_prompt: v
            .get("ollama_system_prompt")
            .and_then(|x| x.as_str())
            .unwrap_or("You are a helpful assistant.")
            .to_owned(),
        auto_respond_direct: v
            .get("ollama_auto_respond_direct")
            .and_then(|x| x.as_bool())
            .unwrap_or(false),
        auto_respond_room: v
            .get("ollama_auto_respond_room")
            .and_then(|x| x.as_bool())
            .unwrap_or(false),
    }
}

/// Build the `x.ollama.v1` capability descriptor.
///
/// `OllamaModule.descriptor()`:
/// `auth=public`, zero per-peer byte/datagram quota (local-only).
pub fn descriptor() -> CapabilityDescriptor {
    CapabilityDescriptor::new(CAPABILITY_ID, "1.0", ChannelKind::Stream)
        .with_auth(AuthTier::Public)
        .with_params(json!({
            "quota_bytes_per_sec": 0,
            "quota_datagrams_per_sec": 0,
        }))
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A streamed token chunk from the Ollama API.
#[derive(Debug, Clone)]
pub struct OllamaChunk {
    pub request_id: String,
    pub text: String,
    pub done: bool,
}

/// Configuration consumed on every query (read-on-query for hot-reload).
#[derive(Debug, Clone)]
pub struct OllamaConfig {
    pub base_url: String,
    pub model: String,
}

impl Default for OllamaConfig {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_owned(),
            model: DEFAULT_MODEL.to_owned(),
        }
    }
}

/// Events emitted by the Ollama module task.
#[derive(Debug, Clone)]
pub enum OllamaEvent {
    /// Streamed token chunk.
    Chunk(OllamaChunk),
    /// HTTP or stream error.
    Error { request_id: String, message: String },
    /// Result of a `ListModels` command.
    /// `models` is sorted; `error` is empty on success.
    Models { models: Vec<String>, error: String },
}

/// One turn in a multi-message Ollama chat (`/api/chat`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatTurn {
    pub role: String,
    pub content: String,
}

impl ChatTurn {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".to_owned(),
            content: content.into(),
        }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".to_owned(),
            content: content.into(),
        }
    }
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".to_owned(),
            content: content.into(),
        }
    }
}

/// Stable conversation keys for auto-reply memory.
pub fn conversation_id_direct(peer_id: &str) -> String {
    format!("direct:{peer_id}")
}

/// Room history is keyed by `room_id` only.
///
/// Multi-homed clients receive the same logical room via different supernode
/// WS sessions (`inbound_supernode_id` rotates across the cluster). Including
/// the supernode in the key used to split Ollama memory and cancel scopes so
/// consecutive messages in one room looked like independent conversations.
pub fn conversation_id_room(room_id: &str) -> String {
    format!("room:{room_id}")
}

/// Max user+assistant turns kept per conversation (system is not counted).
pub const MAX_HISTORY_TURNS: usize = 12;
/// Soft cap per stored message body (chars) to bound memory / context size.
pub const MAX_TURN_CHARS: usize = 4_000;

/// Commands sent to the Ollama module task.
#[derive(Debug)]
pub enum OllamaCommand {
    /// Submit a streaming single-shot query (`/api/generate`) — no history.
    Query {
        request_id: String,
        prompt: String,
        system_prompt: String,
    },
    /// Multi-turn chat (`/api/chat`): appends `user_message` to `conversation_id`
    /// history, streams a reply, then stores the assistant response.
    Chat {
        request_id: String,
        /// e.g. `direct:<peer>` or `room:<room_id>`.
        conversation_id: String,
        user_message: String,
        system_prompt: String,
    },
    /// Drop history for one conversation (or all if empty).
    ClearConversation {
        conversation_id: String,
    },
    /// Cancel an in-flight query by request_id.
    Cancel {
        request_id: String,
    },
    /// Fetch the list of installed models from `GET <base_url>/api/tags`.
    /// The result is returned as `OllamaEvent::Models`.
    ListModels {
        base_url: String,
    },
    /// Update configuration (takes effect on next query).
    SetConfig(OllamaConfig),
    Shutdown,
}

// ---------------------------------------------------------------------------
// OllamaModule
// ---------------------------------------------------------------------------

/// Local-only Ollama streaming query manager.
pub struct OllamaModule {
    config: OllamaConfig,
    client: Client,
    /// Map of `request_id` → cancel sender.
    in_flight: HashMap<String, oneshot::Sender<()>>,
    /// Per-conversation multi-turn history (user/assistant only).
    conversations: Arc<std::sync::Mutex<HashMap<String, Vec<ChatTurn>>>>,
    event_tx: mpsc::Sender<OllamaEvent>,
    cmd_rx: mpsc::Receiver<OllamaCommand>,
}

impl OllamaModule {
    /// Create and split into `(cmd_tx, event_rx, task_future)`.
    pub fn split(
        config: OllamaConfig,
    ) -> (
        mpsc::Sender<OllamaCommand>,
        mpsc::Receiver<OllamaEvent>,
        impl std::future::Future<Output = ()> + Send,
    ) {
        let (event_tx, event_rx) = mpsc::channel::<OllamaEvent>(64);
        let (cmd_tx, cmd_rx) = mpsc::channel::<OllamaCommand>(32);
        let client = Client::builder()
            .timeout(Duration::from_secs(120))
            // Local Ollama must not be routed through HTTP_PROXY/HTTPS_PROXY.
            .no_proxy()
            .build()
            .unwrap_or_default();
        let m = Self {
            config,
            client,
            in_flight: HashMap::new(),
            conversations: Arc::new(std::sync::Mutex::new(HashMap::new())),
            event_tx,
            cmd_rx,
        };
        (cmd_tx, event_rx, m.run())
    }

    // -----------------------------------------------------------------------
    // Streaming query
    // -----------------------------------------------------------------------

    async fn start_query(&mut self, request_id: String, prompt: String, system_prompt: String) {
        // Cancel any existing task for this request_id
        if let Some(tx) = self.in_flight.remove(&request_id) {
            let _ = tx.send(());
        }

        let (cancel_tx, cancel_rx) = oneshot::channel::<()>();
        self.in_flight.insert(request_id.clone(), cancel_tx);

        let url = format!(
            "{}/api/generate",
            normalize_ollama_base_url(&self.config.base_url)
        );
        let model = self.config.model.clone();
        let event_tx = self.event_tx.clone();
        let client = self.client.clone();

        tokio::spawn(async move {
            run_generate_stream(
                client,
                url,
                model,
                request_id,
                prompt,
                system_prompt,
                cancel_rx,
                event_tx,
            )
            .await;
        });
    }

    /// Multi-turn chat with retained history for `conversation_id`.
    async fn start_chat(
        &mut self,
        request_id: String,
        conversation_id: String,
        user_message: String,
        system_prompt: String,
    ) {
        if let Some(tx) = self.in_flight.remove(&request_id) {
            let _ = tx.send(());
        }
        let (cancel_tx, cancel_rx) = oneshot::channel::<()>();
        self.in_flight.insert(request_id.clone(), cancel_tx);

        let user_text = truncate_turn(&user_message);
        // Append user turn + snapshot history for the request.
        let history_snapshot = {
            let mut guard = self.conversations.lock().unwrap_or_else(|e| e.into_inner());
            let turns = guard.entry(conversation_id.clone()).or_default();
            turns.push(ChatTurn::user(user_text));
            trim_history(turns);
            turns.clone()
        };

        let mut messages = Vec::with_capacity(history_snapshot.len() + 1);
        let sys = system_prompt.trim();
        if !sys.is_empty() {
            messages.push(ChatTurn::system(sys));
        }
        messages.extend(history_snapshot);

        let url = format!(
            "{}/api/chat",
            normalize_ollama_base_url(&self.config.base_url)
        );
        let model = self.config.model.clone();
        let event_tx = self.event_tx.clone();
        let client = self.client.clone();
        let conversations = Arc::clone(&self.conversations);
        let conv_id = conversation_id.clone();

        debug!(
            "[ollama] chat conv={} model={} history_msgs={}",
            conv_id,
            model,
            messages.len()
        );

        tokio::spawn(async move {
            let assistant = run_chat_stream(
                client, url, model, request_id, messages, cancel_rx, event_tx,
            )
            .await;
            if let Some(text) = assistant {
                let text = truncate_turn(&text);
                if text.is_empty() {
                    return;
                }
                if let Ok(mut guard) = conversations.lock() {
                    let turns = guard.entry(conv_id).or_default();
                    turns.push(ChatTurn::assistant(text));
                    trim_history(turns);
                }
            }
        });
    }

    // -----------------------------------------------------------------------
    // Event loop
    // -----------------------------------------------------------------------

    async fn run(mut self) {
        while let Some(cmd) = self.cmd_rx.recv().await {
            match cmd {
                OllamaCommand::Shutdown => {
                    // Cancel all in-flight queries
                    for (_, tx) in self.in_flight.drain() {
                        let _ = tx.send(());
                    }
                    break;
                }
                OllamaCommand::Cancel { request_id } => {
                    if let Some(tx) = self.in_flight.remove(&request_id) {
                        let _ = tx.send(());
                    }
                }
                OllamaCommand::SetConfig(cfg) => {
                    self.config = cfg;
                }
                OllamaCommand::Query {
                    request_id,
                    prompt,
                    system_prompt,
                } => {
                    self.start_query(request_id, prompt, system_prompt).await;
                }
                OllamaCommand::Chat {
                    request_id,
                    conversation_id,
                    user_message,
                    system_prompt,
                } => {
                    self.start_chat(request_id, conversation_id, user_message, system_prompt)
                        .await;
                }
                OllamaCommand::ClearConversation { conversation_id } => {
                    if let Ok(mut guard) = self.conversations.lock() {
                        if conversation_id.is_empty() {
                            guard.clear();
                        } else {
                            guard.remove(&conversation_id);
                        }
                    }
                }
                OllamaCommand::ListModels { base_url } => {
                    let client = self.client.clone();
                    let event_tx = self.event_tx.clone();
                    tokio::spawn(async move {
                        let ev = match fetch_model_list(&client, &base_url).await {
                            Ok(models) => OllamaEvent::Models {
                                models,
                                error: String::new(),
                            },
                            Err(e) => OllamaEvent::Models {
                                models: vec![],
                                error: e,
                            },
                        };
                        let _ = event_tx.send(ev).await;
                    });
                }
            }
        }
    }
}

fn truncate_turn(s: &str) -> String {
    let t = s.trim();
    if t.chars().count() <= MAX_TURN_CHARS {
        return t.to_owned();
    }
    let truncated: String = t.chars().take(MAX_TURN_CHARS.saturating_sub(1)).collect();
    format!("{truncated}…")
}

/// Keep the last `MAX_HISTORY_TURNS * 2` user/assistant messages.
fn trim_history(turns: &mut Vec<ChatTurn>) {
    let max = MAX_HISTORY_TURNS.saturating_mul(2);
    if turns.len() > max {
        let drain = turns.len() - max;
        turns.drain(0..drain);
    }
}

// ---------------------------------------------------------------------------
// Streaming implementation
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct GenerateRequest<'a> {
    model: &'a str,
    prompt: &'a str,
    #[serde(skip_serializing_if = "str::is_empty")]
    system: &'a str,
    stream: bool,
}

#[derive(Deserialize)]
struct GenerateChunk {
    response: Option<String>,
    #[serde(default)]
    done: bool,
}

#[derive(Deserialize)]
struct TagsResponse {
    #[serde(default)]
    models: Vec<TagsModel>,
}

#[derive(Deserialize)]
struct TagsModel {
    name: String,
}

/// Fetch sorted model names from `GET <base_url>/api/tags` (8 s timeout).
pub async fn fetch_model_list(client: &Client, base_url: &str) -> Result<Vec<String>, String> {
    // Prefer a concrete loopback host: some Windows setups resolve `localhost`
    // to `::1` while Ollama only listens on 127.0.0.1.
    let base = normalize_ollama_base_url(base_url);
    let url = format!("{base}/api/tags");
    let resp = client
        .get(&url)
        .timeout(Duration::from_secs(8))
        .send()
        .await
        .map_err(|e| format!("HTTP error talking to {url}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("Ollama returned {} for {url}", resp.status()));
    }
    let body: TagsResponse = resp
        .json()
        .await
        .map_err(|e| format!("Parse error from {url}: {e}"))?;
    let mut names: Vec<String> = body
        .models
        .into_iter()
        .map(|m| m.name)
        .filter(|n| !n.is_empty())
        .collect();
    names.sort();
    Ok(names)
}

/// Normalize an Ollama base URL for local use.
///
/// Maps `http://localhost:…` → `http://127.0.0.1:…` so we never depend on
/// IPv6 `::1` resolution when the daemon only bound IPv4.
pub fn normalize_ollama_base_url(base_url: &str) -> String {
    let trimmed = base_url.trim().trim_end_matches('/');
    if let Some(rest) = trimmed.strip_prefix("http://localhost") {
        return format!("http://127.0.0.1{rest}");
    }
    if let Some(rest) = trimmed.strip_prefix("https://localhost") {
        return format!("https://127.0.0.1{rest}");
    }
    if let Some(rest) = trimmed.strip_prefix("http://[::1]") {
        return format!("http://127.0.0.1{rest}");
    }
    trimmed.to_owned()
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: &'a [ChatTurn],
    stream: bool,
}

#[derive(Deserialize)]
struct ChatStreamChunk {
    message: Option<ChatStreamMessage>,
    #[serde(default)]
    done: bool,
}

#[derive(Deserialize)]
struct ChatStreamMessage {
    #[serde(default)]
    content: String,
}

#[allow(clippy::too_many_arguments)]
async fn run_generate_stream(
    client: Client,
    url: String,
    model: String,
    request_id: String,
    prompt: String,
    system_prompt: String,
    mut cancel_rx: oneshot::Receiver<()>,
    event_tx: mpsc::Sender<OllamaEvent>,
) {
    let body = GenerateRequest {
        model: &model,
        prompt: &prompt,
        system: &system_prompt,
        stream: true,
    };

    let resp = tokio::select! {
        r = client.post(&url).json(&body).send() => r,
        _ = &mut cancel_rx => return,
    };

    let resp = match resp {
        Ok(r) if r.status().is_success() => r,
        Ok(r) => {
            let _ = event_tx.try_send(OllamaEvent::Error {
                request_id,
                message: format!("Ollama API returned {}", r.status()),
            });
            return;
        }
        Err(e) => {
            let _ = event_tx.try_send(OllamaEvent::Error {
                request_id,
                message: format!("HTTP error: {e}"),
            });
            return;
        }
    };

    use futures_util::StreamExt;
    let mut stream = resp.bytes_stream();
    let mut buf = Vec::<u8>::new();

    loop {
        let item = tokio::select! {
            item = stream.next() => item,
            _ = &mut cancel_rx => break,
        };

        let chunk = match item {
            Some(Ok(b)) => b,
            Some(Err(e)) => {
                let _ = event_tx.try_send(OllamaEvent::Error {
                    request_id,
                    message: format!("Stream error: {e}"),
                });
                return;
            }
            None => break,
        };

        buf.extend_from_slice(chunk.as_ref());

        // Each newline-delimited JSON object is one chunk
        while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
            let line = buf.drain(..=pos).collect::<Vec<_>>();
            let s = match std::str::from_utf8(&line) {
                Ok(s) => s.trim().to_owned(),
                Err(_) => continue,
            };
            if s.is_empty() {
                continue;
            }
            match serde_json::from_str::<GenerateChunk>(&s) {
                Ok(gc) => {
                    let text = gc.response.unwrap_or_default();
                    let done = gc.done;
                    if !text.is_empty() || done {
                        let _ = event_tx.try_send(OllamaEvent::Chunk(OllamaChunk {
                            request_id: request_id.clone(),
                            text,
                            done,
                        }));
                    }
                    if done {
                        return;
                    }
                }
                Err(e) => {
                    debug!("Ollama JSON parse error: {e} | line: {s}");
                }
            }
        }
    }

    // Stream ended without explicit done=true
    let _ = event_tx.try_send(OllamaEvent::Chunk(OllamaChunk {
        request_id,
        text: String::new(),
        done: true,
    }));
}

/// Stream `/api/chat`. Returns the full assistant text when the stream completes
/// successfully (for history retention). `None` on cancel/error.
#[allow(clippy::too_many_arguments)]
async fn run_chat_stream(
    client: Client,
    url: String,
    model: String,
    request_id: String,
    messages: Vec<ChatTurn>,
    mut cancel_rx: oneshot::Receiver<()>,
    event_tx: mpsc::Sender<OllamaEvent>,
) -> Option<String> {
    let body = ChatRequest {
        model: &model,
        messages: &messages,
        stream: true,
    };

    let resp = tokio::select! {
        r = client.post(&url).json(&body).send() => r,
        _ = &mut cancel_rx => return None,
    };

    let resp = match resp {
        Ok(r) if r.status().is_success() => r,
        Ok(r) => {
            let _ = event_tx.try_send(OllamaEvent::Error {
                request_id,
                message: format!("Ollama chat API returned {}", r.status()),
            });
            return None;
        }
        Err(e) => {
            let _ = event_tx.try_send(OllamaEvent::Error {
                request_id,
                message: format!("HTTP error: {e}"),
            });
            return None;
        }
    };

    use futures_util::StreamExt;
    let mut stream = resp.bytes_stream();
    let mut buf = Vec::<u8>::new();
    let mut assistant = String::new();

    loop {
        let item = tokio::select! {
            item = stream.next() => item,
            _ = &mut cancel_rx => return None,
        };

        let chunk = match item {
            Some(Ok(b)) => b,
            Some(Err(e)) => {
                let _ = event_tx.try_send(OllamaEvent::Error {
                    request_id,
                    message: format!("Stream error: {e}"),
                });
                return None;
            }
            None => break,
        };

        buf.extend_from_slice(chunk.as_ref());

        while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
            let line = buf.drain(..=pos).collect::<Vec<_>>();
            let s = match std::str::from_utf8(&line) {
                Ok(s) => s.trim().to_owned(),
                Err(_) => continue,
            };
            if s.is_empty() {
                continue;
            }
            match serde_json::from_str::<ChatStreamChunk>(&s) {
                Ok(gc) => {
                    let text = gc.message.map(|m| m.content).unwrap_or_default();
                    let done = gc.done;
                    if !text.is_empty() {
                        assistant.push_str(&text);
                        let _ = event_tx.try_send(OllamaEvent::Chunk(OllamaChunk {
                            request_id: request_id.clone(),
                            text,
                            done: false,
                        }));
                    }
                    if done {
                        let _ = event_tx.try_send(OllamaEvent::Chunk(OllamaChunk {
                            request_id: request_id.clone(),
                            text: String::new(),
                            done: true,
                        }));
                        return Some(assistant);
                    }
                }
                Err(e) => {
                    debug!("Ollama chat JSON parse error: {e} | line: {s}");
                }
            }
        }
    }

    let _ = event_tx.try_send(OllamaEvent::Chunk(OllamaChunk {
        request_id,
        text: String::new(),
        done: true,
    }));
    if assistant.is_empty() {
        None
    } else {
        Some(assistant)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_url_trims_trailing_slash() {
        let base = normalize_ollama_base_url("http://127.0.0.1:11434/");
        let url = format!("{base}/api/tags");
        assert_eq!(url, "http://127.0.0.1:11434/api/tags");
    }

    #[test]
    fn normalize_maps_localhost_to_ipv4_loopback() {
        assert_eq!(
            normalize_ollama_base_url("http://localhost:11434"),
            "http://127.0.0.1:11434"
        );
        assert_eq!(
            normalize_ollama_base_url("http://localhost:11434/"),
            "http://127.0.0.1:11434"
        );
        assert_eq!(
            normalize_ollama_base_url("http://127.0.0.1:11434"),
            "http://127.0.0.1:11434"
        );
    }

    #[test]
    fn trim_history_keeps_last_turns() {
        let mut turns = Vec::new();
        for i in 0..30 {
            turns.push(ChatTurn::user(format!("u{i}")));
            turns.push(ChatTurn::assistant(format!("a{i}")));
        }
        trim_history(&mut turns);
        assert_eq!(turns.len(), MAX_HISTORY_TURNS * 2);
        assert_eq!(turns[0].content, format!("u{}", 30 - MAX_HISTORY_TURNS));
    }

    #[test]
    fn conversation_ids_are_stable() {
        assert_eq!(conversation_id_direct("abc"), "direct:abc");
        assert_eq!(conversation_id_room("room1"), "room:room1");
        // Multi-home must not split history by supernode path.
        assert_eq!(
            conversation_id_room("5919ee78b42b260c"),
            conversation_id_room("5919ee78b42b260c")
        );
    }

    #[test]
    fn tags_response_parses_model_names() {
        let json = r#"{
            "models": [
                {"name": "llama3.2:latest", "size": 1},
                {"name": "mistral:7b", "size": 2},
                {"name": "", "size": 3}
            ]
        }"#;
        let body: TagsResponse = serde_json::from_str(json).expect("parse tags");
        let mut names: Vec<String> = body
            .models
            .into_iter()
            .map(|m| m.name)
            .filter(|n| !n.is_empty())
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec!["llama3.2:latest".to_owned(), "mistral:7b".to_owned()]
        );
    }

    #[tokio::test]
    async fn list_models_command_emits_error_on_unreachable_host() {
        let (cmd_tx, mut event_rx, task) = OllamaModule::split(OllamaConfig {
            base_url: "http://127.0.0.1:9".to_owned(), // closed port
            model: DEFAULT_MODEL.to_owned(),
        });
        tokio::spawn(task);
        cmd_tx
            .send(OllamaCommand::ListModels {
                base_url: "http://127.0.0.1:9".to_owned(),
            })
            .await
            .expect("send ListModels");
        let ev = tokio::time::timeout(Duration::from_secs(6), event_rx.recv())
            .await
            .expect("timeout waiting for Models event")
            .expect("channel closed");
        match ev {
            OllamaEvent::Models { models, error } => {
                assert!(models.is_empty());
                assert!(!error.is_empty(), "expected HTTP error message");
            }
            other => panic!("unexpected event: {other:?}"),
        }
        let _ = cmd_tx.send(OllamaCommand::Shutdown).await;
    }
}
