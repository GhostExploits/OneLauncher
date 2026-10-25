//! Loopback telemetry bridge between the SpiderBridge Minecraft mod and the
//! local agent.
//!
//! The mod runs inside the Minecraft client, so it cannot hold the device
//! bearer token and must never talk to Discord. Instead the agent listens on an
//! ephemeral loopback port, publishes `bridge.json` in its data directory, and
//! authenticates every connection with a 256-bit secret generated fresh for
//! this agent run. Nothing in this module is reachable from another machine:
//! the listener is bound to `127.0.0.1`, never `0.0.0.0`.
//!
//! Security boundary, stated plainly: any process running as the *same*
//! operating-system user can read `bridge.json` and connect. That is the same
//! boundary that already protects `agent.json`, and the bridge deliberately
//! exposes only read-only telemetry and a status request. It is not a defense
//! against a hostile same-user process, and it is not advertised as one.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};

use crate::AgentError;

/// Wire protocol between the mod and the agent. Bumped only for breaking
/// changes; the mod refuses to talk to a different number and says so.
///
/// Version 2 adds payload-bearing requests, chat history, chat submission, and
/// policy-checked command submission. Version 1 peers are refused explicitly.
pub const BRIDGE_PROTOCOL: u32 = 2;
/// File the agent publishes so the mod can find the port and credential.
pub const ENDPOINT_FILE: &str = "bridge.json";
/// The mod's file format version.
pub const ENDPOINT_SCHEMA: u32 = 1;
/// A single newline-delimited JSON message. Chat history is the largest
/// legitimate payload, so this must stay above its validated byte cap.
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;
/// Telemetry older than this is reported as stale, never as live.
pub const STALE_AFTER_SECONDS: i64 = 10;
/// How long the agent waits for the mod to answer a live request. Chat and
/// command submissions are queued onto the game thread, which can pause.
pub const REQUEST_TIMEOUT_MS: u64 = 5_000;
/// How long the mod has to complete the handshake.
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
/// A connected mod that stops sending anything is dropped.
const IDLE_TIMEOUT: Duration = Duration::from_secs(90);
/// Bounded concurrency: a workstation rarely runs more than a few clients.
const MAX_CONNECTIONS: usize = 8;
/// Reject telemetry pushed faster than this. The mod samples at 1 s, so a
/// well-behaved client never hits it.
const MIN_TELEMETRY_INTERVAL: Duration = Duration::from_millis(200);
/// Messages allowed per connection in one second before it is closed.
const MAX_MESSAGES_PER_SECOND: u32 = 40;
/// Bounds for a reply payload. The transport cap applies first; this keeps a
/// single chat/action reply from consuming the whole allowance.
pub const MAX_REPLY_DATA_BYTES: usize = 48 * 1024;

pub const DEFAULT_CHAT_RETENTION_SECONDS: u32 = 300;
pub const DEFAULT_CHAT_MAX_MESSAGES: u32 = 200;
pub const MIN_CHAT_RETENTION_SECONDS: u32 = 30;
pub const MAX_CHAT_RETENTION_SECONDS: u32 = 3_600;
pub const MIN_CHAT_MESSAGES: u32 = 20;
pub const MAX_CHAT_MESSAGES: u32 = 500;

/// Owner-controlled bridge policy. The agent sends the chat-collection settings
/// in the welcome message; a mod that was not told to collect must not collect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BridgePolicy {
    pub chat_collection: bool,
    pub chat_retention_seconds: u32,
    pub chat_max_messages: u32,
    pub allow_chat_send: bool,
    pub allow_minecraft_command: bool,
}

impl Default for BridgePolicy {
    fn default() -> Self {
        Self {
            chat_collection: false,
            chat_retention_seconds: DEFAULT_CHAT_RETENTION_SECONDS,
            chat_max_messages: DEFAULT_CHAT_MAX_MESSAGES,
            allow_chat_send: false,
            allow_minecraft_command: false,
        }
    }
}

impl BridgePolicy {
    pub fn bounded(mut self) -> Self {
        self.chat_retention_seconds = self
            .chat_retention_seconds
            .clamp(MIN_CHAT_RETENTION_SECONDS, MAX_CHAT_RETENTION_SECONDS);
        self.chat_max_messages = self
            .chat_max_messages
            .clamp(MIN_CHAT_MESSAGES, MAX_CHAT_MESSAGES);
        self
    }
}


const SECRET_BYTES: usize = 32;

/// Everything the agent publishes for the mod.
#[derive(Debug, Serialize, Deserialize)]
pub struct EndpointFile {
    pub schema: u32,
    pub protocol: u32,
    pub port: u16,
    /// Hex-encoded, generated at agent start, removed at agent exit.
    pub secret: String,
    pub agent_version: String,
    pub agent_pid: u32,
    pub updated_at: DateTime<Utc>,
}

/// The state of the game as reported by the mod.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GameState {
    /// Minecraft is on the title screen or a menu.
    Menu,
    /// A single-player or multiplayer world is loaded.
    InWorld,
    /// A world is loading, or the player entity does not exist yet.
    NoPlayer,
}

/// Minecraft and mod versions, reported by the mod itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BridgeGame {
    pub mc_version: String,
    pub loader: String,
    pub loader_version: String,
    pub bridge_version: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BridgePlayer {
    pub name: String,
    pub health: Option<f32>,
    pub food_level: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BridgeWorld {
    /// Registry id such as `minecraft:overworld`.
    pub dimension: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BridgePosition {
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub yaw: f32,
    pub pitch: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BridgePerformance {
    pub fps: Option<u32>,
    pub frame_time_ms: Option<f32>,
}

/// Baritone is detected from the mods folder, but no control API is wired up.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BridgeBaritone {
    pub detected: bool,
    /// `unavailable` until a verified public API is integrated.
    pub integration: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BridgeTelemetry {
    pub schema: u32,
    pub game: BridgeGame,
    pub state: GameState,
    pub player: Option<BridgePlayer>,
    pub world: Option<BridgeWorld>,
    /// `None` when the player is not in a world. Never zero-filled.
    pub position: Option<BridgePosition>,
    pub performance: Option<BridgePerformance>,
    pub baritone: BridgeBaritone,
    /// When the mod sampled this data, on the mod's clock.
    pub captured_at: DateTime<Utc>,
    /// How long the Minecraft process has been running.
    pub uptime_seconds: u64,
}

impl BridgeTelemetry {
    /// Structural validation of data that arrived from the mod. The mod is
    /// trusted only as far as the OS user boundary, so every field is bounded
    /// before it can reach Discord.
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != ENDPOINT_SCHEMA {
            return Err(format!("unsupported telemetry schema {}", self.schema));
        }
        for (label, value) in [
            ("mc_version", &self.game.mc_version),
            ("loader", &self.game.loader),
            ("loader_version", &self.game.loader_version),
            ("bridge_version", &self.game.bridge_version),
        ] {
            validate_text(label, value, 64)?;
        }
        if let Some(player) = &self.player {
            validate_text("player name", &player.name, 32)?;
            if let Some(health) = player.health
                && (!health.is_finite() || !(0.0..=1024.0).contains(&health))
            {
                return Err("player health is out of range".into());
            }
            if let Some(food) = player.food_level
                && food > 20
            {
                return Err("player food level is out of range".into());
            }
        }
        if let Some(world) = &self.world {
            validate_text("dimension", &world.dimension, 128)?;
        }
        if let Some(position) = &self.position {
            for value in [position.x, position.y, position.z] {
                if !value.is_finite() {
                    return Err("position contains a non-finite coordinate".into());
                }
            }
            if position.x.abs() > 30_000_000.0
                || position.z.abs() > 30_000_000.0
                || !(-4096.0..=4096.0).contains(&position.y)
            {
                return Err("position is outside the playable world".into());
            }
            if !position.yaw.is_finite() || !position.pitch.is_finite() {
                return Err("orientation contains a non-finite angle".into());
            }
        }
        if let Some(performance) = &self.performance {
            if let Some(fps) = performance.fps
                && fps > 20_000
            {
                return Err("reported fps is implausible".into());
            }
            if let Some(frame) = performance.frame_time_ms
                && (!frame.is_finite() || !(0.0..=10_000.0).contains(&frame))
            {
                return Err("reported frame time is implausible".into());
            }
        }
        // A position without a world would be a fabricated coordinate.
        if self.position.is_some() && (self.world.is_none() || self.state != GameState::InWorld) {
            return Err("a position was reported without a loaded world".into());
        }
        Ok(())
    }

    /// True when the sample is recent enough to present as live.
    pub fn is_fresh(&self, now: DateTime<Utc>) -> bool {
        let age = now.signed_duration_since(self.captured_at).num_seconds();
        (-5..=STALE_AFTER_SECONDS).contains(&age)
    }
}

fn validate_text(label: &str, value: &str, max: usize) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!("{label} is empty"));
    }
    if value.chars().count() > max {
        return Err(format!("{label} is longer than {max} characters"));
    }
    if value.chars().any(char::is_control) {
        return Err(format!("{label} contains control characters"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Wire messages
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMessage {
    Hello(ClientHello),
    Telemetry { telemetry: Box<BridgeTelemetry> },
    Reply(BridgeReply),
    Bye,
}

#[derive(Debug, Deserialize)]
struct ClientHello {
    protocol: u32,
    secret: String,
    pid: u32,
    game_dir: String,
    bridge_version: String,
    mc_version: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ServerMessage {
    Welcome {
        protocol: u32,
        session_id: String,
        agent_version: String,
        agent_pid: u32,
        server_time: DateTime<Utc>,
        /// Collection is disabled unless the owner opted in. The mod must not
        /// buffer chat when this is false.
        chat_collection: bool,
        chat_retention_seconds: u32,
        chat_max_messages: u32,
    },
    Request {
        id: String,
        kind: RequestKind,
        #[serde(default)]
        payload: serde_json::Value,
    },
    Error {
        code: &'static str,
        message: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestKind {
    /// Return the current telemetry sample immediately.
    Status,
    /// Return the bounded in-memory chat buffer.
    #[serde(rename = "chat")]
    ChatHistory,
    /// Submit one chat line through the game client.
    ChatSend,
    /// Submit one policy-checked command through the game client.
    Command,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BridgeReply {
    pub id: String,
    pub ok: bool,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub telemetry: Option<Box<BridgeTelemetry>>,
    /// Kind-specific response. Never trusted blindly: the request method that
    /// asked for it validates the shape and limits.
    #[serde(default)]
    pub data: Option<Box<serde_json::Value>>,
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

/// Per-connection state retained for requests and freshness reporting.
struct Link {
    telemetry: BridgeTelemetry,
    /// `None` until the first telemetry message. A placeholder sample is never
    /// treated as fresh, and no synthetic "zero" coordinates are invented.
    received_at: Option<Instant>,
    requests: mpsc::UnboundedSender<ServerMessage>,
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<BridgeReply>>>>,
    /// Identifies the socket that owns this entry. A reconnecting mod replaces
    /// the entry, and the older socket must not keep writing into it.
    connection: Arc<()>,
}

/// The `pid -> session` mapping the agent refreshes on every poll.
#[derive(Clone)]
struct Binding {
    session_id: String,
    game_dir: PathBuf,
}

struct Shared {
    secret: Vec<u8>,
    agent_version: String,
    agent_pid: u32,
    policy: BridgePolicy,
    bindings: Mutex<HashMap<u32, Binding>>,
    links: Mutex<HashMap<String, Link>>,
    next_request: Mutex<u64>,
}

/// A running bridge. Dropping it stops the listener and removes the endpoint
/// file, so a stale port/secret is never left behind after the agent exits.
pub struct BridgeServer {
    shared: Arc<Shared>,
    endpoint_path: PathBuf,
    accept: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for BridgeServer {
    fn drop(&mut self) {
        if let Some(accept) = self.accept.take() {
            accept.abort();
        }
        let _ = std::fs::remove_file(&self.endpoint_path);
    }
}

impl BridgeServer {
    /// Bind the loopback listener and publish the endpoint file. The port is
    /// chosen by the OS so a second agent (or an unrelated program) cannot
    /// collide with it.
    pub async fn start(agent_version: &str) -> Result<Self, AgentError> {
        Self::start_in(&crate::config::data_dir()?, agent_version).await
    }

    /// Start with the owner's bridge policy. The agent uses this path; the
    /// policy is sent to the mod in the welcome message.
    pub async fn start_with_policy(
        agent_version: &str,
        policy: BridgePolicy,
    ) -> Result<Self, AgentError> {
        Self::start_in_with_policy(&crate::config::data_dir()?, agent_version, policy).await
    }

    /// Same as [`start`], but publishes the endpoint file in an explicit
    /// directory. Tests use a temporary directory so they never touch the real
    /// agent data directory.
    pub async fn start_in(dir: &Path, agent_version: &str) -> Result<Self, AgentError> {
        Self::start_in_with_policy(dir, agent_version, BridgePolicy::default()).await
    }

    pub async fn start_in_with_policy(
        dir: &Path,
        agent_version: &str,
        policy: BridgePolicy,
    ) -> Result<Self, AgentError> {
        let secret = random_secret()?;
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let port = listener.local_addr()?.port();
        let endpoint_path = dir.join(ENDPOINT_FILE);
        std::fs::create_dir_all(dir)?;

        let shared = Arc::new(Shared {
            secret: secret.clone(),
            agent_version: agent_version.to_string(),
            agent_pid: std::process::id(),
            policy: policy.bounded(),
            bindings: Mutex::new(HashMap::new()),
            links: Mutex::new(HashMap::new()),
            next_request: Mutex::new(0),
        });

        let endpoint = EndpointFile {
            schema: ENDPOINT_SCHEMA,
            protocol: BRIDGE_PROTOCOL,
            port,
            secret: hex::encode(&secret),
            agent_version: agent_version.to_string(),
            agent_pid: std::process::id(),
            updated_at: Utc::now(),
        };
        write_endpoint(&endpoint_path, &endpoint)?;

        let accept = tokio::spawn(accept_loop(listener, shared.clone()));

        Ok(Self {
            shared,
            endpoint_path,
            accept: Some(accept),
        })
    }

    #[must_use]
    pub fn endpoint_path(&self) -> &Path {
        &self.endpoint_path
    }

    /// Refresh which process belongs to which agent session. Links whose
    /// process disappeared are dropped so a stale bridge cannot answer for a
    /// session that has closed.
    pub fn apply_sessions(&self, sessions: &[(u32, String, PathBuf)]) {
        let mut bindings = self
            .shared
            .bindings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        bindings.clear();
        for (pid, session_id, game_dir) in sessions {
            bindings.insert(
                *pid,
                Binding {
                    session_id: session_id.clone(),
                    game_dir: game_dir.clone(),
                },
            );
        }
        let known: std::collections::HashSet<&str> =
            sessions.iter().map(|(_, id, _)| id.as_str()).collect();
        let mut links = self
            .shared
            .links
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        links.retain(|session_id, _| known.contains(session_id.as_str()));
    }

    /// The latest telemetry the mod reported for this session, if any. The
    /// caller decides whether it is fresh enough to show.
    #[must_use]
    pub fn snapshot(&self, session_id: &str) -> Option<BridgeTelemetry> {
        let links = self
            .shared
            .links
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        links.get(session_id).map(|link| link.telemetry.clone())
    }

    /// Seconds since the last telemetry message, if a mod is connected.
    #[must_use]
    pub fn age_seconds(&self, session_id: &str) -> Option<i64> {
        let links = self
            .shared
            .links
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        links
            .get(session_id)
            .and_then(|link| link.received_at)
            .map(|received| received.elapsed().as_secs() as i64)
    }

    #[must_use]
    pub fn is_connected(&self, session_id: &str) -> bool {
        let links = self
            .shared
            .links
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        links.contains_key(session_id)
    }

    #[must_use]
    pub fn connected_count(&self) -> usize {
        let links = self
            .shared
            .links
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        links.len()
    }

    /// Ask the mod for a fresh sample. This is a live round trip, not a cache
    /// read: `/coords` must never present stale or remembered coordinates.
    pub async fn request_status(&self, session_id: &str) -> Result<BridgeTelemetry, String> {
        let reply = self
            .request(session_id, RequestKind::Status, serde_json::json!({}))
            .await?;
        let telemetry = reply
            .telemetry
            .ok_or_else(|| "the SpiderBridge reply contained no telemetry".to_string())?;
        telemetry.validate()?;
        Ok(*telemetry)
    }

    /// Fetch the mod's bounded, in-memory chat buffer. The mod's `session_id` is
    /// checked against the requested session so a reply cannot be attributed to
    /// another running instance.
    pub async fn request_chat(
        &self,
        session_id: &str,
        payload: crate::protocol::ChatLogsPayload,
    ) -> Result<crate::protocol::ChatHistory, String> {
        if !self.shared.policy.chat_collection {
            return Err("chat history collection is disabled on this device".into());
        }
        let bounded = payload.bounded();
        let reply = self
            .request(
                session_id,
                RequestKind::ChatHistory,
                serde_json::json!({
                    "limit": bounded.limit,
                    "include_system": bounded.include_system,
                    "include_action_bar": bounded.include_action_bar,
                }),
            )
            .await?;
        let data = reply
            .data
            .ok_or_else(|| "the SpiderBridge reply contained no chat data".to_string())?;
        let value = data
            .get("chat")
            .ok_or_else(|| "the SpiderBridge reply contained no chat history".to_string())?;
        let history: crate::protocol::ChatHistory = serde_json::from_value(value.clone())
            .map_err(|_| "the mod returned malformed chat history".to_string())?;
        history.validate()?;
        if history.session_id != session_id {
            return Err(
                "the chat history came from a different Minecraft session; it was discarded".into(),
            );
        }
        if !history.collection_enabled {
            return Err(
                "chat collection is disabled in the game; enable it in the Remote settings and reconnect".into(),
            );
        }
        Ok(history)
    }

    /// Submit a chat line through the game client.
    pub async fn request_chat_send(
        &self,
        session_id: &str,
        message: &str,
    ) -> Result<BridgeAction, String> {
        if !self.shared.policy.allow_chat_send {
            return Err("remote chat sending is disabled on this device".into());
        }
        let reply = self
            .request(
                session_id,
                RequestKind::ChatSend,
                serde_json::json!({ "message": message }),
            )
            .await?;
        parse_action(reply.data.as_deref())
    }

    /// Hand a policy-checked command to the game client for submission.
    pub async fn request_command(
        &self,
        session_id: &str,
        command: &str,
    ) -> Result<BridgeAction, String> {
        if !self.shared.policy.allow_minecraft_command {
            return Err("remote Minecraft commands are disabled on this device".into());
        }
        let reply = self
            .request(
                session_id,
                RequestKind::Command,
                serde_json::json!({ "command": command }),
            )
            .await?;
        parse_action(reply.data.as_deref())
    }

    async fn request(
        &self,
        session_id: &str,
        kind: RequestKind,
        payload: serde_json::Value,
    ) -> Result<BridgeReply, String> {
        let (request_id, sender, pending) = {
            let links = self
                .shared
                .links
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let link = links.get(session_id).ok_or_else(|| {
                "the SpiderBridge mod is not connected for this session".to_string()
            })?;
            let mut counter = self
                .shared
                .next_request
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *counter = counter.wrapping_add(1);
            let id = format!("req-{}", *counter);
            drop(counter);
            (id, link.requests.clone(), link.pending.clone())
        };

        let (tx, rx) = oneshot::channel();
        {
            let mut pending = pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            pending.insert(request_id.clone(), tx);
        }

        let message = ServerMessage::Request {
            id: request_id.clone(),
            kind,
            payload,
        };
        if sender.send(message).is_err() {
            pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&request_id);
            return Err("the SpiderBridge connection closed before the request was sent".into());
        }

        let reply = match tokio::time::timeout(Duration::from_millis(REQUEST_TIMEOUT_MS), rx).await {
            Ok(Ok(reply)) => reply,
            Ok(Err(_)) => {
                return Err("the SpiderBridge connection closed while waiting for a reply".into());
            }
            Err(_) => {
                pending
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(&request_id);
                return Err(format!(
                    "the SpiderBridge mod did not answer within {} seconds; the game may be paused",
                    REQUEST_TIMEOUT_MS / 1000
                ));
            }
        };

        if reply.id != request_id {
            return Err("the SpiderBridge reply did not match the request id".into());
        }
        if let Some(message) = &reply.message
            && (message.chars().count() > 1_024 || message.chars().any(char::is_control))
        {
            return Err("the SpiderBridge reply contained an invalid message".into());
        }
        if let Some(data) = &reply.data {
            let size = serde_json::to_vec(data.as_ref())
                .map_err(|_| "the SpiderBridge reply data was not serialisable".to_string())?
                .len();
            if size > MAX_REPLY_DATA_BYTES {
                return Err("the SpiderBridge reply data was larger than the accepted limit".into());
            }
        }
        if !reply.ok {
            return Err(reply
                .message
                .unwrap_or_else(|| "the SpiderBridge mod refused the request".into()));
        }
        Ok(reply)
    }
}

/// The mod accepted and handed an action to the game client. This is *not* a
/// claim that the server executed it. `uncertain` means the client started the
/// action but could not confirm it, so it may still have been submitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeAction {
    pub accepted: bool,
    pub uncertain: bool,
    pub detail: Option<String>,
}

fn parse_action(data: Option<&serde_json::Value>) -> Result<BridgeAction, String> {
    let data = data.ok_or_else(|| "the SpiderBridge reply contained no action status".to_string())?;
    let action = data
        .get("action")
        .ok_or_else(|| "the SpiderBridge reply contained no action status".to_string())?;
    let accepted = action
        .get("accepted")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| "the SpiderBridge action status was malformed".to_string())?;
    let uncertain = action
        .get("uncertain")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if accepted && uncertain {
        return Err("the SpiderBridge action status was contradictory".into());
    }
    let detail = match action.get("detail").and_then(serde_json::Value::as_str) {
        Some(detail) => {
            if detail.chars().count() > 512 || detail.chars().any(char::is_control) {
                return Err("the SpiderBridge action detail was invalid".into());
            }
            Some(detail.to_string())
        }
        None => None,
    };
    Ok(BridgeAction {
        accepted,
        uncertain,
        detail,
    })
}

async fn accept_loop(listener: TcpListener, shared: Arc<Shared>) {
    let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    loop {
        let Ok((stream, _addr)) = listener.accept().await else {
            return;
        };
        if active.load(std::sync::atomic::Ordering::Relaxed) >= MAX_CONNECTIONS {
            // Refuse rather than queue: the mod retries with backoff.
            drop(stream);
            continue;
        }
        active.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let shared = shared.clone();
        let active = active.clone();
        tokio::spawn(async move {
            if let Err(err) = handle_connection(stream, shared.clone()).await {
                tracing::debug!(error = %err, "SpiderBridge connection ended");
            }
            shared.forget_closed();
            active.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        });
    }
}

impl Shared {
    /// Drop links whose writer task has gone away. Telemetry cleanup by session
    /// happens in `apply_sessions`, so this only removes dead handles.
    fn forget_closed(&self) {
        let stale: Vec<String> = {
            let links = self
                .links
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            links
                .iter()
                .filter(|(_, link)| link.requests.is_closed())
                .map(|(id, _)| id.clone())
                .collect()
        };
        if stale.is_empty() {
            return;
        }
        let mut links = self
            .links
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for id in stale {
            links.remove(&id);
        }
    }
}

async fn handle_connection(stream: TcpStream, shared: Arc<Shared>) -> Result<(), String> {
    stream.set_nodelay(true).ok();
    let (read_half, mut write_half) = stream.into_split();
    // A buffered reader is required for correctness, not speed: a single TCP
    // read can contain a telemetry message and a reply back to back, and the
    // second line must not be discarded.
    let mut reader = BufReader::new(read_half);

    let hello_line = tokio::time::timeout(HELLO_TIMEOUT, read_line(&mut reader))
        .await
        .map_err(|_| "handshake timed out".to_string())??;

    let message: ClientMessage =
        serde_json::from_slice(&hello_line).map_err(|_| "malformed handshake".to_string())?;
    let ClientMessage::Hello(hello) = message else {
        return write_error(&mut write_half, "expected_hello", "the first message must be hello").await;
    };

    if hello.protocol != BRIDGE_PROTOCOL {
        let message = format!(
            "the mod speaks bridge protocol {} but this agent speaks {BRIDGE_PROTOCOL}; update one of them",
            hello.protocol
        );
        let _ = write_error(&mut write_half, "protocol_mismatch", &message).await;
        return Err(message);
    }
    if !secret_matches(&shared.secret, &hello.secret) {
        let _ = write_error(&mut write_half, "unauthorized", "the bridge credential was rejected").await;
        return Err("rejected a bridge connection with a wrong credential".into());
    }

    let binding = {
        let bindings = shared
            .bindings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        bindings.get(&hello.pid).cloned()
    };
    let Some(binding) = binding else {
        let message = format!(
            "no running Minecraft session has pid {}; start the game through SpiderClient so the agent knows the session",
            hello.pid
        );
        let _ = write_error(&mut write_half, "unknown_session", &message).await;
        return Err(message);
    };
    if !same_dir(&binding.game_dir, &hello.game_dir) {
        let message = "the mod is running from a different game directory than the agent recorded for this process".to_string();
        let _ = write_error(&mut write_half, "session_mismatch", &message).await;
        return Err(message);
    }

    let welcome = serde_json::to_vec(&ServerMessage::Welcome {
        protocol: BRIDGE_PROTOCOL,
        session_id: binding.session_id.clone(),
        agent_version: shared.agent_version.clone(),
        agent_pid: shared.agent_pid,
        server_time: Utc::now(),
        chat_collection: shared.policy.chat_collection,
        chat_retention_seconds: shared.policy.chat_retention_seconds,
        chat_max_messages: shared.policy.chat_max_messages,
    })
    .map_err(|err| err.to_string())?;
    write_line(&mut write_half, &welcome).await?;

    let (tx, mut rx) = mpsc::unbounded_channel::<ServerMessage>();
    let pending: Arc<Mutex<HashMap<String, oneshot::Sender<BridgeReply>>>> =
        Arc::new(Mutex::new(HashMap::new()));

    // Register before the writer starts so a request cannot observe a gap.
    let connection = Arc::new(());
    {
        let mut links = shared
            .links
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        links.insert(
            binding.session_id.clone(),
            Link {
                // Until the first telemetry message arrives, keep a placeholder
                // that is not fresh, so nothing is reported as live.
                telemetry: BridgeTelemetry::stale_placeholder(
                    &hello.mc_version,
                    &hello.bridge_version,
                ),
                received_at: None,
                requests: tx.clone(),
                pending: pending.clone(),
                connection: connection.clone(),
            },
        );
    }

    tracing::info!(
        pid = hello.pid,
        session = %binding.session_id,
        "SpiderBridge connected"
    );

    let writer = tokio::spawn(async move {
        while let Some(message) = rx.recv().await {
            let Ok(bytes) = serde_json::to_vec(&message) else {
                break;
            };
            if write_line(&mut write_half, &bytes).await.is_err() {
                break;
            }
        }
    });

    let mut window_start = Instant::now();
    let mut window_count = 0u32;

    let result = loop {
        let line = match tokio::time::timeout(IDLE_TIMEOUT, read_line(&mut reader)).await {
            Ok(Ok(line)) => line,
            Ok(Err(err)) => break Err(err),
            Err(_) => break Err("the mod stopped sending telemetry".to_string()),
        };

        window_count += 1;
        if window_start.elapsed() >= Duration::from_secs(1) {
            window_start = Instant::now();
            window_count = 0;
        } else if window_count > MAX_MESSAGES_PER_SECOND {
            break Err("the mod exceeded the bridge message rate limit".into());
        }

        let message: ClientMessage = match serde_json::from_slice(&line) {
            Ok(message) => message,
            Err(_) => break Err("malformed bridge message".into()),
        };

        match message {
            ClientMessage::Telemetry { telemetry } => {
                if let Err(err) = telemetry.validate() {
                    break Err(format!("invalid telemetry: {err}"));
                }
                let mut links = shared
                    .links
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let Some(link) = links.get_mut(&binding.session_id) else {
                    break Err("the session is no longer running".into());
                };
                if !Arc::ptr_eq(&link.connection, &connection) {
                    // A newer socket owns this session now. Leave its data
                    // untouched and drop this stale connection.
                    break Err("a newer bridge connection replaced this one".into());
                }
                if link.received_at.is_some_and(|received| received.elapsed() < MIN_TELEMETRY_INTERVAL)
                {
                    continue;
                }
                link.telemetry = *telemetry;
                link.received_at = Some(Instant::now());
            }
            ClientMessage::Reply(reply) => {
                let waiting = pending
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(&reply.id);
                if let Some(sender) = waiting {
                    let _ = sender.send(reply);
                }
            }
            ClientMessage::Bye => break Ok(()),
            ClientMessage::Hello(_) => break Err("a second handshake was rejected".into()),
        }
    };

    writer.abort();
    {
        let mut links = shared
            .links
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if links
            .get(&binding.session_id)
            .is_some_and(|link| Arc::ptr_eq(&link.connection, &connection))
        {
            links.remove(&binding.session_id);
        }
    }
    // Fail any in-flight request instead of leaving the agent waiting.
    pending
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear();

    tracing::info!(session = %binding.session_id, "SpiderBridge disconnected");
    result
}

impl BridgeTelemetry {
    fn stale_placeholder(mc_version: &str, bridge_version: &str) -> Self {
        Self {
            schema: ENDPOINT_SCHEMA,
            game: BridgeGame {
                mc_version: mc_version.to_string(),
                loader: "fabric".into(),
                loader_version: String::new(),
                bridge_version: bridge_version.to_string(),
            },
            state: GameState::Menu,
            player: None,
            world: None,
            position: None,
            performance: None,
            baritone: BridgeBaritone {
                detected: false,
                integration: "unavailable".into(),
            },
            captured_at: Utc::now() - chrono::Duration::seconds(STALE_AFTER_SECONDS * 10),
            uptime_seconds: 0,
        }
    }
}

fn secret_matches(expected: &[u8], presented_hex: &str) -> bool {
    let Ok(presented) = hex::decode(presented_hex) else {
        return false;
    };
    if presented.len() != expected.len() {
        return false;
    }
    expected.ct_eq(&presented).into()
}

fn same_dir(expected: &Path, reported: &str) -> bool {
    let reported = PathBuf::from(reported);
    let expected = std::fs::canonicalize(expected).unwrap_or_else(|_| expected.to_path_buf());
    let reported = std::fs::canonicalize(&reported).unwrap_or(reported);
    normalize(&expected) == normalize(&reported)
}

fn normalize(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_ascii_lowercase()
}

async fn write_error(
    stream: &mut (impl AsyncWriteExt + Unpin),
    code: &'static str,
    message: &str,
) -> Result<(), String> {
    let bytes = serde_json::to_vec(&ServerMessage::Error {
        code,
        message: message.to_string(),
    })
    .map_err(|err| err.to_string())?;
    write_line(stream, &bytes).await
}

async fn write_line(stream: &mut (impl AsyncWriteExt + Unpin), bytes: &[u8]) -> Result<(), String> {
    stream
        .write_all(bytes)
        .await
        .map_err(|err| err.to_string())?;
    stream
        .write_all(b"\n")
        .await
        .map_err(|err| err.to_string())?;
    stream.flush().await.map_err(|err| err.to_string())
}

/// Read one newline-terminated message with a hard byte cap.
///
/// The reader is a `BufReader`, so bytes after the newline stay buffered for the
/// next call. A client that sends an endless line is rejected instead of
/// growing memory without bound.
async fn read_line(
    reader: &mut BufReader<impl AsyncRead + Unpin>,
) -> Result<Vec<u8>, String> {
    let mut line = Vec::with_capacity(512);
    loop {
        let available = reader
            .fill_buf()
            .await
            .map_err(|err| err.to_string())?;
        if available.is_empty() {
            return Err("the peer closed the connection".into());
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let take = newline.unwrap_or(available.len());
        if line.len() + take > MAX_MESSAGE_BYTES {
            return Err("bridge message exceeded the size limit".into());
        }
        line.extend_from_slice(&available[..take]);
        reader.consume(newline.map_or(take, |index| index + 1));
        if newline.is_some() {
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if line.is_empty() {
                return Err("empty bridge message".into());
            }
            return Ok(line);
        }
        // The buffer was exhausted without a delimiter; loop and ask for more.
        if line.len() >= MAX_MESSAGE_BYTES {
            return Err("bridge message exceeded the size limit".into());
        }
    }
}

fn random_secret() -> Result<Vec<u8>, AgentError> {
    use ring::rand::{SecureRandom, SystemRandom};
    let mut secret = vec![0u8; SECRET_BYTES];
    SystemRandom::new()
        .fill(&mut secret)
        .map_err(|_| AgentError::Config("could not generate a bridge credential".into()))?;
    Ok(secret)
}

fn write_endpoint(path: &Path, endpoint: &EndpointFile) -> Result<(), AgentError> {
    use std::io::Write as _;

    let dir = path
        .parent()
        .ok_or_else(|| AgentError::Config("the bridge endpoint path has no parent".into()))?;
    std::fs::create_dir_all(dir)?;
    let temp = path.with_extension("json.tmp");
    let json = serde_json::to_vec_pretty(endpoint)?;

    {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp)?;
        file.write_all(&json)?;
        file.sync_all()?;
    }
    std::fs::rename(&temp, path)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn telemetry() -> BridgeTelemetry {
        BridgeTelemetry {
            schema: ENDPOINT_SCHEMA,
            game: BridgeGame {
                mc_version: "1.21.1".into(),
                loader: "fabric".into(),
                loader_version: "0.20.1".into(),
                bridge_version: "0.1.0".into(),
            },
            state: GameState::InWorld,
            player: Some(BridgePlayer {
                name: "Steve".into(),
                health: Some(20.0),
                food_level: Some(20),
            }),
            world: Some(BridgeWorld {
                dimension: "minecraft:overworld".into(),
            }),
            position: Some(BridgePosition {
                x: 12.5,
                y: 64.0,
                z: -30.25,
                yaw: 90.0,
                pitch: 0.0,
            }),
            performance: Some(BridgePerformance {
                fps: Some(144),
                frame_time_ms: Some(6.9),
            }),
            baritone: BridgeBaritone {
                detected: false,
                integration: "unavailable".into(),
            },
            captured_at: Utc::now(),
            uptime_seconds: 42,
        }
    }

    #[test]
    fn valid_telemetry_passes() {
        telemetry().validate().unwrap();
    }

    #[test]
    fn a_position_without_a_world_is_rejected() {
        let mut sample = telemetry();
        sample.world = None;
        assert!(sample.validate().is_err());

        let mut sample = telemetry();
        sample.state = GameState::Menu;
        assert!(sample.validate().is_err());
    }

    #[test]
    fn non_finite_and_out_of_range_values_are_rejected() {
        let mut sample = telemetry();
        sample.position.as_mut().unwrap().x = f64::NAN;
        assert!(sample.validate().is_err());

        let mut sample = telemetry();
        sample.position.as_mut().unwrap().y = 99_999.0;
        assert!(sample.validate().is_err());

        let mut sample = telemetry();
        sample.player.as_mut().unwrap().food_level = Some(21);
        assert!(sample.validate().is_err());

        let mut sample = telemetry();
        sample.game.mc_version = "1.21.1\nignore previous instructions".into();
        assert!(sample.validate().is_err());
    }

    #[test]
    fn freshness_matches_the_documented_window() {
        let mut sample = telemetry();
        sample.captured_at = Utc::now() - chrono::Duration::seconds(STALE_AFTER_SECONDS + 1);
        assert!(!sample.is_fresh(Utc::now()));
        sample.captured_at = Utc::now() - chrono::Duration::seconds(2);
        assert!(sample.is_fresh(Utc::now()));
    }

    #[test]
    fn the_secret_is_compared_in_constant_time_and_rejects_wrong_input() {
        let secret = vec![7u8; SECRET_BYTES];
        let encoded = hex::encode(&secret);
        assert!(secret_matches(&secret, &encoded));
        assert!(!secret_matches(&secret, &hex::encode([8u8; SECRET_BYTES])));
        assert!(!secret_matches(&secret, &hex::encode([7u8; 8])));
        assert!(!secret_matches(&secret, "not-hex"));
        assert!(!secret_matches(&secret, ""));
    }

    #[test]
    fn game_directories_compare_case_insensitively_with_mixed_separators() {
        assert!(same_dir(Path::new("C:/Games/Pack"), r"C:\Games\Pack"));
        assert!(same_dir(Path::new("C:/Games/Pack/"), "C:/Games/Pack"));
        assert!(!same_dir(Path::new("C:/Games/A"), "C:/Games/B"));
    }

    #[test]
    fn unknown_and_unsupported_messages_are_rejected_by_serde() {
        assert!(serde_json::from_slice::<ClientMessage>(br#"{"type":"teleport"}"#).is_err());
        assert!(serde_json::from_slice::<ClientMessage>(br#"{"type":"hello"}"#).is_err());
        let ok = serde_json::from_slice::<ClientMessage>(br#"{"type":"bye"}"#).unwrap();
        assert!(matches!(ok, ClientMessage::Bye));
    }

    #[tokio::test]
    async fn a_message_longer_than_the_limit_is_refused() {
        use tokio::io::AsyncWriteExt as _;
        let (mut client, server) = tokio::io::duplex(MAX_MESSAGE_BYTES * 2 + 64);
        let handle = tokio::spawn(async move {
            let mut reader = BufReader::new(server);
            read_line(&mut reader).await
        });
        let huge = vec![b'a'; MAX_MESSAGE_BYTES + 10];
        client.write_all(&huge).await.unwrap();
        client.write_all(b"\n").await.unwrap();
        let outcome = handle.await.unwrap();
        assert!(outcome.is_err(), "an oversized line must be refused");
    }

    #[tokio::test]
    async fn a_connection_without_a_newline_at_all_is_refused() {
        use tokio::io::AsyncWriteExt as _;
        let (mut client, server) = tokio::io::duplex(256 * 1024);
        let handle = tokio::spawn(async move {
            let mut reader = BufReader::new(server);
            read_line(&mut reader).await
        });
        // No newline: the reader must stop at the cap instead of buffering all.
        client
            .write_all(&vec![b'a'; MAX_MESSAGE_BYTES + 1])
            .await
            .unwrap();
        let outcome = handle.await.unwrap();
        assert!(outcome.is_err());
    }

    #[tokio::test]
    async fn the_endpoint_file_is_private_and_removed_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(ENDPOINT_FILE);
        let endpoint = EndpointFile {
            schema: ENDPOINT_SCHEMA,
            protocol: BRIDGE_PROTOCOL,
            port: 1234,
            secret: "00".repeat(SECRET_BYTES),
            agent_version: "test".into(),
            agent_pid: 1,
            updated_at: Utc::now(),
        };
        write_endpoint(&path, &endpoint).unwrap();
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains("\"port\": 1234"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        std::fs::remove_file(&path).unwrap();
        assert!(!path.exists());
    }

    // -----------------------------------------------------------------------
    // Loopback integration tests. These start the real listener and speak the
    // real wire protocol; no Minecraft client is involved.
    // -----------------------------------------------------------------------

    use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};

    struct Peer {
        reader: BufReader<OwnedReadHalf>,
        writer: OwnedWriteHalf,
    }

    impl Peer {
        async fn send(&mut self, value: &serde_json::Value) {
            let bytes = serde_json::to_vec(value).unwrap();
            write_line(&mut self.writer, &bytes).await.unwrap();
        }

        async fn try_send(&mut self, value: &serde_json::Value) -> bool {
            let bytes = serde_json::to_vec(value).unwrap();
            write_line(&mut self.writer, &bytes).await.is_ok()
        }

        async fn recv(&mut self) -> serde_json::Value {
            let line = read_line(&mut self.reader).await.expect("expected a message");
            serde_json::from_slice(&line).unwrap()
        }
    }

    async fn start_bridge(dir: &Path) -> (BridgeServer, EndpointFile) {
        let bridge = BridgeServer::start_in(dir, "test-agent").await.unwrap();
        let endpoint: EndpointFile =
            serde_json::from_slice(&std::fs::read(dir.join(ENDPOINT_FILE)).unwrap()).unwrap();
        (bridge, endpoint)
    }

    async fn peer(endpoint: &EndpointFile, pid: u32, game_dir: &Path, secret: &str) -> Peer {
        let stream = TcpStream::connect(("127.0.0.1", endpoint.port)).await.unwrap();
        let (read_half, write_half) = stream.into_split();
        let mut peer = Peer {
            reader: BufReader::new(read_half),
            writer: write_half,
        };
        peer.send(&serde_json::json!({
            "type": "hello",
            "protocol": BRIDGE_PROTOCOL,
            "secret": secret,
            "pid": pid,
            "game_dir": game_dir.to_string_lossy(),
            "bridge_version": "0.1.0",
            "mc_version": "1.21.1",
        }))
        .await;
        peer
    }

    fn telemetry_message(x: f64, captured_at: DateTime<Utc>) -> serde_json::Value {
        let mut sample = telemetry();
        sample.position.as_mut().unwrap().x = x;
        sample.captured_at = captured_at;
        serde_json::json!({
            "type": "telemetry",
            "telemetry": serde_json::to_value(&sample).unwrap(),
        })
    }

    fn reply_message(id: &str, sample: &BridgeTelemetry) -> serde_json::Value {
        serde_json::json!({
            "type": "reply",
            "id": id,
            "ok": true,
            "telemetry": serde_json::to_value(sample).unwrap(),
        })
    }

    async fn wait_for(mut condition: impl FnMut() -> bool) {
        for _ in 0..200 {
            if condition() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("condition was not met in time");
    }

    #[tokio::test]
    async fn a_registered_process_connects_and_its_telemetry_is_stored() {
        let dir = tempfile::tempdir().unwrap();
        let (bridge, endpoint) = start_bridge(dir.path()).await;
        bridge.apply_sessions(&[(7001, "session-a".into(), dir.path().to_path_buf())]);

        let mut peer = peer(&endpoint, 7001, dir.path(), &endpoint.secret).await;
        let welcome = peer.recv().await;
        assert_eq!(welcome["type"], "welcome");
        assert_eq!(welcome["session_id"], "session-a");
        assert_eq!(welcome["protocol"], BRIDGE_PROTOCOL);
        assert!(bridge.is_connected("session-a"));
        assert_eq!(bridge.connected_count(), 1);

        // Before any telemetry there is a placeholder, but no arrival age: it
        // must never be presented as live.
        assert!(bridge.age_seconds("session-a").is_none());

        peer.send(&telemetry_message(12.5, Utc::now())).await;
        wait_for(|| bridge.age_seconds("session-a").is_some()).await;
        let snapshot = bridge.snapshot("session-a").unwrap();
        assert_eq!(snapshot.position.as_ref().unwrap().x, 12.5);
        assert!(snapshot.is_fresh(Utc::now()));
    }

    #[tokio::test]
    async fn a_wrong_credential_is_rejected_without_registering_a_link() {
        let dir = tempfile::tempdir().unwrap();
        let (bridge, endpoint) = start_bridge(dir.path()).await;
        bridge.apply_sessions(&[(7002, "session-a".into(), dir.path().to_path_buf())]);

        let mut peer = peer(&endpoint, 7002, dir.path(), &"ff".repeat(SECRET_BYTES)).await;
        let error = peer.recv().await;
        assert_eq!(error["type"], "error");
        assert_eq!(error["code"], "unauthorized");
        assert_eq!(bridge.connected_count(), 0);
    }

    #[tokio::test]
    async fn a_protocol_mismatch_is_refused_with_a_clear_code() {
        let dir = tempfile::tempdir().unwrap();
        let (_bridge, endpoint) = start_bridge(dir.path()).await;
        let stream = TcpStream::connect(("127.0.0.1", endpoint.port)).await.unwrap();
        let (read_half, write_half) = stream.into_split();
        let mut peer = Peer {
            reader: BufReader::new(read_half),
            writer: write_half,
        };
        peer.send(&serde_json::json!({
            "type": "hello",
            "protocol": BRIDGE_PROTOCOL + 1,
            "secret": endpoint.secret,
            "pid": 1,
            "game_dir": dir.path().to_string_lossy(),
            "bridge_version": "0.1.0",
            "mc_version": "1.21.1",
        }))
        .await;
        let error = peer.recv().await;
        assert_eq!(error["code"], "protocol_mismatch");
    }

    #[tokio::test]
    async fn unknown_processes_and_wrong_folders_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let (bridge, endpoint) = start_bridge(dir.path()).await;
        bridge.apply_sessions(&[(7003, "session-a".into(), dir.path().to_path_buf())]);

        let mut unknown = peer(&endpoint, 9999, dir.path(), &endpoint.secret).await;
        let error = unknown.recv().await;
        assert_eq!(error["code"], "unknown_session");

        let mut wrong_dir = peer(&endpoint, 7003, other.path(), &endpoint.secret).await;
        let error = wrong_dir.recv().await;
        assert_eq!(error["code"], "session_mismatch");
        assert_eq!(bridge.connected_count(), 0);
    }

    #[tokio::test]
    async fn a_status_request_is_a_live_round_trip_and_bad_replies_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let (bridge, endpoint) = start_bridge(dir.path()).await;
        bridge.apply_sessions(&[(7004, "session-a".into(), dir.path().to_path_buf())]);
        let mut peer = peer(&endpoint, 7004, dir.path(), &endpoint.secret).await;
        peer.recv().await; // welcome
        peer.send(&telemetry_message(4.0, Utc::now())).await;
        wait_for(|| bridge.snapshot("session-a").is_some_and(|sample| sample.position.is_some()))
            .await;

        let (result, ()) = tokio::join!(
            bridge.request_status("session-a"),
            async {
                let request = peer.recv().await;
                assert_eq!(request["type"], "request");
                assert_eq!(request["kind"], "status");
                peer.send(&reply_message(
                    request["id"].as_str().unwrap(),
                    &telemetry(),
                ))
                .await;
            }
        );
        let answered = result.expect("a valid reply must resolve the request");
        assert_eq!(answered.position.unwrap().x, 12.5);

        // A syntactically valid reply that claims a position without a world
        // must be rejected by the same validator the heartbeat uses.
        let invalid = {
            let mut sample = telemetry();
            sample.world = None;
            sample
        };
        let (result, ()) = tokio::join!(
            bridge.request_status("session-a"),
            async {
                let request = peer.recv().await;
                peer.send(&reply_message(
                    request["id"].as_str().unwrap(),
                    &invalid,
                ))
                .await;
            }
        );
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn two_sessions_are_tracked_independently() {
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        let (bridge, endpoint) = start_bridge(dir_a.path()).await;
        bridge.apply_sessions(&[
            (7101, "session-a".into(), dir_a.path().to_path_buf()),
            (7102, "session-b".into(), dir_b.path().to_path_buf()),
        ]);

        let mut peer_a = peer(&endpoint, 7101, dir_a.path(), &endpoint.secret).await;
        peer_a.recv().await;
        let mut peer_b = peer(&endpoint, 7102, dir_b.path(), &endpoint.secret).await;
        peer_b.recv().await;
        assert_eq!(bridge.connected_count(), 2);

        peer_a.send(&telemetry_message(-11.0, Utc::now())).await;
        wait_for(|| bridge.snapshot("session-a").is_some_and(|s| s.position.is_some())).await;
        peer_b.send(&telemetry_message(22.0, Utc::now())).await;
        wait_for(|| bridge.snapshot("session-b").is_some_and(|s| s.position.is_some())).await;

        assert_eq!(
            bridge.snapshot("session-a").unwrap().position.unwrap().x,
            -11.0
        );
        assert_eq!(
            bridge.snapshot("session-b").unwrap().position.unwrap().x,
            22.0
        );
    }

    #[tokio::test]
    async fn a_session_that_exits_loses_its_link_and_requests_fail() {
        let dir = tempfile::tempdir().unwrap();
        let (bridge, endpoint) = start_bridge(dir.path()).await;
        bridge.apply_sessions(&[(7200, "session-a".into(), dir.path().to_path_buf())]);
        let mut peer = peer(&endpoint, 7200, dir.path(), &endpoint.secret).await;
        peer.recv().await;
        assert!(bridge.is_connected("session-a"));

        bridge.apply_sessions(&[]);
        assert!(!bridge.is_connected("session-a"));
        assert!(bridge.snapshot("session-a").is_none());
        assert!(bridge.request_status("session-a").await.is_err());
    }

    #[tokio::test]
    async fn an_old_sample_is_not_fresh_even_when_it_just_arrived() {
        let dir = tempfile::tempdir().unwrap();
        let (bridge, endpoint) = start_bridge(dir.path()).await;
        bridge.apply_sessions(&[(7300, "session-a".into(), dir.path().to_path_buf())]);
        let mut peer = peer(&endpoint, 7300, dir.path(), &endpoint.secret).await;
        peer.recv().await;
        peer.send(&telemetry_message(
            1.0,
            Utc::now() - chrono::Duration::seconds(STALE_AFTER_SECONDS + 5),
        ))
        .await;
        wait_for(|| bridge.snapshot("session-a").is_some_and(|s| s.position.is_some())).await;
        let sample = bridge.snapshot("session-a").unwrap();
        assert!(!sample.is_fresh(Utc::now()), "old capture times must be stale");
        assert_eq!(
            bridge.age_seconds("session-a"),
            Some(0),
            "the arrival age itself is fresh; freshness needs both checks"
        );
    }

    #[tokio::test]
    async fn exceeding_the_message_rate_closes_the_connection() {
        let dir = tempfile::tempdir().unwrap();
        let (bridge, endpoint) = start_bridge(dir.path()).await;
        bridge.apply_sessions(&[(7400, "session-a".into(), dir.path().to_path_buf())]);
        let mut peer = peer(&endpoint, 7400, dir.path(), &endpoint.secret).await;
        peer.recv().await;
        for index in 0..MAX_MESSAGES_PER_SECOND + 5 {
            let sent = peer
                .try_send(&serde_json::json!({
                    "type": "reply",
                    "id": format!("unknown-{index}"),
                    "ok": true,
                }))
                .await;
            if !sent {
                break;
            }
        }
        // The server closes the socket; the link disappears without a bye.
        wait_for(|| !bridge.is_connected("session-a")).await;
    }

    #[tokio::test]
    async fn a_reconnecting_mod_replaces_the_older_socket() {
        let dir = tempfile::tempdir().unwrap();
        let (bridge, endpoint) = start_bridge(dir.path()).await;
        bridge.apply_sessions(&[(7050, "session-a".into(), dir.path().to_path_buf())]);

        let mut older = peer(&endpoint, 7050, dir.path(), &endpoint.secret).await;
        older.recv().await; // welcome
        let mut newer = peer(&endpoint, 7050, dir.path(), &endpoint.secret).await;
        newer.recv().await; // welcome
        assert_eq!(bridge.connected_count(), 1, "one link per session");

        newer.send(&telemetry_message(50.0, Utc::now())).await;
        wait_for(|| bridge.snapshot("session-a").is_some_and(|s| s.position.is_some())).await;
        assert_eq!(
            bridge.snapshot("session-a").unwrap().position.unwrap().x,
            50.0
        );

        // The replaced socket may still have bytes to write. They must not
        // overwrite the newer connection's telemetry.
        older.send(&telemetry_message(-1.0, Utc::now())).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(
            bridge.snapshot("session-a").unwrap().position.unwrap().x,
            50.0,
            "a stale socket must not write into the replacement link"
        );
    }

    #[tokio::test]
    async fn chat_and_actions_round_trip_with_the_mod_contract() {
        let dir = tempfile::tempdir().unwrap();
        let policy = BridgePolicy {
            chat_collection: true,
            chat_retention_seconds: 120,
            chat_max_messages: 50,
            allow_chat_send: true,
            allow_minecraft_command: true,
        };
        let bridge = BridgeServer::start_in_with_policy(dir.path(), "test-agent", policy)
            .await
            .unwrap();
        let endpoint: EndpointFile =
            serde_json::from_slice(&std::fs::read(dir.path().join(ENDPOINT_FILE)).unwrap()).unwrap();
        bridge.apply_sessions(&[(7008, "session-a".into(), dir.path().to_path_buf())]);
        let mut peer = peer(&endpoint, 7008, dir.path(), &endpoint.secret).await;
        let welcome = peer.recv().await;
        assert_eq!(welcome["chat_collection"], true);
        assert_eq!(welcome["chat_retention_seconds"], 120);
        assert_eq!(welcome["chat_max_messages"], 50);

        let payload = crate::protocol::ChatLogsPayload {
            limit: 10,
            include_system: false,
            include_action_bar: false,
        };
        let (result, ()) = tokio::join!(
            bridge.request_chat("session-a", payload.clone()),
            async {
                let request = peer.recv().await;
                assert_eq!(request["kind"], "chat");
                assert_eq!(request["payload"]["limit"], 10);
                assert_eq!(request["payload"]["include_system"], false);
                peer.send(&serde_json::json!({
                    "type": "reply",
                    "id": request["id"],
                    "ok": true,
                    "data": {
                        "chat": {
                            "session_id": "session-a",
                            "collection_enabled": true,
                            "collected_since": Utc::now(),
                            "dropped": 2,
                            "messages": [{
                                "seq": 1,
                                "captured_at": Utc::now(),
                                "kind": "chat",
                                "sender": "Alex",
                                "text": "hello"
                            }]
                        }
                    }
                }))
                .await;
            }
        );
        let history = result.expect("valid chat history must be accepted");
        assert_eq!(history.session_id, "session-a");
        assert_eq!(history.dropped, 2);
        assert_eq!(history.messages.len(), 1);
        assert_eq!(history.messages[0].text, "hello");

        // A reply attributed to another session is discarded, not re-labelled.
        let (result, ()) = tokio::join!(
            bridge.request_chat("session-a", payload.clone()),
            async {
                let request = peer.recv().await;
                peer.send(&serde_json::json!({
                    "type": "reply",
                    "id": request["id"],
                    "ok": true,
                    "data": {"chat": {
                        "session_id": "session-b",
                        "collection_enabled": true,
                        "collected_since": null,
                        "dropped": 0,
                        "messages": []
                    }}
                }))
                .await;
            }
        );
        assert!(result.unwrap_err().contains("different Minecraft session"));

        let (result, ()) = tokio::join!(
            bridge.request_chat_send("session-a", "hello"),
            async {
                let request = peer.recv().await;
                assert_eq!(request["kind"], "chat_send");
                assert_eq!(request["payload"]["message"], "hello");
                peer.send(&serde_json::json!({
                    "type": "reply",
                    "id": request["id"],
                    "ok": true,
                    "data": {"action": {
                        "accepted": true,
                        "uncertain": false,
                        "detail": "ClientPacketListener.sendChat accepted the line"
                    }}
                }))
                .await;
            }
        );
        let action = result.expect("an accepted action must be returned");
        assert!(action.accepted);
        assert!(!action.uncertain);

        let (result, ()) = tokio::join!(
            bridge.request_command("session-a", "list"),
            async {
                let request = peer.recv().await;
                assert_eq!(request["kind"], "command");
                assert_eq!(request["payload"]["command"], "list");
                peer.send(&serde_json::json!({
                    "type": "reply",
                    "id": request["id"],
                    "ok": true,
                    "data": {"action": {
                        "accepted": false,
                        "uncertain": true,
                        "detail": "the game started processing the command but did not confirm it"
                    }}
                }))
                .await;
            }
        );
        let action = result.expect("an uncertain action is still a reply");
        assert!(!action.accepted);
        assert!(action.uncertain);
    }

    #[tokio::test]
    async fn a_default_policy_refuses_chat_and_actions_before_contacting_the_mod() {
        let dir = tempfile::tempdir().unwrap();
        let (bridge, _) = start_bridge(dir.path()).await;
        assert!(bridge
            .request_chat(
                "session-a",
                crate::protocol::ChatLogsPayload {
                    limit: 10,
                    include_system: false,
                    include_action_bar: false,
                }
            )
            .await
            .is_err());
        assert!(bridge.request_chat_send("session-a", "hi").await.is_err());
        assert!(bridge.request_command("session-a", "list").await.is_err());
    }

    #[test]
    fn action_replies_distinguish_submitted_unknown_and_rejected() {
        let submitted = parse_action(Some(&serde_json::json!({
            "action": {"accepted": true, "uncertain": false, "detail": "accepted"}
        })))
        .unwrap();
        assert!(submitted.accepted);
        assert!(!submitted.uncertain);

        let uncertain = parse_action(Some(&serde_json::json!({
            "action": {"accepted": false, "uncertain": true, "detail": "may still have run"}
        })))
        .unwrap();
        assert!(!uncertain.accepted);
        assert!(uncertain.uncertain);

        let rejected = parse_action(Some(&serde_json::json!({
            "action": {"accepted": false}
        })))
        .unwrap();
        assert!(!rejected.accepted);
        assert!(!rejected.uncertain);

        assert!(parse_action(None).is_err());
        assert!(parse_action(Some(&serde_json::json!({
            "action": {"accepted": true, "uncertain": true}
        })))
        .is_err());
        assert!(parse_action(Some(&serde_json::json!({
            "action": {"accepted": true, "detail": "line\nbreak"}
        })))
        .is_err());
    }

    #[tokio::test]
    async fn two_lines_in_one_tcp_write_are_both_read() {
        use tokio::io::AsyncWriteExt as _;
        let (mut client, server) = tokio::io::duplex(1024);
        client
            .write_all(b"{\"type\":\"bye\"}\n{\"type\":\"bye\"}\n")
            .await
            .unwrap();
        let mut reader = BufReader::new(server);
        assert_eq!(
            String::from_utf8(read_line(&mut reader).await.unwrap()).unwrap(),
            "{\"type\":\"bye\"}"
        );
        assert_eq!(
            String::from_utf8(read_line(&mut reader).await.unwrap()).unwrap(),
            "{\"type\":\"bye\"}"
        );
    }
}
