//! Trusted backend: Discord interactions, pairing, device registry, and the
//! agent control plane.
//!
//! The bot token is read once from the server environment. It is never sent to
//! agents, never serialized into an HTTP response, and never logged.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use actix_web::http::header;
use actix_web::{App, HttpRequest, HttpResponse, HttpServer, web};
use chrono::Utc;
use serde_json::{Value, json};

use crate::discord;
use crate::protocol::{
    AgentHeartbeat, ApiError, CommandResult, DeviceSnapshot, HeartbeatResponse,
    MAX_SCREENSHOT_BYTES, PROTOCOL_VERSION, PairClaim, PairResponse, ScreenshotAttachment,
    truncate_text, validate_device_name,
};
use crate::redact::redact;
use crate::security::{bearer_token, verify_discord_signature};
use crate::store::{DeviceRecord, Store, StoreError};

const HEARTBEAT_STALE_SECONDS: u64 = 45;
const PAIR_WINDOW: Duration = Duration::from_secs(10 * 60);
const PAIR_ATTEMPTS_PER_WINDOW: usize = 12;

pub struct BackendConfig {
    pub bind: String,
    pub database_url: String,
    pub bot_token: String,
    pub public_key: String,
    pub application_id: String,
    pub admin_discord_ids: Vec<String>,
    pub allowed_guild_ids: Vec<String>,
    pub alert_channel_id: Option<String>,
    pub website_api_token: Option<String>,
    pub allow_launch: bool,
    pub allow_stop: bool,
    pub allow_screenshot: bool,
    /// Backend-side gate for live coordinates from the SpiderBridge mod. The
    /// agent has its own `allow_bridge` switch; both must be on.
    pub allow_coordinates: bool,
    /// Read-only chat history from the mod.
    pub allow_chat: bool,
    /// Chat submission through the game client.
    pub allow_chat_send: bool,
    /// Minecraft command submission through the game client.
    pub allow_minecraft_command: bool,
    /// Which command categories this backend will even ask an agent to run.
    pub command_policy: crate::command_policy::CommandPolicy,
    /// Require `confirm:true` for every state-changing command category.
    pub require_command_confirmation: bool,
    pub audit_retention_days: i64,
    pub pair_ttl_seconds: i64,
}

impl BackendConfig {
    pub fn from_env() -> Result<Self, String> {
        let bot_token = std::env::var("DISCORD_BOT_TOKEN")
            .map_err(|_| "DISCORD_BOT_TOKEN is not set on the backend host".to_string())?;
        if bot_token.trim().len() < 50 {
            return Err("DISCORD_BOT_TOKEN does not look like a bot token".into());
        }
        let public_key = std::env::var("DISCORD_PUBLIC_KEY")
            .map_err(|_| "DISCORD_PUBLIC_KEY is not set on the backend host".to_string())?;
        if hex::decode(public_key.trim()).map(|key| key.len()).ok() != Some(32) {
            return Err("DISCORD_PUBLIC_KEY must be 32 bytes of hexadecimal".into());
        }
        let application_id = std::env::var("DISCORD_APPLICATION_ID")
            .map_err(|_| "DISCORD_APPLICATION_ID is not set on the backend host".to_string())?;
        if application_id.trim().is_empty()
            || !application_id.trim().chars().all(|c| c.is_ascii_digit())
        {
            return Err("DISCORD_APPLICATION_ID must be numeric".into());
        }
        let admin_discord_ids = split_ids(
            std::env::var("SPIDER_OWNER_DISCORD_IDS")
                .unwrap_or_default()
                .as_str(),
        );
        let allowed_guild_ids = split_ids(
            std::env::var("SPIDER_ALLOWED_GUILD_IDS")
                .unwrap_or_default()
                .as_str(),
        );
        Ok(Self {
            bind: std::env::var("SPIDER_BIND").unwrap_or_else(|_| "127.0.0.1:8787".into()),
            database_url: std::env::var("SPIDER_DATABASE_URL")
                .unwrap_or_else(|_| "sqlite://spider_remote.db".into()),
            bot_token,
            public_key: public_key.trim().to_ascii_lowercase(),
            application_id,
            admin_discord_ids,
            allowed_guild_ids,
            alert_channel_id: std::env::var("SPIDER_ALERT_CHANNEL_ID")
                .ok()
                .filter(|id| !id.trim().is_empty()),
            website_api_token: std::env::var("SPIDER_WEBSITE_API_TOKEN")
                .ok()
                .filter(|token| !token.trim().is_empty()),
            allow_launch: env_bool("SPIDER_ALLOW_LAUNCH"),
            allow_stop: env_bool("SPIDER_ALLOW_STOP"),
            allow_screenshot: env_bool("SPIDER_ALLOW_SCREENSHOT"),
            allow_coordinates: env_bool("SPIDER_ALLOW_COORDINATES"),
            allow_chat: env_bool("SPIDER_ALLOW_CHAT"),
            allow_chat_send: env_bool("SPIDER_ALLOW_CHAT_SEND"),
            allow_minecraft_command: env_bool("SPIDER_ALLOW_MC_COMMAND"),
            command_policy: crate::command_policy::CommandPolicy {
                read_only: env_bool("SPIDER_MC_ALLOW_READ_ONLY"),
                disruptive: env_bool("SPIDER_MC_ALLOW_DISRUPTIVE"),
                administrative: env_bool("SPIDER_MC_ALLOW_ADMIN"),
            },
            require_command_confirmation: env_bool_default("SPIDER_REQUIRE_MC_CONFIRM", true),
            audit_retention_days: std::env::var("SPIDER_AUDIT_RETENTION_DAYS")
                .ok()
                .and_then(|value| value.parse::<i64>().ok())
                .unwrap_or(7)
                .clamp(1, 90),
            pair_ttl_seconds: 600,
        })
    }

    pub fn policy_capabilities(&self) -> crate::protocol::Capabilities {
        crate::protocol::Capabilities {
            launch: self.allow_launch,
            stop: self.allow_stop,
            screenshot: self.allow_screenshot,
            coordinates: self.allow_coordinates,
            chat: self.allow_chat,
            chat_send: self.allow_chat_send,
            minecraft_command: self.allow_minecraft_command,
            ..crate::protocol::Capabilities::default()
        }
    }

    pub fn is_admin(&self, user_id: &str) -> bool {
        self.admin_discord_ids.iter().any(|id| id == user_id)
    }

    pub fn guild_allowed(&self, guild_id: Option<&str>) -> bool {
        if self.allowed_guild_ids.is_empty() {
            return true;
        }
        guild_id.is_some_and(|id| self.allowed_guild_ids.iter().any(|allowed| allowed == id))
    }

    /// Deliberately no token or key material.
    pub fn public_summary(&self) -> Value {
        json!({
            "bind": self.bind,
            "application_id": self.application_id,
            "admin_count": self.admin_discord_ids.len(),
            "guild_restriction": !self.allowed_guild_ids.is_empty(),
            "alerts_configured": self.alert_channel_id.is_some(),
            "website_api_enabled": self.website_api_token.is_some(),
            "allow_launch": self.allow_launch,
            "allow_stop": self.allow_stop,
            "allow_screenshot": self.allow_screenshot,
            "allow_coordinates": self.allow_coordinates,
            "allow_chat": self.allow_chat,
            "allow_chat_send": self.allow_chat_send,
            "allow_minecraft_command": self.allow_minecraft_command,
            "allow_read_only_commands": self.command_policy.read_only,
            "allow_disruptive_commands": self.command_policy.disruptive,
            "allow_administrative_commands": self.command_policy.administrative,
            "require_command_confirmation": self.require_command_confirmation,
            "audit_retention_days": self.audit_retention_days,
        })
    }
}

fn split_ids(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|id| !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()))
        .map(str::to_string)
        .collect()
}

fn env_bool(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| value == "1" || value.eq_ignore_ascii_case("true"))
}

fn env_bool_default(name: &str, default: bool) -> bool {
    match std::env::var(name) {
        Ok(value) => value == "1" || value.eq_ignore_ascii_case("true"),
        Err(_) => default,
    }
}

pub struct BackendState {
    pub store: Store,
    pub config: BackendConfig,
    pub http: reqwest::Client,
    pair_attempts: Mutex<HashMap<String, VecDeque<Instant>>>,
    command_attempts: Mutex<HashMap<String, VecDeque<Instant>>>,
    screenshot_attempts: Mutex<HashMap<String, Instant>>,
    action_attempts: Mutex<HashMap<String, Instant>>,
    alert_cooldowns: Mutex<HashMap<String, Instant>>,
}

impl BackendState {
    pub fn new(store: Store, config: BackendConfig) -> Result<Self, String> {
        let http = reqwest::Client::builder()
            .user_agent("SpiderClient-Remote/0.1")
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|err| format!("could not build the backend HTTP client: {err}"))?;
        Ok(Self {
            store,
            config,
            http,
            pair_attempts: Mutex::new(HashMap::new()),
            command_attempts: Mutex::new(HashMap::new()),
            screenshot_attempts: Mutex::new(HashMap::new()),
            action_attempts: Mutex::new(HashMap::new()),
            alert_cooldowns: Mutex::new(HashMap::new()),
        })
    }

    fn allow_pair_attempt(&self, key: &str) -> bool {
        let mut attempts = self
            .pair_attempts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        let entry = attempts.entry(key.to_string()).or_default();
        while entry
            .front()
            .is_some_and(|time| now.duration_since(*time) > PAIR_WINDOW)
        {
            entry.pop_front();
        }
        if entry.len() >= PAIR_ATTEMPTS_PER_WINDOW {
            return false;
        }
        entry.push_back(now);
        true
    }

    pub fn allow_command(&self, user_id: &str) -> bool {
        let mut attempts = self
            .command_attempts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        let entry = attempts.entry(user_id.to_string()).or_default();
        while entry
            .front()
            .is_some_and(|time| now.duration_since(*time) > Duration::from_secs(60))
        {
            entry.pop_front();
        }
        if entry.len() >= 30 {
            return false;
        }
        entry.push_back(now);
        true
    }

    /// Screenshots are more expensive than a log tail and may contain private
    /// on-screen content, so each user/device pair has its own cooldown.
    pub fn allow_screenshot(&self, user_id: &str, device_id: &str) -> bool {
        let mut attempts = self
            .screenshot_attempts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        let key = format!("{user_id}:{device_id}");
        if attempts.get(&key).is_some_and(|last| {
            now.duration_since(*last)
                < Duration::from_secs(crate::protocol::SCREENSHOT_COOLDOWN_SECONDS)
        }) {
            return false;
        }
        attempts.insert(key, now);
        true
    }

    /// Per-user, per-device, per-action cooldown. Chat and command submission
    /// change external state, so they get their own bucket instead of sharing
    /// the general command rate limit.
    pub fn allow_action(
        &self,
        user_id: &str,
        device_id: &str,
        kind: &str,
        cooldown_seconds: u64,
    ) -> bool {
        let mut attempts = self
            .action_attempts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        let key = format!("{user_id}:{device_id}:{kind}");
        if attempts.get(&key).is_some_and(|last| {
            now.duration_since(*last) < Duration::from_secs(cooldown_seconds)
        }) {
            return false;
        }
        attempts.insert(key, now);
        true
    }

    fn alert_allowed(&self, key: &str) -> bool {
        let mut cooldowns = self
            .alert_cooldowns
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        if cooldowns
            .get(key)
            .is_some_and(|last| now.duration_since(*last) < Duration::from_secs(60))
        {
            return false;
        }
        cooldowns.insert(key.to_string(), now);
        true
    }

    pub async fn send_alert(&self, content: String) {
        let Some(channel) = self.config.alert_channel_id.as_deref() else {
            return;
        };
        let content = truncate_text(&redact(&content), 1800);
        let url = format!("https://discord.com/api/v10/channels/{channel}/messages");
        match self
            .http
            .post(url)
            .header("authorization", format!("Bot {}", self.config.bot_token))
            .json(&json!({"content": content, "allowed_mentions": {"parse": []}}))
            .send()
            .await
        {
            Ok(response) if response.status().is_success() => {}
            Ok(response) => {
                tracing::warn!(status = response.status().as_u16(), "alert delivery failed")
            }
            Err(err) => tracing::warn!(error = %err, "alert network request failed"),
        }
    }

    pub async fn follow_up(&self, interaction_token: &str, content: String) {
        self.follow_up_with_screenshot(interaction_token, content, None)
            .await;
    }

    /// Send the final interaction response, optionally with a freshly captured
    /// image. The image is decoded here, validated again, and never written to
    /// disk or to the command database.
    pub async fn follow_up_with_screenshot(
        &self,
        interaction_token: &str,
        content: String,
        screenshot: Option<&ScreenshotAttachment>,
    ) {
        let content = truncate_text(&redact(&content), 1900);
        let url = format!(
            "https://discord.com/api/v10/webhooks/{}/{}",
            self.config.application_id, interaction_token
        );
        let payload = json!({
            "content": content,
            "allowed_mentions": {"parse": []},
            "flags": 64
        });

        let request = match screenshot {
            Some(screenshot) => {
                let Ok(bytes) = base64::Engine::decode(
                    &base64::engine::general_purpose::STANDARD,
                    &screenshot.data_base64,
                ) else {
                    tracing::warn!("screenshot attachment was not valid base64; sending text only");
                    return;
                };
                if bytes.is_empty()
                    || bytes.len() > MAX_SCREENSHOT_BYTES
                    || !screenshot.is_acceptable()
                {
                    tracing::warn!(
                        "screenshot attachment failed size validation; sending text only"
                    );
                    return;
                }
                let Ok(part) = reqwest::multipart::Part::bytes(bytes)
                    .file_name(screenshot.filename.clone())
                    .mime_str(&screenshot.content_type)
                else {
                    tracing::warn!("screenshot attachment had an invalid content type");
                    return;
                };
                let form = reqwest::multipart::Form::new()
                    .text("payload_json", payload.to_string())
                    .part("files[0]", part);
                self.http.post(url).multipart(form)
            }
            None => self.http.post(url).json(&payload),
        };

        match request.send().await {
            Ok(response) if response.status().is_success() => {}
            Ok(response) => {
                tracing::warn!(
                    status = response.status().as_u16(),
                    "Discord follow-up failed"
                )
            }
            Err(_) => {
                // Do not log the webhook URL: it embeds the interaction token.
                tracing::warn!("Discord follow-up failed");
            }
        }
    }
}

pub async fn serve(config: BackendConfig) -> anyhow::Result<()> {
    let store = Store::open(&config.database_url)
        .await
        .map_err(|err| anyhow::anyhow!("could not open backend database: {err}"))?;
    let bind = config.bind.clone();
    let public = config.public_summary();
    let state = web::Data::new(
        BackendState::new(store, config)
            .map_err(|err| anyhow::anyhow!("could not initialize backend: {err}"))?,
    );

    tracing::info!(?public, "SpiderClient remote backend starting");
    spawn_watchers(state.clone());

    HttpServer::new(move || {
        App::new()
            .app_data(state.clone())
            .app_data(web::PayloadConfig::new(1024 * 1024))
            .route("/health", web::get().to(health))
            .route("/v1/pair/claim", web::post().to(pair_claim))
            .service(
                web::resource("/v1/agent/heartbeat")
                    // A heartbeat can carry an encoded screenshot in a result.
                    .app_data(web::PayloadConfig::new(
                        crate::protocol::MAX_SCREENSHOT_BASE64_CHARS + 512 * 1024,
                    ))
                    .route(web::post().to(agent_heartbeat)),
            )
            .service(
                web::resource("/v1/agent/command-result")
                    .app_data(web::PayloadConfig::new(
                        crate::protocol::MAX_SCREENSHOT_BASE64_CHARS + 512 * 1024,
                    ))
                    .route(web::post().to(command_result)),
            )
            .route("/interactions", web::post().to(interactions))
    })
    .bind(&bind)?
    .run()
    .await?;
    Ok(())
}

async fn health() -> HttpResponse {
    HttpResponse::Ok().json(json!({
        "ok": true,
        "service": "spiderclient-remote",
        "protocol": PROTOCOL_VERSION,
        "time": Utc::now(),
    }))
}

async fn pair_claim(
    req: HttpRequest,
    state: web::Data<BackendState>,
    body: web::Json<PairClaim>,
) -> HttpResponse {
    let client = req
        .peer_addr()
        .map(|addr| addr.ip().to_string())
        .unwrap_or_else(|| "unknown".into());
    if !state.allow_pair_attempt(&client) {
        return api_error(429, "rate_limited", "too many pairing attempts; try later");
    }
    let claim = body.into_inner();
    if claim.protocol != PROTOCOL_VERSION {
        return api_error(400, "protocol_mismatch", "unsupported protocol version");
    }
    if let Err(message) = validate_device_name(&claim.device_name) {
        return api_error(400, "invalid_device_name", &message);
    }
    let capabilities = claim
        .capabilities
        .allowed_by(&state.config.policy_capabilities());
    match state
        .store
        .claim_pairing_code(
            &claim.pairing_code,
            claim.device_name.trim(),
            &claim.platform,
            &capabilities,
        )
        .await
    {
        Ok((device_id, token)) => {
            tracing::info!(%device_id, "device paired");
            HttpResponse::Ok().json(PairResponse {
                protocol: PROTOCOL_VERSION,
                device_id,
                device_token: token,
                server_time: Utc::now(),
            })
        }
        Err(StoreError::InvalidPairingCode) => api_error(
            400,
            "invalid_pairing_code",
            "that pairing code is invalid or has already been used",
        ),
        Err(StoreError::ExpiredPairingCode) => api_error(
            410,
            "expired_pairing_code",
            "that pairing code has expired; create a new one",
        ),
        Err(err) => {
            tracing::warn!(error = %err, "pairing failed");
            api_error(500, "pairing_failed", "pairing could not be completed")
        }
    }
}

async fn agent_heartbeat(
    req: HttpRequest,
    state: web::Data<BackendState>,
    body: web::Json<AgentHeartbeat>,
) -> HttpResponse {
    let device = match authenticate(&req, &state).await {
        Ok(device) => device,
        Err(response) => return response,
    };
    let heartbeat = body.into_inner();
    if heartbeat.protocol != PROTOCOL_VERSION {
        return api_error(400, "protocol_mismatch", "unsupported protocol version");
    }
    if !valid_snapshot(&heartbeat.snapshot) {
        return api_error(400, "invalid_snapshot", "the device snapshot was invalid");
    }

    let previous = device.snapshot.clone();
    let was_online = device.online;
    // Never persist a capability the server policy does not grant, even if a
    // modified agent claims it.
    let mut snapshot = heartbeat.snapshot;
    snapshot.capabilities = snapshot
        .capabilities
        .allowed_by(&state.config.policy_capabilities());
    for session in &mut snapshot.sessions {
        session.capabilities = session
            .capabilities
            .allowed_by(&state.config.policy_capabilities());
    }
    if let Err(err) = state.store.record_heartbeat(&device.id, &snapshot).await {
        tracing::warn!(error = %err, "could not record heartbeat");
        return api_error(500, "heartbeat_failed", "heartbeat could not be stored");
    }

    for result in &heartbeat.results {
        if !valid_result(result) {
            tracing::warn!(command_id = %result.command_id, "rejected malformed command result");
            continue;
        }
        match state.store.complete_command(&device.id, result).await {
            Ok(Some(completed)) => {
                if let Some(token) = completed.interaction_token {
                    let state = state.clone();
                    let message = format_result(&completed.kind, result);
                    let screenshot = result.screenshot.clone();
                    actix_web::rt::spawn(async move {
                        state
                            .follow_up_with_screenshot(&token, message, screenshot.as_ref())
                            .await;
                    });
                }
            }
            Ok(None) => {}
            Err(StoreError::CommandNotFound) => {
                tracing::warn!(command_id = %result.command_id, "unknown command result")
            }
            Err(err) => tracing::warn!(error = %err, "could not record command result"),
        }
    }

    notify_session_transitions(&state, &device, &previous, &snapshot, was_online).await;

    let commands = match state.store.claim_pending_commands(&device.id, 8).await {
        Ok(commands) => commands,
        Err(err) => {
            tracing::warn!(error = %err, "could not claim commands");
            Vec::new()
        }
    };
    if !commands.is_empty() {
        let ids: Vec<String> = commands.iter().map(|command| command.id.clone()).collect();
        if let Err(err) = state
            .store
            .mark_audit_agent_received(&device.id, &ids)
            .await
        {
            tracing::warn!(error = %err, "could not mark audit rows as received");
        }
    }
    HttpResponse::Ok().json(HeartbeatResponse {
        protocol: PROTOCOL_VERSION,
        server_time: Utc::now(),
        commands,
    })
}

async fn command_result(
    req: HttpRequest,
    state: web::Data<BackendState>,
    body: web::Json<CommandResult>,
) -> HttpResponse {
    let device = match authenticate(&req, &state).await {
        Ok(device) => device,
        Err(response) => return response,
    };
    let result = body.into_inner();
    if !valid_result(&result) {
        return api_error(400, "invalid_result", "the command result was invalid");
    }
    match state.store.complete_command(&device.id, &result).await {
        Ok(Some(completed)) => {
            if let Some(token) = completed.interaction_token {
                let message = format_result(&completed.kind, &result);
                state
                    .follow_up_with_screenshot(&token, message, result.screenshot.as_ref())
                    .await;
            }
            HttpResponse::Ok().json(json!({"ok": true}))
        }
        Ok(None) => HttpResponse::Ok().json(json!({"ok": true, "duplicate": true})),
        Err(StoreError::CommandNotFound) => {
            api_error(404, "unknown_command", "that command is unknown or expired")
        }
        Err(err) => {
            tracing::warn!(error = %err, "could not record command result");
            api_error(500, "result_failed", "the result could not be stored")
        }
    }
}

async fn interactions(
    req: HttpRequest,
    state: web::Data<BackendState>,
    body: web::Bytes,
) -> HttpResponse {
    let signature = req
        .headers()
        .get("x-signature-ed25519")
        .and_then(|value| value.to_str().ok());
    let timestamp = req
        .headers()
        .get("x-signature-timestamp")
        .and_then(|value| value.to_str().ok());
    let (Some(signature), Some(timestamp)) = (signature, timestamp) else {
        return api_error(
            401,
            "missing_signature",
            "Discord signature headers are required",
        );
    };
    if !verify_discord_signature(
        &state.config.public_key,
        signature,
        timestamp,
        &body,
        Utc::now().timestamp(),
    ) {
        return api_error(
            401,
            "invalid_signature",
            "Discord signature verification failed",
        );
    }
    let interaction: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => return api_error(400, "invalid_payload", "invalid interaction payload"),
    };
    match discord::handle_interaction(state.get_ref(), interaction).await {
        Some(response) => HttpResponse::Ok().json(response),
        None => api_error(400, "unsupported_interaction", "unsupported interaction"),
    }
}

async fn authenticate(
    req: &HttpRequest,
    state: &BackendState,
) -> Result<DeviceRecord, HttpResponse> {
    let token = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| bearer_token(Some(value)));
    let Some(token) = token else {
        return Err(api_error(
            401,
            "missing_token",
            "a device bearer token is required",
        ));
    };
    state.store.authenticate(token).await.map_err(|err| {
        tracing::warn!(error = %err, "device authentication failed");
        api_error(401, "invalid_token", "device authentication failed")
    })
}

fn valid_snapshot(snapshot: &DeviceSnapshot) -> bool {
    if snapshot.protocol != PROTOCOL_VERSION {
        return false;
    }
    if snapshot.sessions.len() > crate::protocol::MAX_SESSIONS {
        return false;
    }
    let age = Utc::now().signed_duration_since(snapshot.captured_at);
    if age.num_seconds().abs() > 900 {
        return false;
    }
    snapshot.sessions.iter().all(|session| {
        !session.id.is_empty()
            && session.id.len() <= 128
            && session.pid > 0
            && session.profile.chars().count() <= 128
            && session.bridge.as_ref().is_none_or(valid_bridge)
    })
}

/// Telemetry arrives from the agent, which in turn relays it from the mod. The
/// agent bounds it too, but the backend never relies on another process having
/// validated a value it is about to show to a Discord user.
fn valid_bridge(bridge: &crate::protocol::BridgeSnapshot) -> bool {
    let text = |value: &str, max: usize| {
        !value.is_empty() && value.chars().count() <= max && !value.chars().any(char::is_control)
    };
    if !text(&bridge.state, 16)
        || !text(&bridge.mc_version, 64)
        || !text(&bridge.loader, 64)
        || !text(&bridge.bridge_version, 64)
        || !text(&bridge.baritone_integration, 32)
    {
        return false;
    }
    if bridge
        .dimension
        .as_deref()
        .is_some_and(|dimension| !text(dimension, 128))
        || bridge
            .player_name
            .as_deref()
            .is_some_and(|name| !text(name, 32))
    {
        return false;
    }
    if bridge.age_seconds.is_some_and(|age| !(0..=86_400).contains(&age)) {
        return false;
    }
    if bridge.position.as_ref().is_some_and(|p| !valid_position(p)) {
        return false;
    }
    if bridge
        .health
        .is_some_and(|health| !health.is_finite() || !(0.0..=1024.0).contains(&health))
        || bridge.food_level.is_some_and(|food| food > 20)
        || bridge.fps.is_some_and(|fps| fps > 20_000)
    {
        return false;
    }
    // The mod never reports a position without a world, and neither may a
    // tampered snapshot. Zero-filled coordinates are exactly what this guards.
    if bridge.position.is_some() && (bridge.dimension.is_none() || bridge.state != "in_world") {
        return false;
    }
    // `fresh` is only meaningful together with an arrival age.
    bridge.age_seconds.is_some() || !bridge.fresh
}

fn valid_position(position: &crate::protocol::PositionSnapshot) -> bool {
    position.x.is_finite()
        && position.y.is_finite()
        && position.z.is_finite()
        && position.yaw.is_finite()
        && position.pitch.is_finite()
        && position.x.abs() <= 30_000_000.0
        && position.z.abs() <= 30_000_000.0
        && (-4096.0..=4096.0).contains(&position.y)
}

fn valid_result(result: &CommandResult) -> bool {
    !result.command_id.is_empty()
        && result.command_id.len() <= 128
        && result.message.chars().count() <= 2_000
        && result
            .log_excerpt
            .as_ref()
            .is_none_or(|log| log.len() <= 70 * 1024)
        && result
            .screenshot
            .as_ref()
            .is_none_or(ScreenshotAttachment::is_acceptable)
        && result.chat.as_ref().is_none_or(|chat| {
            chat.validate().is_ok()
                && chat.session_id.len() <= 128
                && !chat.session_id.chars().any(char::is_control)
        })
        && result
            .execution
            .as_ref()
            .is_none_or(|execution| execution.validate().is_ok())
}

fn format_result(kind: &crate::protocol::CommandKind, result: &CommandResult) -> String {
    let unknown = result
        .execution
        .as_ref()
        .is_some_and(|execution| execution.state == "unknown");
    let mut message = if result.success {
        format!("**{}** completed: {}", kind.as_str(), result.message)
    } else if unknown {
        format!("**{}** outcome unknown: {}", kind.as_str(), result.message)
    } else {
        format!("**{}** could not run: {}", kind.as_str(), result.message)
    };
    if let Some(execution) = &result.execution {
        message.push_str(&format!(
            "\nSubmission: **{}**",
            match execution.state.as_str() {
                "submitted_verified" => "submitted and verified",
                "submitted_unverified" => "submitted by the client, outcome unverified",
                "rejected" => "not submitted (rejected)",
                "unknown" => "unknown; the request may still have been submitted",
                _ => "not submitted",
            }
        ));
        if let Some(detail) = &execution.detail {
            message.push_str(&format!("\n{}", truncate_text(&redact(detail), 300)));
        }
    }
    if let Some(chat) = &result.chat {
        let mut block = String::new();
        for entry in &chat.messages {
            let sender = entry.sender.as_deref().unwrap_or(match entry.kind.as_str() {
                "chat" => "chat",
                "action_bar" => "action bar",
                _ => "system",
            });
            block.push_str(&format!(
                "[{}] {}: {}\n",
                entry.captured_at.format("%H:%M:%S"),
                sender,
                redact(&entry.text)
            ));
        }
        if block.is_empty() {
            block.push_str("No messages were retained in the in-game buffer.\n");
        }
        message.push_str("\n```\n");
        let budget = 1_700usize.saturating_sub(message.len());
        message.push_str(&truncate_text(&block, budget));
        message.push_str("\n```");
    }
    if let Some(log) = &result.log_excerpt {
        message.push_str("\n```\n");
        let budget = 1_700usize.saturating_sub(message.len());
        message.push_str(&truncate_text(&redact(log), budget));
        message.push_str("\n```");
    }
    truncate_text(&message, 1_900)
}

async fn notify_session_transitions(
    state: &BackendState,
    device: &DeviceRecord,
    previous: &Option<DeviceSnapshot>,
    next: &DeviceSnapshot,
    was_online: bool,
) {
    if state.config.alert_channel_id.is_none() {
        return;
    }
    if !was_online && state.alert_allowed(&format!("online:{}", device.id)) {
        state
            .send_alert(format!(
                "**{}** came online (agent {}).",
                device.name, next.agent_version
            ))
            .await;
    }
    let Some(previous) = previous else {
        return;
    };
    let previous_ids: std::collections::HashSet<&str> = previous
        .sessions
        .iter()
        .map(|session| session.id.as_str())
        .collect();
    for session in &next.sessions {
        if !previous_ids.contains(session.id.as_str())
            && state.alert_allowed(&format!("start:{}:{}", device.id, session.id))
        {
            state
                .send_alert(format!(
                    "**{}**: session started for `{}` (PID {}).",
                    device.name,
                    session.username.as_deref().unwrap_or(&session.profile),
                    session.pid
                ))
                .await;
        }
    }
    let next_ids: std::collections::HashSet<&str> = next
        .sessions
        .iter()
        .map(|session| session.id.as_str())
        .collect();
    for session in &previous.sessions {
        if !next_ids.contains(session.id.as_str())
            && state.alert_allowed(&format!("stop:{}:{}", device.id, session.id))
        {
            state
                .send_alert(format!(
                    "**{}**: session ended for `{}`.",
                    device.name,
                    session.username.as_deref().unwrap_or(&session.profile)
                ))
                .await;
        }
    }
}

fn spawn_watchers(state: web::Data<BackendState>) {
    let offline = state.clone();
    actix_web::rt::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(15)).await;
            match offline
                .store
                .mark_stale_offline(Duration::from_secs(HEARTBEAT_STALE_SECONDS))
                .await
            {
                Ok(devices) => {
                    for device in devices {
                        if offline.config.alert_channel_id.is_some()
                            && offline.alert_allowed(&format!("offline:{}", device.id))
                        {
                            offline
                                .send_alert(format!(
                                    "**{}** went offline; no new Minecraft commands can reach it.",
                                    device.name
                                ))
                                .await;
                        }
                    }
                }
                Err(err) => tracing::warn!(error = %err, "offline watcher failed"),
            }
        }
    });

    let expiry = state.clone();
    actix_web::rt::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(10)).await;
            match expiry.store.expire_commands().await {
                Ok(commands) => {
                    for command in commands {
                        if let Some(token) = command.interaction_token {
                            expiry
                                .follow_up(
                                    &token,
                                    format!(
                                        "**{}** did not complete: {}",
                                        command.kind.as_str(),
                                        command.result.message
                                    ),
                                )
                                .await;
                        }
                    }
                }
                Err(err) => tracing::warn!(error = %err, "command expiry watcher failed"),
            }
        }
    });

    let cleanup = state;
    actix_web::rt::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(6 * 60 * 60)).await;
            let days = cleanup.config.audit_retention_days;
            if let Err(err) = cleanup.store.cleanup(days).await {
                tracing::warn!(error = %err, "backend cleanup failed");
            }
        }
    });
}

fn api_error(status: u16, code: &str, message: &str) -> HttpResponse {
    let mut builder = match status {
        400 => HttpResponse::BadRequest(),
        401 => HttpResponse::Unauthorized(),
        404 => HttpResponse::NotFound(),
        410 => HttpResponse::Gone(),
        429 => HttpResponse::TooManyRequests(),
        _ => HttpResponse::InternalServerError(),
    };
    builder.json(ApiError {
        error: code.to_string(),
        message: message.to_string(),
    })
}

/// Create a pairing code from the backend CLI, printing it exactly once.
pub async fn create_pairing_code(
    config: BackendConfig,
    owner_discord_id: &str,
    ttl_seconds: i64,
) -> anyhow::Result<()> {
    if owner_discord_id.is_empty() || !owner_discord_id.chars().all(|c| c.is_ascii_digit()) {
        anyhow::bail!("owner Discord ID must be numeric");
    }
    let store = Store::open(&config.database_url).await?;
    let code = store
        .create_pairing_code(owner_discord_id, ttl_seconds)
        .await?;
    println!("SpiderClient pairing code (single use):");
    println!("  {}", code.code);
    println!("Owner Discord ID: {owner_discord_id}");
    println!("Expires: {}", code.expires_at.to_rfc3339());
    println!("Run `spider-agent pair --code ...` on the gaming PC.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_parsed_strictly() {
        assert_eq!(
            split_ids("123, 456,not-an-id,789"),
            vec!["123".to_string(), "456".to_string(), "789".to_string()]
        );
    }

    #[test]
    fn guild_restriction_is_fail_closed_when_configured() {
        let config = BackendConfig {
            bind: "127.0.0.1:0".into(),
            database_url: "sqlite::memory:".into(),
            bot_token: "not-used".into(),
            public_key: "00".repeat(32),
            application_id: "1".into(),
            admin_discord_ids: vec![],
            allowed_guild_ids: vec!["42".into()],
            alert_channel_id: None,
            website_api_token: None,
            allow_launch: false,
            allow_stop: false,
            allow_screenshot: false,
            allow_coordinates: false,
            allow_chat: false,
            allow_chat_send: false,
            allow_minecraft_command: false,
            command_policy: crate::command_policy::CommandPolicy::default(),
            require_command_confirmation: true,
            audit_retention_days: 7,
            pair_ttl_seconds: 600,
        };
        assert!(config.guild_allowed(Some("42")));
        assert!(!config.guild_allowed(Some("43")));
        assert!(!config.guild_allowed(None));
    }

    #[test]
    fn public_summary_does_not_contain_secrets() {
        let config = BackendConfig {
            bind: "127.0.0.1:0".into(),
            database_url: "sqlite::memory:".into(),
            bot_token: "super-secret-token".into(),
            public_key: "ab".repeat(32),
            application_id: "1".into(),
            admin_discord_ids: vec!["123".into()],
            allowed_guild_ids: vec![],
            alert_channel_id: Some("456".into()),
            website_api_token: Some("website-secret".into()),
            allow_launch: false,
            allow_stop: false,
            allow_screenshot: false,
            allow_coordinates: false,
            allow_chat: false,
            allow_chat_send: false,
            allow_minecraft_command: false,
            command_policy: crate::command_policy::CommandPolicy::default(),
            require_command_confirmation: true,
            audit_retention_days: 7,
            pair_ttl_seconds: 600,
        };
        let summary = config.public_summary().to_string();
        assert!(!summary.contains("super-secret-token"));
        assert!(!summary.contains("website-secret"));
        assert!(!summary.contains(&"ab".repeat(32)));
    }

    #[test]
    fn result_limits_reject_oversized_logs() {
        let result = CommandResult {
            command_id: "c".into(),
            success: true,
            message: "ok".into(),
            log_excerpt: Some("x".repeat(71 * 1024)),
            screenshot: None,
            chat: None,
            execution: None,
            completed_at: Utc::now(),
        };
        assert!(!valid_result(&result));
    }

    #[test]
    fn result_limits_reject_invalid_screenshots() {
        let mut result = CommandResult::text("c", true, "captured", None);
        result.screenshot = Some(ScreenshotAttachment {
            content_type: "image/png".into(),
            filename: "shot.png".into(),
            width: 320,
            height: 200,
            data_base64: "A".repeat(crate::protocol::MAX_SCREENSHOT_BASE64_CHARS + 1),
        });
        assert!(!valid_result(&result));

        result.screenshot = Some(ScreenshotAttachment {
            content_type: "image/gif".into(),
            filename: "shot.gif".into(),
            width: 320,
            height: 200,
            data_base64: "AAAA".into(),
        });
        assert!(!valid_result(&result));
    }

    #[test]
    fn execution_is_reported_as_unverified_or_unknown_but_never_verified() {
        let submitted = CommandResult {
            execution: Some(crate::protocol::ExecutionStatus::submitted_unverified(
                "ClientPacketListener.sendChat accepted the line",
            )),
            ..CommandResult::text("c", true, "the message was submitted", None)
        };
        assert!(valid_result(&submitted));
        let text = format_result(&crate::protocol::CommandKind::Say, &submitted);
        assert!(text.contains("submitted by the client, outcome unverified"));
        assert!(!text.contains("submitted and verified"));

        let unknown = CommandResult {
            execution: Some(crate::protocol::ExecutionStatus::unknown(
                "the client did not confirm the submission",
            )),
            ..CommandResult::text("c", false, "the game did not confirm the line", None)
        };
        assert!(valid_result(&unknown));
        let text = format_result(&crate::protocol::CommandKind::Say, &unknown);
        assert!(text.contains("outcome unknown"));
        assert!(!text.contains("could not run"));
        assert!(text.contains("may still have been submitted"));

        let rejected = CommandResult {
            execution: Some(crate::protocol::ExecutionStatus::rejected(
                "no player is in a world",
            )),
            ..CommandResult::text("c", false, "no player is in a world", None)
        };
        let text = format_result(&crate::protocol::CommandKind::MinecraftCommand, &rejected);
        assert!(text.contains("not submitted (rejected)"));
    }

    #[test]
    fn a_verified_execution_must_also_be_submitted() {
        let invalid = crate::protocol::ExecutionStatus {
            submitted: false,
            verified: true,
            state: "submitted_verified".into(),
            detail: None,
        };
        assert!(invalid.validate().is_err());
        let result = CommandResult {
            execution: Some(invalid),
            ..CommandResult::text("c", true, "claimed", None)
        };
        assert!(!valid_result(&result));
    }

    fn bridge(state: &str, with_position: bool, age: Option<i64>) -> crate::protocol::BridgeSnapshot {
        crate::protocol::BridgeSnapshot {
            state: state.into(),
            mc_version: "1.21.1".into(),
            loader: "fabric".into(),
            bridge_version: "0.1.0".into(),
            dimension: with_position.then(|| "minecraft:overworld".into()),
            position: with_position.then(|| crate::protocol::PositionSnapshot {
                x: 0.5,
                y: 64.0,
                z: -12.5,
                yaw: 90.0,
                pitch: 0.0,
            }),
            player_name: Some("Alex".into()),
            health: Some(20.0),
            food_level: Some(20),
            fps: Some(120),
            age_seconds: age,
            fresh: age.is_some_and(|seconds| seconds <= 10),
            baritone_detected: false,
            baritone_integration: "unavailable".into(),
        }
    }

    #[test]
    fn a_consistent_bridge_sample_is_accepted() {
        assert!(valid_bridge(&bridge("in_world", true, Some(1))));
        assert!(valid_bridge(&bridge("menu", false, Some(3))));
        assert!(valid_bridge(&bridge("no_player", false, Some(0))));
        // Connected but no sample yet: no position, no arrival age.
        assert!(valid_bridge(&bridge("menu", false, None)));
    }

    #[test]
    fn a_position_without_a_world_is_rejected() {
        // Zero coordinates from a menu are exactly the fabrication this
        // validation exists to stop.
        let mut fake = bridge("menu", false, Some(1));
        fake.position = Some(crate::protocol::PositionSnapshot {
            x: 0.0,
            y: 64.0,
            z: 0.0,
            yaw: 0.0,
            pitch: 0.0,
        });
        assert!(!valid_bridge(&fake));

        let mut no_world = bridge("in_world", true, Some(1));
        no_world.dimension = None;
        assert!(!valid_bridge(&no_world));
    }

    #[test]
    fn impossible_ages_and_values_are_rejected() {
        assert!(!valid_bridge(&bridge("in_world", true, Some(-1))));
        assert!(!valid_bridge(&bridge("in_world", true, Some(86_401))));

        let mut bad = bridge("in_world", true, Some(1));
        bad.position.as_mut().unwrap().x = f64::NAN;
        assert!(!valid_bridge(&bad));

        let mut bad = bridge("in_world", true, Some(1));
        bad.food_level = Some(21);
        assert!(!valid_bridge(&bad));

        let mut bad = bridge("in_world", true, Some(1));
        bad.health = Some(10_000.0);
        assert!(!valid_bridge(&bad));

        let mut bad = bridge("in_world", true, Some(1));
        bad.dimension = Some("bad\ndimension".into());
        assert!(!valid_bridge(&bad));
    }

    #[test]
    fn a_fresh_flag_without_an_age_is_rejected() {
        let mut sample = bridge("in_world", true, None);
        sample.fresh = true;
        assert!(
            !valid_bridge(&sample),
            "fresh requires an arrival age so it can actually be checked"
        );
    }
}
