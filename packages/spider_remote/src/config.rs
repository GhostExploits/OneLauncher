use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use serde::de::Error as _;
use serde::ser::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use url::Url;

use crate::AgentError;
use crate::protocol::{Capabilities, PROTOCOL_VERSION};
use crate::security::{protect_secret, unprotect_secret};

pub const CONFIG_FILE: &str = "agent.json";
pub const STATUS_FILE: &str = "agent-status.json";
pub const STOP_FILE: &str = "agent.stop";
pub const CONFIG_VERSION: u32 = 1;

/// The bearer credential issued by the trusted backend.
///
/// The in-memory value is never serialized directly. `Serialize` protects it at
/// rest and `Deserialize` accepts both the protected format and the legacy
/// plaintext format for migration. It deliberately has no `Debug` or `Display`
/// implementation so it cannot be accidentally interpolated into diagnostics.
#[derive(Clone)]
pub struct DeviceToken(String);

impl DeviceToken {
    pub fn empty() -> Self {
        Self(String::new())
    }

    pub fn new(value: String) -> Result<Self, AgentError> {
        if value.trim().len() < 32 {
            return Err(AgentError::Config(
                "the backend issued a malformed device credential".into(),
            ));
        }
        Ok(Self(value))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Serialize for DeviceToken {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let protected = protect_secret(&self.0).map_err(S::Error::custom)?;
        serializer.serialize_str(&protected)
    }
}

impl<'de> Deserialize<'de> for DeviceToken {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let stored = String::deserialize(deserializer)?;
        let plaintext = unprotect_secret(&stored).map_err(D::Error::custom)?;
        Ok(Self(plaintext))
    }
}

#[derive(Serialize, Deserialize)]
pub struct AgentConfig {
    pub version: u32,
    pub server_url: String,
    pub device_id: String,
    /// Bearer credential issued by the trusted backend. Never log this field.
    pub device_token: DeviceToken,
    pub device_name: String,
    pub launcher_path: Option<PathBuf>,
    pub allow_launch: bool,
    pub allow_stop: bool,
    /// Off by default. The agent captures only the selected game window, never
    /// the desktop, and only when the owner enables it here and on the backend.
    #[serde(default)]
    pub allow_screenshot: bool,
    /// Local telemetry bridge for the SpiderBridge Minecraft mod. This is a
    /// loopback-only, authenticated channel: it never talks to Discord, and the
    /// remote backend still has its own gates. On by default so an installed mod
    /// is usable; set it to false to disable the listener entirely.
    #[serde(default = "default_true")]
    pub allow_bridge: bool,
    /// Read-only chat history. Off by default: the mod is not told to collect
    /// anything until the owner enables this here.
    #[serde(default)]
    pub allow_chat: bool,
    /// Send chat lines through the selected game client.
    #[serde(default)]
    pub allow_chat_send: bool,
    /// Submit policy-checked Minecraft commands through the selected client.
    #[serde(default)]
    pub allow_minecraft_command: bool,
    /// Which command categories the owner permits. All false by default.
    #[serde(default)]
    pub command_policy: crate::command_policy::CommandPolicy,
    #[serde(default = "default_chat_retention")]
    pub chat_retention_seconds: u32,
    #[serde(default = "default_chat_messages")]
    pub chat_max_messages: u32,
    pub poll_seconds: u64,
    pub capabilities: Capabilities,
}

impl AgentConfig {
    pub fn validate(&self) -> Result<(), AgentError> {
        validate_server_url(&self.server_url)?;
        if self.device_id.trim().is_empty() || self.device_token.is_empty() {
            return Err(AgentError::Config(
                "the agent is not paired: device id or token is empty".into(),
            ));
        }
        if self.device_token.expose().len() < 32 {
            return Err(AgentError::Config(
                "the stored device token is malformed".into(),
            ));
        }
        if !(2..=300).contains(&self.poll_seconds) {
            return Err(AgentError::Config(
                "poll_seconds must be between 2 and 300".into(),
            ));
        }
        if !(crate::bridge::MIN_CHAT_RETENTION_SECONDS
            ..=crate::bridge::MAX_CHAT_RETENTION_SECONDS)
            .contains(&self.chat_retention_seconds)
        {
            return Err(AgentError::Config(
                "chat_retention_seconds must be between 30 and 3600".into(),
            ));
        }
        if !(crate::bridge::MIN_CHAT_MESSAGES..=crate::bridge::MAX_CHAT_MESSAGES)
            .contains(&self.chat_max_messages)
        {
            return Err(AgentError::Config(
                "chat_max_messages must be between 20 and 500".into(),
            ));
        }
        Ok(())
    }

    pub fn policy_capabilities(&self) -> Capabilities {
        Capabilities {
            launch: self.allow_launch && self.launcher_path.is_some(),
            stop: self.allow_stop,
            screenshot: self.allow_screenshot,
            coordinates: self.allow_bridge,
            chat: self.allow_bridge && self.allow_chat,
            chat_send: self.allow_bridge && self.allow_chat_send,
            minecraft_command: self.allow_bridge && self.allow_minecraft_command,
            ..Capabilities::default()
        }
    }

    pub fn bridge_policy(&self) -> crate::bridge::BridgePolicy {
        crate::bridge::BridgePolicy {
            // Collection is only requested when the owner enabled both the
            // listener and chat history. The backward-compatible coordinates
            // path stays available on its own.
            chat_collection: self.allow_bridge && self.allow_chat,
            chat_retention_seconds: self.chat_retention_seconds,
            chat_max_messages: self.chat_max_messages,
            allow_chat_send: self.allow_bridge && self.allow_chat_send,
            allow_minecraft_command: self.allow_bridge && self.allow_minecraft_command,
        }
        .bounded()
    }
}

fn default_chat_retention() -> u32 {
    crate::bridge::DEFAULT_CHAT_RETENTION_SECONDS
}

fn default_chat_messages() -> u32 {
    crate::bridge::DEFAULT_CHAT_MAX_MESSAGES
}

fn default_true() -> bool {
    true
}

pub fn config_dir() -> Result<PathBuf, AgentError> {
    ProjectDirs::from("org", "SpiderClient", "SpiderClient")
        .map(|dirs| dirs.config_dir().to_path_buf())
        .ok_or_else(|| AgentError::Config("the operating system has no config directory".into()))
}

pub fn data_dir() -> Result<PathBuf, AgentError> {
    ProjectDirs::from("org", "SpiderClient", "SpiderClient")
        .map(|dirs| dirs.data_local_dir().to_path_buf())
        .ok_or_else(|| AgentError::Config("the operating system has no data directory".into()))
}

pub fn config_path() -> Result<PathBuf, AgentError> {
    Ok(config_dir()?.join(CONFIG_FILE))
}

pub fn status_path() -> Result<PathBuf, AgentError> {
    Ok(data_dir()?.join(STATUS_FILE))
}

/// A local, user-owned request for the running agent to exit cleanly. The file
/// contains no credential; the launcher writes it and the agent removes it.
pub fn stop_path() -> Result<PathBuf, AgentError> {
    Ok(data_dir()?.join(STOP_FILE))
}

pub fn load(path: &Path) -> Result<AgentConfig, AgentError> {
    let text = fs::read_to_string(path).map_err(|err| {
        AgentError::Config(format!(
            "could not read agent config at {}: {err}",
            path.display()
        ))
    })?;
    let config: AgentConfig = serde_json::from_str(&text)?;
    if config.version != CONFIG_VERSION {
        return Err(AgentError::Config(format!(
            "unsupported agent config version {}",
            config.version
        )));
    }
    config.validate()?;
    Ok(config)
}

pub fn save(config: &AgentConfig) -> Result<PathBuf, AgentError> {
    config.validate()?;
    let dir = config_dir()?;
    fs::create_dir_all(&dir)?;
    save_at(config, &dir.join(CONFIG_FILE))
}

/// Write through the protected serializer to an explicit path. Used by `save`
/// and by tests that verify the plaintext-to-protected migration.
fn save_at(config: &AgentConfig, path: &Path) -> Result<PathBuf, AgentError> {
    config.validate()?;
    let dir = path
        .parent()
        .ok_or_else(|| AgentError::Config("the agent config path has no parent".into()))?;
    fs::create_dir_all(dir)?;
    let temp = path.with_extension("json.tmp");
    let json = serde_json::to_vec_pretty(config)?;

    {
        let mut options = fs::OpenOptions::new();
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

    fs::rename(&temp, path)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }

    Ok(path.to_path_buf())
}

pub fn validate_server_url(raw: &str) -> Result<(), AgentError> {
    let url = Url::parse(raw)
        .map_err(|_| AgentError::Config("the remote service URL is not a valid URL".into()))?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(AgentError::Config(
            "the remote service URL must not contain credentials, a query, or a fragment".into(),
        ));
    }
    match url.scheme() {
        "https" => Ok(()),
        "http" if is_loopback(url.host_str()) => Ok(()),
        "http" => Err(AgentError::Config(
            "the remote service must use HTTPS (plain HTTP is allowed only for loopback development)"
                .into(),
        )),
        _ => Err(AgentError::Config(
            "the remote service URL must use HTTPS".into(),
        )),
    }
}

fn is_loopback(host: Option<&str>) -> bool {
    matches!(host, Some("127.0.0.1" | "::1" | "localhost"))
}

pub fn new_pairing(
    server_url: String,
    device_name: String,
    launcher_path: Option<PathBuf>,
) -> Result<AgentConfig, AgentError> {
    validate_server_url(&server_url)?;
    Ok(AgentConfig {
        version: CONFIG_VERSION,
        server_url,
        device_id: String::new(),
        device_token: DeviceToken::empty(),
        device_name,
        launcher_path,
        allow_launch: false,
        allow_stop: false,
        allow_screenshot: false,
        allow_bridge: true,
        allow_chat: false,
        allow_chat_send: false,
        allow_minecraft_command: false,
        command_policy: crate::command_policy::CommandPolicy::default(),
        chat_retention_seconds: default_chat_retention(),
        chat_max_messages: default_chat_messages(),
        poll_seconds: 5,
        capabilities: Capabilities::default(),
    })
}

pub fn protocol() -> u32 {
    PROTOCOL_VERSION
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_plaintext_remote_transport() {
        assert!(validate_server_url("http://example.com").is_err());
        assert!(validate_server_url("https://example.com").is_ok());
        assert!(validate_server_url("http://127.0.0.1:8787").is_ok());
        assert!(validate_server_url("https://user:pass@example.com").is_err());
        assert!(validate_server_url("https://example.com/?token=secret").is_err());
    }

    #[test]
    fn token_is_protected_at_rest_and_round_trips() {
        let token = "a".repeat(43);
        let config = AgentConfig {
            version: CONFIG_VERSION,
            server_url: "https://example.com".into(),
            device_id: "device".into(),
            device_token: DeviceToken::new(token.clone()).unwrap(),
            device_name: "PC".into(),
            launcher_path: None,
            allow_launch: false,
            allow_stop: false,
            allow_screenshot: false,
            allow_bridge: true,
            allow_chat: false,
            allow_chat_send: false,
            allow_minecraft_command: false,
            command_policy: crate::command_policy::CommandPolicy::default(),
            chat_retention_seconds: default_chat_retention(),
            chat_max_messages: default_chat_messages(),
            poll_seconds: 5,
            capabilities: Capabilities::default(),
        };
        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains("device_token"));
        #[cfg(windows)]
        assert!(!json.contains(&token), "the raw token must not be written");
        #[cfg(not(windows))]
        assert!(json.contains("plain:v1:"));
        let restored: AgentConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.device_token.expose(), token);
    }

    #[test]
    fn legacy_plaintext_token_still_loads() {
        let stored = format!(
            r#"{{"version":1,"server_url":"https://example.com","device_id":"device","device_token":"{}","device_name":"PC","launcher_path":null,"allow_launch":false,"allow_stop":false,"poll_seconds":5,"capabilities":{{"logs":true,"process_list":true,"launch":false,"stop":false,"screenshot":false,"coordinates":false,"chat":false,"baritone_control":false,"performance":true}}}}"#,
            "b".repeat(43)
        );
        let config: AgentConfig = serde_json::from_str(&stored).unwrap();
        assert_eq!(config.device_token.expose(), "b".repeat(43));
        assert!(!config.allow_screenshot);
        assert!(!config.policy_capabilities().screenshot);
    }

    #[test]
    fn loading_then_saving_upgrades_a_legacy_plaintext_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_FILE);
        let token = "d".repeat(43);
        let legacy = serde_json::json!({
            "version": 1,
            "server_url": "https://example.com",
            "device_id": "device",
            "device_token": token.clone(),
            "device_name": "PC",
            "launcher_path": null,
            "allow_launch": false,
            "allow_stop": false,
            "poll_seconds": 5,
            "capabilities": serde_json::to_value(Capabilities::default()).unwrap(),
        });
        fs::write(&path, serde_json::to_vec_pretty(&legacy).unwrap()).unwrap();

        let config = load(&path).unwrap();
        assert_eq!(config.device_token.expose(), token);
        save_at(&config, &path).unwrap();

        let rewritten = fs::read_to_string(&path).unwrap();
        #[cfg(windows)]
        assert!(
            !rewritten.contains(&token),
            "the plaintext token must not survive a rewrite"
        );
        #[cfg(not(windows))]
        assert!(rewritten.contains("plain:v1:"));

        let restored = load(&path).unwrap();
        assert_eq!(restored.device_token.expose(), token);
        assert!(!path.with_extension("json.tmp").exists());
    }
}
