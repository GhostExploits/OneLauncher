//! Versioned wire types shared by the agent and the trusted backend.
//!
//! The protocol deliberately carries no Minecraft access tokens, Microsoft
//! refresh tokens, or account UUIDs. The agent reports the display name that is
//! already visible in the local process arguments, plus a stable per-process
//! session ID.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_LOG_LINES: usize = 200;
pub const MAX_LOG_BYTES: usize = 64 * 1024;
pub const MAX_SESSIONS: usize = 64;
pub const MAX_COMMAND_TTL_SECONDS: i64 = 120;
pub const MAX_DEVICE_NAME_CHARS: usize = 64;
pub const MAX_PAIRING_CODE_CHARS: usize = 32;
/// Encoded image bytes accepted from an agent. Base64 adds about a third on
/// the wire, so the HTTP body limit is derived from this value instead.
pub const MAX_SCREENSHOT_BYTES: usize = 6 * 1024 * 1024;
pub const MAX_SCREENSHOT_BASE64_CHARS: usize = MAX_SCREENSHOT_BYTES.div_ceil(3) * 4;
/// Capture is downscaled before encoding; this is the widest edge we keep.
pub const MAX_SCREENSHOT_DIMENSION: u32 = 1920;
pub const SCREENSHOT_COOLDOWN_SECONDS: u64 = 10;
/// Vanilla caps a chat message at 256 characters. The remote path enforces the
/// same bound instead of relying on the server to truncate or reject it.
pub const MAX_CHAT_MESSAGE_CHARS: usize = 256;
/// One command line, without a leading slash. Vanilla uses the same limit.
pub const MAX_MINECRAFT_COMMAND_CHARS: usize = 256;
pub const MAX_CHAT_MESSAGES: usize = 200;
pub const MAX_CHAT_TEXT_CHARS: usize = 512;
pub const MAX_CHAT_SENDER_CHARS: usize = 64;
pub const MAX_CHAT_HISTORY_CHARS: usize = 32 * 1024;
pub const CHAT_COOLDOWN_SECONDS: u64 = 2;
pub const SAY_COOLDOWN_SECONDS: u64 = 3;
pub const MINECRAFT_COMMAND_COOLDOWN_SECONDS: u64 = 3;
pub const MAX_AUDIT_ROWS: usize = 25;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Capabilities {
    pub logs: bool,
    pub process_list: bool,
    pub launch: bool,
    pub stop: bool,
    /// These are false unless a real, separately maintained integration exists.
    pub screenshot: bool,
    pub coordinates: bool,
    /// Read-only chat history collected by the SpiderBridge mod.
    #[serde(default)]
    pub chat: bool,
    /// Sending a chat line through the game client.
    #[serde(default)]
    pub chat_send: bool,
    /// Submitting a policy-checked Minecraft command through the game client.
    #[serde(default)]
    pub minecraft_command: bool,
    pub baritone_control: bool,
    pub performance: bool,
}

impl Default for Capabilities {
    fn default() -> Self {
        Self {
            logs: true,
            process_list: true,
            launch: false,
            stop: false,
            screenshot: false,
            coordinates: false,
            chat: false,
            chat_send: false,
            minecraft_command: false,
            baritone_control: false,
            performance: true,
        }
    }
}

impl Capabilities {
    pub fn supports(&self, kind: &CommandKind) -> bool {
        match kind {
            CommandKind::GetLogs => self.logs,
            CommandKind::Launch => self.launch,
            CommandKind::Stop => self.stop,
            CommandKind::Screenshot => self.screenshot,
            CommandKind::Coordinates => self.coordinates,
            CommandKind::ChatLogs => self.chat,
            CommandKind::Say => self.chat_send,
            CommandKind::MinecraftCommand => self.minecraft_command,
            CommandKind::BaritoneStatus | CommandKind::BaritoneControl => self.baritone_control,
            CommandKind::Pause | CommandKind::Resume => {
                // Pausing is an in-game operation; there is no generic process
                // pause that is safe to expose as a game action.
                false
            }
            CommandKind::GetStatus | CommandKind::GetMods => true,
        }
    }

    /// Intersect the device's declared capabilities with the server's own
    /// policy. A device cannot gain a permission by claiming it.
    pub fn allowed_by(&self, policy: &Capabilities) -> Self {
        Self {
            logs: self.logs && policy.logs,
            process_list: self.process_list && policy.process_list,
            launch: self.launch && policy.launch,
            stop: self.stop && policy.stop,
            screenshot: self.screenshot && policy.screenshot,
            coordinates: self.coordinates && policy.coordinates,
            chat: self.chat && policy.chat,
            chat_send: self.chat_send && policy.chat_send,
            minecraft_command: self.minecraft_command && policy.minecraft_command,
            baritone_control: self.baritone_control && policy.baritone_control,
            performance: self.performance && policy.performance,
        }
    }

    pub fn labels(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        for (enabled, label) in [
            (self.process_list, "sessions"),
            (self.logs, "logs"),
            (self.performance, "performance"),
            (self.launch, "launch"),
            (self.stop, "stop"),
            (self.screenshot, "screenshot"),
            (self.coordinates, "coordinates"),
            (self.chat, "chat history"),
            (self.chat_send, "chat send"),
            (self.minecraft_command, "minecraft command"),
            (self.baritone_control, "baritone control"),
        ] {
            if enabled {
                out.push(label);
            }
        }
        out
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CommandKind {
    GetLogs,
    Launch,
    Stop,
    Screenshot,
    GetStatus,
    GetMods,
    Coordinates,
    ChatLogs,
    Say,
    MinecraftCommand,
    BaritoneStatus,
    BaritoneControl,
    Pause,
    Resume,
}

impl CommandKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::GetLogs => "logs",
            Self::Launch => "launch",
            Self::Stop => "stop",
            Self::Screenshot => "screenshot",
            Self::GetStatus => "status",
            Self::GetMods => "mods",
            Self::Coordinates => "coordinates",
            Self::ChatLogs => "chat",
            Self::Say => "say",
            Self::MinecraftCommand => "minecraft_command",
            Self::BaritoneStatus => "baritone_status",
            Self::BaritoneControl => "baritone_control",
            Self::Pause => "pause",
            Self::Resume => "resume",
        }
    }

    pub fn needs_confirmation(&self) -> bool {
        matches!(self, Self::Launch | Self::Stop | Self::BaritoneControl)
    }

    /// True when the action changes game or server state, as opposed to reading
    /// data. Used by the audit lifecycle so a submitted action is never treated
    /// as a completed one.
    pub fn is_state_changing(&self) -> bool {
        matches!(
            self,
            Self::Launch
                | Self::Stop
                | Self::Say
                | Self::MinecraftCommand
                | Self::BaritoneControl
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairClaim {
    pub protocol: u32,
    pub pairing_code: String,
    pub device_name: String,
    pub platform: String,
    pub agent_version: String,
    pub capabilities: Capabilities,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairResponse {
    pub protocol: u32,
    pub device_id: String,
    pub device_token: String,
    pub server_time: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SystemSpecs {
    pub cpu: Option<String>,
    pub physical_cores: Option<usize>,
    pub logical_cores: Option<usize>,
    pub total_memory_bytes: Option<u64>,
    pub available_memory_bytes: Option<u64>,
    pub os: Option<String>,
    pub hostname: Option<String>,
    pub gpu: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSnapshot {
    /// Stable for the life of this process. Not derived from username.
    pub id: String,
    pub profile: String,
    pub username: Option<String>,
    pub mc_version: Option<String>,
    pub loader: Option<String>,
    pub pid: u32,
    pub started_at: DateTime<Utc>,
    pub uptime_seconds: u64,
    pub memory_bytes: Option<u64>,
    pub cpu_percent: Option<f32>,
    pub mods_count: Option<usize>,
    pub baritone_detected: bool,
    /// Live data from the SpiderBridge mod, when one is connected to this
    /// session. Absent means the mod is not installed or not connected; it is
    /// never synthesized from process metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bridge: Option<BridgeSnapshot>,
    /// "running" is the only state observed by the process monitor.
    pub state: String,
    pub capabilities: Capabilities,
}

/// A point-in-time copy of the mod's telemetry as the agent last received it.
/// `age_seconds` and `fresh` let the backend refuse to present stale data as
/// live without having to trust the agent's clock.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeSnapshot {
    /// `in_world`, `menu`, or `no_player`.
    pub state: String,
    pub mc_version: String,
    pub loader: String,
    pub bridge_version: String,
    pub dimension: Option<String>,
    pub position: Option<PositionSnapshot>,
    pub player_name: Option<String>,
    pub health: Option<f32>,
    pub food_level: Option<u32>,
    pub fps: Option<u32>,
    /// Seconds since the agent received this sample. `None` means the mod is
    /// connected but has not sent telemetry yet, which is not the same as zero.
    pub age_seconds: Option<i64>,
    /// True when the sample is recent enough to show as live.
    pub fresh: bool,
    pub baritone_detected: bool,
    pub baritone_integration: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PositionSnapshot {
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub yaw: f32,
    pub pitch: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceSnapshot {
    pub protocol: u32,
    pub agent_version: String,
    pub platform: String,
    pub specs: SystemSpecs,
    pub sessions: Vec<SessionSnapshot>,
    pub capabilities: Capabilities,
    pub captured_at: DateTime<Utc>,
    pub agent_started_at: DateTime<Utc>,
    pub log_excerpt_supported: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentHeartbeat {
    pub protocol: u32,
    pub snapshot: DeviceSnapshot,
    pub results: Vec<CommandResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeartbeatResponse {
    pub protocol: u32,
    pub server_time: DateTime<Utc>,
    pub commands: Vec<CommandRequest>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandRequest {
    pub id: String,
    pub kind: CommandKind,
    pub session_id: Option<String>,
    pub payload: Value,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

impl CommandRequest {
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        now >= self.expires_at
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandResult {
    pub command_id: String,
    pub success: bool,
    pub message: String,
    /// Bounded, redacted text. Only log requests populate this.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log_excerpt: Option<String>,
    /// A real freshly captured game-window image. Never persisted in the
    /// command database; the backend relays it to Discord and drops it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screenshot: Option<ScreenshotAttachment>,
    /// Chat history as captured by the game integration. Never synthesized
    /// from `latest.log`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat: Option<ChatHistory>,
    /// Explicit distinction between "the client submitted it" and "the result
    /// was observed". A submitted command is not a completed one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<ExecutionStatus>,
    pub completed_at: DateTime<Utc>,
}

impl CommandResult {
    /// A text-only result. Keeps every constructor explicit about the absence
    /// of an image instead of silently reusing an old one.
    pub fn text(
        command_id: impl Into<String>,
        success: bool,
        message: impl Into<String>,
        log_excerpt: Option<String>,
    ) -> Self {
        Self {
            command_id: command_id.into(),
            success,
            message: message.into(),
            log_excerpt,
            screenshot: None,
            chat: None,
            execution: None,
            completed_at: Utc::now(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatMessage {
    /// Monotonic per-game-process sequence number. Used only for ordering and
    /// duplicate avoidance, never as a cross-session identity.
    pub seq: u64,
    pub captured_at: DateTime<Utc>,
    /// `chat`, `system`, or `action_bar`.
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender: Option<String>,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatHistory {
    /// The session the mod belonged to when it answered. The agent checks this
    /// against the requested session, so a misrouted reply is rejected.
    pub session_id: String,
    /// True only while the mod is collecting. A disabled answer is not history.
    pub collection_enabled: bool,
    /// Oldest message still retained by the bounded buffer.
    pub collected_since: Option<DateTime<Utc>>,
    /// Number of messages the bounded buffer discarded.
    pub dropped: u64,
    pub messages: Vec<ChatMessage>,
}

impl ChatHistory {
    pub fn validate(&self) -> Result<(), String> {
        if self.messages.len() > MAX_CHAT_MESSAGES {
            return Err("chat history contains too many messages".into());
        }
        let mut bytes = 0usize;
        let mut previous = None;
        for message in &self.messages {
            if !matches!(message.kind.as_str(), "chat" | "system" | "action_bar") {
                return Err("chat history contains an unknown message kind".into());
            }
            if message.text.is_empty()
                || message.text.chars().count() > MAX_CHAT_TEXT_CHARS
                || message.text.chars().any(char::is_control)
            {
                return Err("chat history contains invalid text".into());
            }
            if message
                .sender
                .as_deref()
                .is_some_and(|sender| {
                    sender.is_empty()
                        || sender.chars().count() > MAX_CHAT_SENDER_CHARS
                        || sender.chars().any(char::is_control)
                })
            {
                return Err("chat history contains an invalid sender".into());
            }
            if previous.is_some_and(|seen| message.seq <= seen) {
                return Err("chat history is not ordered by sequence".into());
            }
            previous = Some(message.seq);
            bytes += message.text.len() + message.sender.as_deref().map_or(0, str::len);
        }
        if bytes > MAX_CHAT_HISTORY_CHARS {
            return Err("chat history is larger than the accepted limit".into());
        }
        Ok(())
    }
}

/// The observed step of a state-changing action. `verified` is only true when
/// a reliable API event confirmed the outcome. Vanilla does not correlate
/// command output, so remote Minecraft commands stay `submitted_unverified`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecutionStatus {
    /// The game integration accepted the request and handed it to the client.
    pub submitted: bool,
    /// A reliable event confirmed the outcome. False means unknown, not failed.
    pub verified: bool,
    /// `submitted_unverified`, `submitted_verified`, `rejected`,
    /// `not_submitted`, or `unknown`.
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl ExecutionStatus {
    pub fn submitted_unverified(detail: impl Into<String>) -> Self {
        Self {
            submitted: true,
            verified: false,
            state: "submitted_unverified".into(),
            detail: Some(detail.into()),
        }
    }

    pub fn rejected(detail: impl Into<String>) -> Self {
        Self {
            submitted: false,
            verified: false,
            state: "rejected".into(),
            detail: Some(detail.into()),
        }
    }

    /// The client started the action but could not confirm it. The line may
    /// still have been submitted; this is deliberately not reported as a
    /// rejection.
    pub fn unknown(detail: impl Into<String>) -> Self {
        Self {
            submitted: false,
            verified: false,
            state: "unknown".into(),
            detail: Some(detail.into()),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if !matches!(
            self.state.as_str(),
            "submitted_unverified" | "submitted_verified" | "rejected" | "not_submitted" | "unknown"
        ) {
            return Err("unknown execution state".into());
        }
        if self.verified && !self.submitted {
            return Err("an action cannot be verified without being submitted".into());
        }
        if self
            .detail
            .as_deref()
            .is_some_and(|detail| detail.chars().count() > 512 || detail.chars().any(char::is_control))
        {
            return Err("execution detail is invalid".into());
        }
        Ok(())
    }
}

/// Base64 image payload. The backend validates the decoded size before it
/// forwards the attachment, and the agent bounds the encoded size before it
/// sends anything.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScreenshotAttachment {
    /// Only `image/png` and `image/jpeg` are accepted.
    pub content_type: String,
    pub filename: String,
    pub width: u32,
    pub height: u32,
    pub data_base64: String,
}

impl ScreenshotAttachment {
    pub fn decoded_len(&self) -> Option<usize> {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD
            .decode(&self.data_base64)
            .ok()
            .map(|bytes| bytes.len())
    }

    pub fn is_acceptable(&self) -> bool {
        let content_type_ok = matches!(self.content_type.as_str(), "image/png" | "image/jpeg");
        let filename_ok = !self.filename.is_empty()
            && self.filename.len() <= 64
            && self
                .filename
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
        content_type_ok
            && filename_ok
            && self.data_base64.len() <= MAX_SCREENSHOT_BASE64_CHARS
            && self
                .decoded_len()
                .is_some_and(|len| len > 0 && len <= MAX_SCREENSHOT_BYTES)
            && self.width > 0
            && self.height > 0
            && self.width <= MAX_SCREENSHOT_DIMENSION
            && self.height <= MAX_SCREENSHOT_DIMENSION
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiError {
    pub error: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogsPayload {
    pub lines: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatLogsPayload {
    #[serde(default = "default_chat_limit")]
    pub limit: usize,
    #[serde(default = "default_true")]
    pub include_system: bool,
    #[serde(default)]
    pub include_action_bar: bool,
}

fn default_chat_limit() -> usize {
    50
}

fn default_true() -> bool {
    true
}

impl ChatLogsPayload {
    pub fn bounded(&self) -> Self {
        Self {
            limit: self.limit.clamp(1, MAX_CHAT_MESSAGES),
            include_system: self.include_system,
            include_action_bar: self.include_action_bar,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SayPayload {
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MinecraftCommandPayload {
    pub command: String,
    #[serde(default)]
    pub confirm: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchPayload {
    pub installation: String,
    pub confirm: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StopPayload {
    pub session_id: String,
    pub confirm: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DevicePublic {
    pub id: String,
    pub name: String,
    pub owner_discord_id: String,
    pub online: bool,
    pub last_seen: Option<DateTime<Utc>>,
    pub agent_version: Option<String>,
    pub platform: Option<String>,
    pub session_count: usize,
    pub capabilities: Capabilities,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairingCode {
    pub code: String,
    pub owner_discord_id: String,
    pub expires_at: DateTime<Utc>,
}

pub fn validate_device_name(name: &str) -> Result<(), String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("device name cannot be empty".into());
    }
    if trimmed.chars().count() > MAX_DEVICE_NAME_CHARS {
        return Err(format!(
            "device name must be at most {MAX_DEVICE_NAME_CHARS} characters"
        ));
    }
    if trimmed.chars().any(|c| c.is_control()) {
        return Err("device name cannot contain control characters".into());
    }
    Ok(())
}

pub fn clamp_log_lines(lines: usize) -> usize {
    lines.clamp(1, MAX_LOG_LINES)
}

pub fn truncate_text(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &value[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_cannot_be_escalated_by_the_device() {
        let device = Capabilities {
            stop: true,
            screenshot: true,
            ..Capabilities::default()
        };
        let policy = Capabilities {
            stop: false,
            screenshot: false,
            ..Capabilities::default()
        };
        let allowed = device.allowed_by(&policy);
        assert!(!allowed.stop);
        assert!(!allowed.screenshot);
        assert!(allowed.logs);
        assert!(!allowed.supports(&CommandKind::Stop));
    }

    #[test]
    fn command_confirmation_contract_is_explicit() {
        assert!(CommandKind::Launch.needs_confirmation());
        assert!(CommandKind::Stop.needs_confirmation());
        assert!(!CommandKind::GetLogs.needs_confirmation());
    }

    #[test]
    fn log_limits_are_bounded() {
        assert_eq!(clamp_log_lines(0), 1);
        assert_eq!(clamp_log_lines(10_000), MAX_LOG_LINES);
    }

    #[test]
    fn truncation_respects_utf8_boundaries() {
        let text = "a".repeat(MAX_LOG_BYTES) + "é";
        let out = truncate_text(&text, MAX_LOG_BYTES + 1);
        assert!(out.ends_with("..."));
        assert!(out.len() <= MAX_LOG_BYTES + 4);
    }

    fn attachment(content_type: &str, bytes: usize) -> ScreenshotAttachment {
        use base64::Engine as _;
        ScreenshotAttachment {
            content_type: content_type.into(),
            filename: "minecraft.png".into(),
            width: 640,
            height: 480,
            data_base64: base64::engine::general_purpose::STANDARD.encode(vec![7u8; bytes]),
        }
    }

    #[test]
    fn valid_screenshot_attachment_is_accepted() {
        assert!(attachment("image/png", 1024).is_acceptable());
        assert!(attachment("image/jpeg", 1024).is_acceptable());
    }

    #[test]
    fn screenshot_attachment_rejects_wrong_type_names_and_sizes() {
        assert!(!attachment("image/svg+xml", 1024).is_acceptable());
        let mut bad_name = attachment("image/png", 1024);
        bad_name.filename = "../secret.png".into();
        assert!(!bad_name.is_acceptable());

        let oversized = attachment("image/png", MAX_SCREENSHOT_BYTES + 1);
        assert!(!oversized.is_acceptable());

        let mut too_wide = attachment("image/png", 16);
        too_wide.width = MAX_SCREENSHOT_DIMENSION + 1;
        assert!(!too_wide.is_acceptable());

        let mut huge_url = attachment("image/png", 16);
        huge_url.data_base64 = "A".repeat(MAX_SCREENSHOT_BASE64_CHARS + 1);
        assert!(!huge_url.is_acceptable());
    }

    #[test]
    fn screenshot_field_is_optional_on_the_wire() {
        let result = CommandResult::text("c1", true, "ok", None);
        let json = serde_json::to_value(&result).unwrap();
        assert!(json.get("screenshot").is_none());
        let decoded: CommandResult = serde_json::from_value(json).unwrap();
        assert!(decoded.screenshot.is_none());
    }
}
