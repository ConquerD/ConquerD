//! x.ollama.v1 — local Ollama AI integration as a FeatureModule.
//!
//! Local-only: queries go to the user's own Ollama instance over HTTP streaming.
//! The capability is announced to peers as a presence signal (auth=public) so
//! they can see AI assistance is available; no data is sent to/from peers.
//!
//! `on_invoke` / `on_message` are intentional no-ops.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use conquerd_features::{AuthTier, CapabilityDescriptor, ChannelKind};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tracing::{debug, warn};

pub const DEFAULT_BASE_URL: &str = "http://localhost:11434";
pub const DEFAULT_MODEL: &str = "llama3";

/// Capability id advertised by this module.
pub const CAPABILITY_ID: &str = "x.ollama.v1";

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

/// Commands sent to the Ollama module task.
#[derive(Debug)]
pub enum OllamaCommand {
    /// Submit a streaming query.
    Query {
        request_id: String,
        prompt: String,
        system_prompt: String,
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
            .build()
            .unwrap_or_default();
        let m = Self {
            config,
            client,
            in_flight: HashMap::new(),
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

        let url = format!("{}/api/generate", self.config.base_url);
        let model = self.config.model.clone();
        let event_tx = self.event_tx.clone();
        let client = self.client.clone();

        tokio::spawn(async move {
            run_stream(
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

/// Fetch sorted model names from `GET <base_url>/api/tags` (4 s timeout).
pub async fn fetch_model_list(client: &Client, base_url: &str) -> Result<Vec<String>, String> {
    let url = format!("{}/api/tags", base_url.trim_end_matches('/'));
    let resp = client
        .get(&url)
        .timeout(Duration::from_secs(4))
        .send()
        .await
        .map_err(|e| format!("HTTP error: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("Ollama returned {}", resp.status()));
    }
    let body: TagsResponse = resp.json().await.map_err(|e| format!("Parse error: {e}"))?;
    let mut names: Vec<String> = body
        .models
        .into_iter()
        .map(|m| m.name)
        .filter(|n| !n.is_empty())
        .collect();
    names.sort();
    Ok(names)
}

#[allow(clippy::too_many_arguments)]
async fn run_stream(
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
