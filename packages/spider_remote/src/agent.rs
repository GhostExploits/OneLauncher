use std::collections::{HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use reqwest::StatusCode;
use serde::Serialize;
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, Signal, System, UpdateKind};
use tokio::time::sleep;

use crate::config::{self, AgentConfig};
use crate::protocol::{
    AgentHeartbeat, ApiError, BridgeSnapshot, ChatHistory, ChatLogsPayload, CommandKind,
    CommandRequest, CommandResult, DeviceSnapshot, ExecutionStatus, HeartbeatResponse,
    LaunchPayload, LogsPayload, MinecraftCommandPayload, PROTOCOL_VERSION, PairClaim, PairResponse,
    PositionSnapshot, SayPayload, ScreenshotAttachment, SessionSnapshot, StopPayload, SystemSpecs,
    clamp_log_lines, validate_device_name,
};
use crate::redact::{redact, tail_text};
use crate::screenshot;
use crate::{AGENT_VERSION, AgentError};

const STATUS_PORT: u16 = 45_817;
const COMMAND_POLL_SECONDS: u64 = 5;
const MAX_RESULT_BATCH: usize = 8;
const MAX_SEEN_COMMANDS: usize = 256;

pub async fn pair(
    server_url: String,
    code: String,
    device_name: String,
    launcher_path: Option<PathBuf>,
) -> Result<PathBuf, AgentError> {
    if code.trim().is_empty() {
        return Err(AgentError::Config("pairing code cannot be empty".into()));
    }
    validate_device_name(&device_name)?;
    let mut config = config::new_pairing(server_url, device_name, launcher_path)?;
    let client = http_client()?;
    let endpoint = endpoint(&config.server_url, "/v1/pair/claim");
    let response = client
        .post(endpoint)
        .json(&PairClaim {
            protocol: PROTOCOL_VERSION,
            pairing_code: code,
            device_name: config.device_name.clone(),
            platform: std::env::consts::OS.to_string(),
            agent_version: AGENT_VERSION.to_string(),
            capabilities: config.policy_capabilities(),
        })
        .send()
        .await?;

    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() {
        let detail = serde_json::from_str::<ApiError>(&body)
            .map(|err| err.message)
            .unwrap_or_else(|_| format!("pairing failed with HTTP {}", status.as_u16()));
        return Err(AgentError::Pairing(detail));
    }

    let pair: PairResponse = serde_json::from_str(&body)?;
    if pair.protocol != PROTOCOL_VERSION {
        return Err(AgentError::Pairing(format!(
            "server speaks protocol {}, this agent speaks {PROTOCOL_VERSION}",
            pair.protocol
        )));
    }
    config.device_id = pair.device_id;
    config.device_token = crate::config::DeviceToken::new(pair.device_token)?;
    config.save_path()
}

impl AgentConfig {
    fn save_path(&self) -> Result<PathBuf, AgentError> {
        config::save(self)
    }
}

#[derive(Serialize)]
struct StatusFile {
    version: u32,
    agent_version: String,
    pid: u32,
    device_id: String,
    device_name: String,
    server_url: String,
    online: bool,
    paired: bool,
    last_heartbeat: Option<DateTime<Utc>>,
    last_error: Option<String>,
    session_count: usize,
    /// True when the loopback SpiderBridge listener is running.
    bridge_enabled: bool,
    /// Path the local mod reads. It contains the bridge credential, so the
    /// status file never repeats that credential here.
    bridge_endpoint: Option<String>,
    /// How many Minecraft processes currently have a mod connected.
    bridge_sessions: usize,
    updated_at: DateTime<Utc>,
}

struct LocalSession {
    snapshot: SessionSnapshot,
    log_path: Option<PathBuf>,
    game_dir: PathBuf,
}

/// Run until the process is stopped. The fixed loopback port is a cheap
/// cross-platform single-instance guard: a second agent cannot bind it and
/// therefore exits instead of registering the device twice.
pub async fn run(config: AgentConfig) -> Result<(), AgentError> {
    config.validate()?;
    let _guard = std::net::TcpListener::bind(("127.0.0.1", STATUS_PORT)).map_err(|_| {
        AgentError::AlreadyRunning(format!(
            "another SpiderClient Agent is already using the local status port {STATUS_PORT}"
        ))
    })?;

    let client = http_client()?;
    let mut sys = System::new();
    sys.refresh_memory();
    sys.refresh_cpu_all();
    let status_path = config::status_path()?;
    if let Some(parent) = status_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let stop_path = config::stop_path()?;
    // A stop marker left over from a previous run must not stop this one.
    let _ = fs::remove_file(&stop_path);
    let agent_started_at = Utc::now();
    let mut seen = HashSet::<String>::new();
    let mut seen_order = VecDeque::<String>::new();
    let mut pending_results = Vec::<CommandResult>::new();
    let mut backoff = 2u64;

    // The mod -> agent channel. Failure to start it must not prevent remote
    // monitoring from working, so it is reported and skipped.
    let bridge = if config.allow_bridge {
        match crate::bridge::BridgeServer::start_with_policy(
            AGENT_VERSION,
            config.bridge_policy(),
        )
        .await
        {
            Ok(server) => {
                tracing::info!(
                    endpoint = %server.endpoint_path().display(),
                    "SpiderBridge listener started"
                );
                Some(server)
            }
            Err(err) => {
                tracing::warn!(error = %err, "SpiderBridge listener could not start");
                None
            }
        }
    } else {
        None
    };

    loop {
        if stop_path.exists() {
            let _ = fs::remove_file(&stop_path);
            let _ = write_status(
                &status_path,
                &config,
                false,
                None,
                Some("agent stopped by the local user".into()),
                0,
                &BridgeStatus::default(),
            );
            tracing::info!("clean shutdown requested by the local launcher");
            return Ok(());
        }

        // `refresh_processes` does not fetch command lines. Detection needs the
        // JVM arguments, so request cmd explicitly.
        sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing()
                .with_memory()
                .with_cpu()
                .with_cmd(UpdateKind::OnlyIfNotSet),
        );
        sys.refresh_memory();
        sys.refresh_cpu_usage();

        let mut sessions = collect_sessions(&mut sys, &config);
        if let Some(bridge) = bridge.as_ref() {
            let bindings: Vec<(u32, String, PathBuf)> = sessions
                .iter()
                .map(|session| {
                    (
                        session.snapshot.pid,
                        session.snapshot.id.clone(),
                        session.game_dir.clone(),
                    )
                })
                .collect();
            bridge.apply_sessions(&bindings);
            for session in &mut sessions {
                attach_bridge(session, bridge, &config);
            }
        }
        let bridge_status = bridge.as_ref().map_or_else(BridgeStatus::default, |bridge| {
            BridgeStatus {
                enabled: true,
                endpoint: Some(bridge.endpoint_path().display().to_string()),
                sessions: bridge.connected_count(),
            }
        });

        let snapshot = DeviceSnapshot {
            protocol: PROTOCOL_VERSION,
            agent_version: AGENT_VERSION.to_string(),
            platform: std::env::consts::OS.to_string(),
            specs: collect_specs(&sys),
            sessions: sessions.iter().map(|s| s.snapshot.clone()).collect(),
            capabilities: config.policy_capabilities(),
            captured_at: Utc::now(),
            agent_started_at,
            log_excerpt_supported: true,
        };

        let heartbeat = AgentHeartbeat {
            protocol: PROTOCOL_VERSION,
            snapshot: snapshot.clone(),
            // Keep results until the backend has definitely accepted them.
            // A transport failure must not silently lose a completed command.
            results: pending_results.clone(),
        };

        match send_heartbeat(&client, &config, &heartbeat).await {
            Ok(response) => {
                backoff = 2;
                pending_results.clear();
                for command in response.commands {
                    if seen.contains(&command.id) {
                        continue;
                    }
                    if seen_order.len() >= MAX_SEEN_COMMANDS
                        && let Some(old) = seen_order.pop_front()
                    {
                        seen.remove(&old);
                    }
                    seen.insert(command.id.clone());
                    seen_order.push_back(command.id.clone());

                    if pending_results.len() >= MAX_RESULT_BATCH {
                        break;
                    }
                    let result =
                        execute_command(&mut sys, &config, &sessions, &command, bridge.as_ref())
                            .await;
                    pending_results.push(result);
                }
                write_status(
                    &status_path,
                    &config,
                    true,
                    Some(Utc::now()),
                    None,
                    snapshot.sessions.len(),
                    &bridge_status,
                )?;
            }
            Err(err) => {
                let detail = sanitize_error(&err.to_string());
                write_status(
                    &status_path,
                    &config,
                    false,
                    None,
                    Some(detail.clone()),
                    snapshot.sessions.len(),
                    &bridge_status,
                )?;
                if matches!(err, AgentError::Auth(_)) {
                    tracing::error!(error = %detail, "agent credential was rejected; stopping");
                    return Err(AgentError::Auth(detail));
                }
                tracing::warn!(error = %detail, "agent heartbeat failed");
                if wait_or_stop(backoff, &stop_path).await {
                    let _ = fs::remove_file(&stop_path);
                    return Ok(());
                }
                backoff = (backoff * 2).min(60);
                continue;
            }
        }

        let poll_seconds = config.poll_seconds.max(COMMAND_POLL_SECONDS);
        if wait_or_stop(poll_seconds, &stop_path).await {
            let _ = fs::remove_file(&stop_path);
            let _ = write_status(
                &status_path,
                &config,
                false,
                None,
                Some("agent stopped by the local user".into()),
                snapshot.sessions.len(),
                &bridge_status,
            );
            tracing::info!("clean shutdown requested by the local launcher");
            return Ok(());
        }
    }
}

/// Sleep in one-second slices so a local stop request is observed promptly even
/// when the configured poll interval is several minutes.
async fn wait_or_stop(seconds: u64, stop_path: &Path) -> bool {
    for _ in 0..seconds.max(1) {
        if stop_path.exists() {
            return true;
        }
        sleep(Duration::from_secs(1)).await;
    }
    stop_path.exists()
}

fn http_client() -> Result<reqwest::Client, AgentError> {
    reqwest::Client::builder()
        .user_agent(format!("SpiderClient-Agent/{AGENT_VERSION}"))
        .timeout(Duration::from_secs(25))
        .https_only(false)
        .build()
        .map_err(AgentError::Http)
}

fn endpoint(base: &str, path: &str) -> String {
    format!("{}{}", base.trim_end_matches('/'), path)
}

async fn send_heartbeat(
    client: &reqwest::Client,
    config: &AgentConfig,
    heartbeat: &AgentHeartbeat,
) -> Result<HeartbeatResponse, AgentError> {
    let response = client
        .post(endpoint(&config.server_url, "/v1/agent/heartbeat"))
        .bearer_auth(config.device_token.expose())
        .json(heartbeat)
        .send()
        .await?;
    let status = response.status();
    let body = response.text().await?;

    if status == StatusCode::UNAUTHORIZED {
        return Err(AgentError::Auth(
            "the backend rejected this device token; re-pair the agent".into(),
        ));
    }
    if !status.is_success() {
        return Err(AgentError::HttpStatus {
            status: status.as_u16(),
            message: parse_api_error(&body),
        });
    }
    let response: HeartbeatResponse = serde_json::from_str(&body)?;
    if response.protocol != PROTOCOL_VERSION {
        return Err(AgentError::Protocol(format!(
            "backend protocol {} does not match agent protocol {PROTOCOL_VERSION}",
            response.protocol
        )));
    }
    Ok(response)
}

fn parse_api_error(body: &str) -> String {
    serde_json::from_str::<ApiError>(body)
        .map(|err| err.message)
        .unwrap_or_else(|_| "the backend returned an error".into())
}

fn sanitize_error(value: &str) -> String {
    let value = redact(value);
    value.chars().take(300).collect()
}

#[derive(Default, Clone)]
struct BridgeStatus {
    enabled: bool,
    endpoint: Option<String>,
    sessions: usize,
}

fn write_status(
    path: &Path,
    config: &AgentConfig,
    online: bool,
    last_heartbeat: Option<DateTime<Utc>>,
    last_error: Option<String>,
    session_count: usize,
    bridge: &BridgeStatus,
) -> Result<(), AgentError> {
    let status = StatusFile {
        version: 1,
        agent_version: AGENT_VERSION.to_string(),
        pid: std::process::id(),
        device_id: config.device_id.clone(),
        device_name: config.device_name.clone(),
        server_url: config.server_url.clone(),
        online,
        paired: !config.device_id.is_empty(),
        last_heartbeat,
        last_error,
        session_count,
        bridge_enabled: bridge.enabled,
        bridge_endpoint: bridge.endpoint.clone(),
        bridge_sessions: bridge.sessions,
        updated_at: Utc::now(),
    };
    let temp = path.with_extension("json.tmp");
    fs::write(&temp, serde_json::to_vec_pretty(&status)?)?;
    fs::rename(temp, path)?;
    Ok(())
}

fn collect_specs(sys: &System) -> SystemSpecs {
    SystemSpecs {
        cpu: sys.cpus().first().map(|cpu| cpu.brand().trim().to_string()),
        physical_cores: System::physical_core_count(),
        logical_cores: Some(sys.cpus().len()),
        total_memory_bytes: Some(sys.total_memory()),
        available_memory_bytes: Some(sys.available_memory()),
        os: System::long_os_version().or_else(System::name),
        hostname: System::host_name(),
        gpu: None,
    }
}

fn collect_sessions(sys: &mut System, config: &AgentConfig) -> Vec<LocalSession> {
    let mut sessions = Vec::new();
    let mut seen = HashSet::<String>::new();
    let capabilities = config.policy_capabilities();

    for (pid, process) in sys.processes() {
        let args: Vec<String> = process
            .cmd()
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        let Some(parsed) = parse_minecraft_args(&args) else {
            continue;
        };
        let start = process.start_time();
        let started_at = Utc
            .timestamp_opt(start as i64, 0)
            .single()
            .unwrap_or_else(Utc::now);
        let session_id = session_id(config, *pid, start, &parsed.game_dir);
        // A stale `sysinfo` refresh can repeat a process. Never expose the same
        // session twice, otherwise `/select` and log routing become ambiguous.
        if !seen.insert(session_id.clone()) {
            continue;
        }
        let profile = parsed
            .game_dir
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.is_empty())
            .or_else(|| parsed.version.clone())
            .unwrap_or_else(|| "Minecraft".into());
        let loader = detect_loader(&profile, parsed.version.as_deref());
        let mods = mods_dir(&parsed.game_dir);
        let mods_count = mods.as_deref().and_then(count_mods);
        let baritone_detected = mods.as_deref().is_some_and(has_baritone);
        // These four need a connected SpiderBridge mod. A process that merely
        // has the mod installed is not enough, so they are granted per session
        // in `attach_bridge` instead of inherited from the device policy.
        let mut session_capabilities = capabilities.clone();
        session_capabilities.coordinates = false;
        session_capabilities.chat = false;
        session_capabilities.chat_send = false;
        session_capabilities.minecraft_command = false;

        sessions.push(LocalSession {
            snapshot: SessionSnapshot {
                id: session_id,
                profile,
                username: parsed.username,
                mc_version: parsed.version,
                loader,
                pid: pid.as_u32(),
                started_at,
                uptime_seconds: Utc::now()
                    .signed_duration_since(started_at)
                    .num_seconds()
                    .max(0) as u64,
                memory_bytes: Some(process.memory()),
                cpu_percent: Some(process.cpu_usage()),
                mods_count,
                baritone_detected,
                bridge: None,
                state: "running".into(),
                capabilities: session_capabilities,
            },
            log_path: log_for_session(&parsed.game_dir, started_at),
            game_dir: parsed.game_dir,
        });

        if sessions.len() >= crate::protocol::MAX_SESSIONS {
            break;
        }
    }

    sessions.sort_by_key(|session| session.snapshot.started_at);
    // Two clients that share a game directory both write `logs/latest.log`.
    // Reading it for one of them would silently return the other's lines, so
    // those sessions are reported without a log source. Canonicalising avoids
    // treating `C:/game` and `C:\game` as different folders.
    let mut folder_counts = std::collections::HashMap::<PathBuf, usize>::new();
    for session in &sessions {
        *folder_counts
            .entry(folder_identity(&session.game_dir))
            .or_default() += 1;
    }
    for session in &mut sessions {
        if folder_counts
            .get(&folder_identity(&session.game_dir))
            .copied()
            .unwrap_or(0)
            > 1
        {
            session.log_path = None;
        }
    }
    sessions
}

/// Copy the mod's latest sample into the heartbeat for this session.
///
/// A connected bridge is what makes `/coords` possible, so the capability is
/// granted per session while the mod is attached. When a sample exists it is
/// published with its true age: the backend must be able to show "stale"
/// instead of pretending old data is live.
fn attach_bridge(
    session: &mut LocalSession,
    bridge: &crate::bridge::BridgeServer,
    config: &AgentConfig,
) {
    if !bridge.is_connected(&session.snapshot.id) {
        return;
    }
    let policy = config.policy_capabilities();
    session.snapshot.capabilities.coordinates = policy.coordinates;
    session.snapshot.capabilities.chat = policy.chat;
    session.snapshot.capabilities.chat_send = policy.chat_send;
    session.snapshot.capabilities.minecraft_command = policy.minecraft_command;
    let Some(sample) = bridge.snapshot(&session.snapshot.id) else {
        return;
    };
    let age = bridge.age_seconds(&session.snapshot.id);
    let fresh = age.is_some_and(|seconds| seconds <= crate::bridge::STALE_AFTER_SECONDS)
        && sample.is_fresh(Utc::now());
    session.snapshot.bridge = Some(BridgeSnapshot {
        state: match sample.state {
            crate::bridge::GameState::InWorld => "in_world",
            crate::bridge::GameState::Menu => "menu",
            crate::bridge::GameState::NoPlayer => "no_player",
        }
        .into(),
        mc_version: sample.game.mc_version.clone(),
        loader: sample.game.loader.clone(),
        bridge_version: sample.game.bridge_version.clone(),
        dimension: sample.world.as_ref().map(|world| world.dimension.clone()),
        position: sample.position.as_ref().map(|position| PositionSnapshot {
            x: position.x,
            y: position.y,
            z: position.z,
            yaw: position.yaw,
            pitch: position.pitch,
        }),
        player_name: sample.player.as_ref().map(|player| player.name.clone()),
        health: sample.player.as_ref().and_then(|player| player.health),
        food_level: sample.player.as_ref().and_then(|player| player.food_level),
        fps: sample.performance.as_ref().and_then(|perf| perf.fps),
        age_seconds: age,
        fresh,
        baritone_detected: session.snapshot.baritone_detected || sample.baritone.detected,
        baritone_integration: sample.baritone.integration.clone(),
    });
}

/// Format a live coordinate answer. Returns an explicit reason instead of a
/// fabricated position when the player is not actually in a world.
fn coordinates_message(
    session: &LocalSession,
    sample: &crate::bridge::BridgeTelemetry,
) -> Result<String, String> {
    let profile = session
        .snapshot
        .profile
        .replace(['`', '*', '_', '|', '~'], "");
    let short_id = &session.snapshot.id[..8.min(session.snapshot.id.len())];
    let Some(position) = sample.position.as_ref() else {
        return Err(match sample.state {
            crate::bridge::GameState::Menu => format!(
                "Minecraft is at the menu for **{profile}**, so the player is not in a world and there are no coordinates."
            ),
            crate::bridge::GameState::NoPlayer => format!(
                "the world for **{profile}** is still loading, so no position is available yet."
            ),
            crate::bridge::GameState::InWorld => format!(
                "the mod for **{profile}** reports a loaded world but sent no position; nothing was assumed."
            ),
        });
    };
    let dimension = sample
        .world
        .as_ref()
        .map_or("unknown", |world| world.dimension.as_str());
    let age = Utc::now()
        .signed_duration_since(sample.captured_at)
        .num_seconds()
        .max(0);
    Ok(format!(
        "**{profile}** · session `{short_id}`\nX {:.2} · Y {:.2} · Z {:.2}\ndimension `{dimension}`\nyaw {:.1}° · pitch {:.1}° · sampled {age}s ago\nMinecraft {} · {} · bridge {}",
        position.x,
        position.y,
        position.z,
        position.yaw,
        position.pitch,
        sample.game.mc_version,
        sample.game.loader,
        sample.game.bridge_version,
    ))
}

fn folder_identity(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Resolve `latest.log` only when it plausibly belongs to this process. A log
/// left behind by an older run must not be presented as this session's output.
fn log_for_session(game_dir: &Path, started_at: DateTime<Utc>) -> Option<PathBuf> {
    let path = game_dir.join("logs").join("latest.log");
    let metadata = fs::metadata(&path).ok()?;
    if !metadata.is_file() {
        return None;
    }
    let modified = metadata.modified().ok()?;
    let modified = DateTime::<Utc>::from(modified);
    // Allow for clock granularity and log buffers.
    if modified + chrono::Duration::seconds(60) < started_at {
        return None;
    }
    Some(path)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MinecraftArgs {
    game_dir: PathBuf,
    version: Option<String>,
    username: Option<String>,
}

fn parse_minecraft_args(args: &[String]) -> Option<MinecraftArgs> {
    if !looks_like_minecraft(args) {
        return None;
    }
    Some(MinecraftArgs {
        game_dir: PathBuf::from(arg_value(args, "--gameDir")?),
        version: arg_value(args, "--version"),
        username: arg_value(args, "--username"),
    })
}

fn looks_like_minecraft(args: &[String]) -> bool {
    let has_game_dir = args
        .iter()
        .any(|arg| arg == "--gameDir" || arg.starts_with("--gameDir="));
    let has_main = args.iter().any(|arg| {
        arg.ends_with("net.minecraft.client.main.Main")
            || arg.contains("net.minecraft.launchwrapper.Launch")
            || arg.contains("net.fabricmc.loader.impl.launch.knot.KnotClient")
            || arg.contains("org.quiltmc.loader.impl.launch.knot.KnotClient")
            || arg.ends_with("cpw.mods.bootstraplauncher.BootstrapLauncher")
    });
    // Modern Forge/NeoForge use a bootstrap launcher plus FML arguments. Keep
    // the requirement for --gameDir so unrelated Java tools are not sessions.
    let has_fml = args.iter().any(|arg| arg == "--launchTarget")
        && args
            .iter()
            .any(|arg| arg == "--fml.forgeVersion" || arg == "--fml.neoForgeVersion");
    has_game_dir && (has_main || has_fml)
}

fn arg_value(args: &[String], name: &str) -> Option<String> {
    let prefix = format!("{name}=");
    for (index, arg) in args.iter().enumerate() {
        if arg == name {
            return args.get(index + 1).cloned();
        }
        if let Some(value) = arg.strip_prefix(&prefix) {
            return Some(value.to_string());
        }
    }
    None
}

fn detect_loader(profile: &str, version: Option<&str>) -> Option<String> {
    // The --version argument is the authoritative loader identity for modded
    // launches. The folder name is only a fallback; it may be a shared
    // .minecraft folder or an unrelated custom name.
    for value in [version.unwrap_or_default(), profile] {
        let value = value.to_ascii_lowercase();
        if value.contains("neoforge") {
            return Some("NeoForge".into());
        }
        if value.contains("fabric") {
            return Some("Fabric".into());
        }
        if value.contains("quilt") {
            return Some("Quilt".into());
        }
        if value.contains("forge") {
            return Some("Forge".into());
        }
    }
    Some("Vanilla".into())
}

fn session_id(config: &AgentConfig, pid: Pid, start: u64, game_dir: &Path) -> String {
    let material = format!(
        "{}:{}:{}:{}",
        config.device_id,
        pid.as_u32(),
        start,
        game_dir.to_string_lossy()
    );
    let digest = ring::digest::digest(&ring::digest::SHA256, material.as_bytes());
    hex::encode(&digest.as_ref()[..16])
}

fn mods_dir(game_dir: &Path) -> Option<PathBuf> {
    let dir = game_dir.join("mods");
    dir.is_dir().then_some(dir)
}

fn count_mods(dir: &Path) -> Option<usize> {
    let entries = fs::read_dir(dir).ok()?;
    Some(
        entries
            .flatten()
            .filter(|entry| {
                entry
                    .path()
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("jar"))
            })
            .count(),
    )
}

fn has_baritone(dir: &Path) -> bool {
    fs::read_dir(dir).is_ok_and(|entries| {
        entries.flatten().any(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .to_ascii_lowercase()
                .contains("baritone")
        })
    })
}

async fn execute_command(
    sys: &mut System,
    config: &AgentConfig,
    sessions: &[LocalSession],
    command: &CommandRequest,
    bridge: Option<&crate::bridge::BridgeServer>,
) -> CommandResult {
    let completed_at = Utc::now();
    if command.is_expired(completed_at) {
        return result(
            command,
            false,
            "the request expired before it could run",
            None,
        );
    }

    let allowed = config.policy_capabilities();
    if !allowed.supports(&command.kind) {
        return result(
            command,
            false,
            "this device has not enabled that capability",
            None,
        );
    }

    match command.kind {
        CommandKind::GetLogs => {
            let Some(session) = select_session(sessions, command.session_id.as_deref()) else {
                return result(
                    command,
                    false,
                    "the selected session is no longer running",
                    None,
                );
            };
            if !session_is_current(sys, session) {
                return result(
                    command,
                    false,
                    "the selected process exited; no log from another session was read",
                    None,
                );
            }
            let Some(path) = &session.log_path else {
                return result(
                    command,
                    false,
                    "this session has no attributable latest.log (missing, stale, or its game directory is shared with another running instance)",
                    None,
                );
            };
            let lines = serde_json::from_value::<LogsPayload>(command.payload.clone())
                .map(|payload| clamp_log_lines(payload.lines))
                .unwrap_or(120);
            match read_log(path, lines) {
                Ok(excerpt) => result(command, true, "log excerpt retrieved", Some(excerpt)),
                Err(err) => result(
                    command,
                    false,
                    &format!("could not read the session log: {err}"),
                    None,
                ),
            }
        }
        CommandKind::Launch => {
            let payload = match serde_json::from_value::<LaunchPayload>(command.payload.clone()) {
                Ok(payload) => payload,
                Err(_) => return result(command, false, "invalid launch payload", None),
            };
            if !payload.confirm {
                return result(command, false, "launch requires confirmation", None);
            }
            let Some(launcher) = config.launcher_path.as_ref() else {
                return result(command, false, "no launcher executable is configured", None);
            };
            if !valid_installation(&payload.installation) {
                return result(command, false, "invalid installation name", None);
            }
            if !launcher.is_file() {
                return result(command, false, "the configured launcher is missing", None);
            }
            match std::process::Command::new(launcher)
                .arg("--launch")
                .arg(&payload.installation)
                .spawn()
            {
                Ok(_) => result(
                    command,
                    true,
                    &format!("launch requested for {}", payload.installation),
                    None,
                ),
                Err(err) => result(
                    command,
                    false,
                    &format!("could not start the launcher: {err}"),
                    None,
                ),
            }
        }
        CommandKind::Stop => {
            let payload = match serde_json::from_value::<StopPayload>(command.payload.clone()) {
                Ok(payload) => payload,
                Err(_) => return result(command, false, "invalid stop payload", None),
            };
            if !payload.confirm {
                return result(command, false, "stopping requires confirmation", None);
            }
            let Some(session) = sessions
                .iter()
                .find(|session| session.snapshot.id == payload.session_id)
            else {
                return result(
                    command,
                    false,
                    "the selected session is no longer running",
                    None,
                );
            };
            if stop_process(sys, session.snapshot.pid, session.snapshot.started_at) {
                result(
                    command,
                    true,
                    "a stop request was sent to the game process",
                    None,
                )
            } else {
                result(
                    command,
                    false,
                    "the game process could not be stopped",
                    None,
                )
            }
        }
        CommandKind::GetMods => {
            let Some(session) = select_session(sessions, command.session_id.as_deref()) else {
                return result(
                    command,
                    false,
                    "the selected session is no longer running",
                    None,
                );
            };
            if !session_is_current(sys, session) {
                return result(
                    command,
                    false,
                    "the selected process exited; no mod list from another session was read",
                    None,
                );
            }
            let names = list_mods(&session.game_dir);
            result(
                command,
                true,
                &format!(
                    "{} mod file(s) detected{}",
                    names.len(),
                    if names
                        .iter()
                        .any(|name| name.to_ascii_lowercase().contains("baritone"))
                    {
                        "; Baritone is installed (in-game control is not available)"
                    } else {
                        ""
                    }
                ),
                Some(names.join("\n")),
            )
        }
        CommandKind::Screenshot => {
            let Some(session) = select_session(sessions, command.session_id.as_deref()) else {
                return result(
                    command,
                    false,
                    "the selected session is no longer running; no image was captured",
                    None,
                );
            };
            if !session_is_current(sys, session) {
                return result(
                    command,
                    false,
                    "the selected process exited; no image from another process was captured",
                    None,
                );
            }
            match screenshot::capture_game_window(session.snapshot.pid) {
                Ok(image) => {
                    use base64::Engine as _;
                    let attachment = ScreenshotAttachment {
                        content_type: image.content_type.to_string(),
                        filename: format!(
                            "minecraft-{}.{}",
                            &session.snapshot.id[..8.min(session.snapshot.id.len())],
                            if image.content_type == "image/png" {
                                "png"
                            } else {
                                "jpg"
                            }
                        ),
                        width: image.width,
                        height: image.height,
                        data_base64: base64::engine::general_purpose::STANDARD.encode(&image.bytes),
                    };
                    result_with_screenshot(
                        command,
                        true,
                        &format!(
                            "captured session `{}` at {} ({}x{}, {} KiB)",
                            session.snapshot.id,
                            Utc::now().to_rfc3339(),
                            image.width,
                            image.height,
                            image.bytes.len() / 1024
                        ),
                        attachment,
                    )
                }
                Err(err) => result(
                    command,
                    false,
                    &format!("screenshot failed: {err}; no image was captured"),
                    None,
                ),
            }
        }
        CommandKind::Coordinates => {
            let Some(session) = select_session(sessions, command.session_id.as_deref()) else {
                return result(
                    command,
                    false,
                    "the selected session is no longer running",
                    None,
                );
            };
            if !session_is_current(sys, session) {
                return result(
                    command,
                    false,
                    "the selected process exited; no coordinates from another session were read",
                    None,
                );
            }
            let Some(bridge) = bridge else {
                return result(
                    command,
                    false,
                    "the SpiderBridge listener is disabled in this agent's config",
                    None,
                );
            };
            if !bridge.is_connected(&session.snapshot.id) {
                return result(
                    command,
                    false,
                    &format!(
                        "the SpiderBridge mod is not connected for session **{}**. Install it in this instance and restart the game.",
                        session.snapshot.profile
                    ),
                    None,
                );
            }
            match bridge.request_status(&session.snapshot.id).await {
                Ok(sample) => match coordinates_message(session, &sample) {
                    Ok(message) => result(command, true, &message, None),
                    Err(reason) => result(command, false, &reason, None),
                },
                Err(err) => result(command, false, &err, None),
            }
        }
        CommandKind::ChatLogs => {
            let Some(session) = select_session(sessions, command.session_id.as_deref()) else {
                return result(
                    command,
                    false,
                    "the selected session is no longer running; no chat was read",
                    None,
                );
            };
            if !session_is_current(sys, session) {
                return result(
                    command,
                    false,
                    "the selected process exited; no chat from another session was read",
                    None,
                );
            }
            let Some(bridge) = bridge else {
                return result(
                    command,
                    false,
                    "the SpiderBridge listener is disabled in this agent's config",
                    None,
                );
            };
            if !bridge.is_connected(&session.snapshot.id) {
                return result(
                    command,
                    false,
                    &format!(
                        "the SpiderBridge mod is not connected for session `{}`; no chat was read",
                        session.snapshot.id
                    ),
                    None,
                );
            }
            let payload = serde_json::from_value::<ChatLogsPayload>(command.payload.clone())
                .map(|payload| payload.bounded())
                .unwrap_or_else(|_| {
                    ChatLogsPayload {
                        limit: 50,
                        include_system: true,
                        include_action_bar: false,
                    }
                    .bounded()
                });
            match bridge.request_chat(&session.snapshot.id, payload.clone()).await {
                Ok(mut history) => {
                    history.messages.retain(|message| match message.kind.as_str() {
                        "chat" => true,
                        "system" => payload.include_system,
                        "action_bar" => payload.include_action_bar,
                        _ => false,
                    });
                    if history.messages.len() > payload.limit {
                        let keep = history.messages.len() - payload.limit;
                        history.messages.drain(0..keep);
                    }
                    let message = format!(
                        "captured {} message(s) from the game's in-memory chat buffer{}",
                        history.messages.len(),
                        if history.dropped > 0 {
                            format!("; the buffer discarded {} older message(s)", history.dropped)
                        } else {
                            String::new()
                        }
                    );
                    result_with_chat(command, true, &message, history)
                }
                Err(err) => result(command, false, &err, None),
            }
        }
        CommandKind::Say => {
            let payload = match serde_json::from_value::<SayPayload>(command.payload.clone()) {
                Ok(payload) => payload,
                Err(_) => return result(command, false, "invalid chat payload", None),
            };
            let message = match crate::command_policy::validate_chat_message(&payload.message) {
                Ok(message) => message,
                Err(reason) => return result(command, false, &reason, None),
            };
            let Some(session) = select_session(sessions, command.session_id.as_deref()) else {
                return result(
                    command,
                    false,
                    "the selected session is no longer running; no chat was sent",
                    None,
                );
            };
            if !session_is_current(sys, session) {
                return result(
                    command,
                    false,
                    "the selected process exited; no chat was sent",
                    None,
                );
            }
            let Some(bridge) = bridge else {
                return result(
                    command,
                    false,
                    "the SpiderBridge listener is disabled in this agent's config; no chat was sent",
                    None,
                );
            };
            if !bridge.is_connected(&session.snapshot.id) {
                return result(
                    command,
                    false,
                    &format!(
                        "the SpiderBridge mod is not connected for session `{}`; no chat was sent",
                        session.snapshot.id
                    ),
                    None,
                );
            }
            match bridge
                .request_chat_send(&session.snapshot.id, &message)
                .await
            {
                Ok(action) if action.uncertain => result_with_execution(
                    command,
                    false,
                    "the game started processing the chat line but did not confirm it; it may have been sent",
                    ExecutionStatus::unknown(action.detail.unwrap_or_else(|| {
                        "the client did not confirm whether it submitted the chat line".into()
                    })),
                ),
                Ok(action) if action.accepted => result_with_execution(
                    command,
                    true,
                    "the message was submitted by the Minecraft client; delivery to other players or the server cannot be verified",
                    ExecutionStatus::submitted_unverified(
                        action
                            .detail
                            .unwrap_or_else(|| "ClientPacketListener.sendChat accepted the line".into()),
                    ),
                ),
                Ok(action) => result_with_execution(
                    command,
                    false,
                    &format!(
                        "the game client did not submit the message: {}",
                        action.detail.as_deref().unwrap_or("no detail was provided")
                    ),
                    ExecutionStatus::rejected(
                        action
                            .detail
                            .unwrap_or_else(|| "the game client rejected the chat line".into()),
                    ),
                ),
                Err(err) => result_with_execution(
                    command,
                    false,
                    &err,
                    ExecutionStatus::rejected(err.clone()),
                ),
            }
        }
        CommandKind::MinecraftCommand => {
            let payload =
                match serde_json::from_value::<MinecraftCommandPayload>(command.payload.clone()) {
                    Ok(payload) => payload,
                    Err(_) => return result(command, false, "invalid command payload", None),
                };
            if !payload.confirm {
                return result(
                    command,
                    false,
                    "the command was not confirmed; nothing was submitted",
                    None,
                );
            }
            let classified = match crate::command_policy::validate_command(
                &payload.command,
                &config.command_policy,
            ) {
                Ok(classified) => classified,
                Err(reason) => return result(command, false, &reason, None),
            };
            let Some(session) = select_session(sessions, command.session_id.as_deref()) else {
                return result(
                    command,
                    false,
                    "the selected session is no longer running; no command was submitted",
                    None,
                );
            };
            if !session_is_current(sys, session) {
                return result(
                    command,
                    false,
                    "the selected process exited; no command was submitted",
                    None,
                );
            }
            let Some(bridge) = bridge else {
                return result(
                    command,
                    false,
                    "the SpiderBridge listener is disabled in this agent's config; no command was submitted",
                    None,
                );
            };
            if !bridge.is_connected(&session.snapshot.id) {
                return result(
                    command,
                    false,
                    &format!(
                        "the SpiderBridge mod is not connected for session `{}`; no command was submitted",
                        session.snapshot.id
                    ),
                    None,
                );
            }
            match bridge
                .request_command(&session.snapshot.id, &classified.command)
                .await
            {
                Ok(action) if action.uncertain => result_with_execution(
                    command,
                    false,
                    &format!(
                        "the game started processing the `{}` command but did not confirm it; it may have been submitted",
                        classified.command
                    ),
                    ExecutionStatus::unknown(action.detail.unwrap_or_else(|| {
                        "the client did not confirm whether it submitted the command".into()
                    })),
                ),
                Ok(action) if action.accepted => result_with_execution(
                    command,
                    true,
                    &format!(
                        "the `{}` command was submitted by the Minecraft client; the server-side result could not be verified",
                        classified.command
                    ),
                    ExecutionStatus::submitted_unverified(
                        action.detail.unwrap_or_else(|| {
                            "the client accepted the command; vanilla chat does not correlate command output".into()
                        }),
                    ),
                ),
                Ok(action) => result_with_execution(
                    command,
                    false,
                    &format!(
                        "the game client did not submit the command: {}",
                        action.detail.as_deref().unwrap_or("no detail was provided")
                    ),
                    ExecutionStatus::rejected(
                        action
                            .detail
                            .unwrap_or_else(|| "the game client rejected the command".into()),
                    ),
                ),
                Err(err) => result_with_execution(
                    command,
                    false,
                    &err,
                    ExecutionStatus::rejected(err.clone()),
                ),
            }
        }
        CommandKind::BaritoneStatus | CommandKind::BaritoneControl => result(
            command,
            false,
            "Baritone control requires a separate in-game integration, which is not installed",
            None,
        ),
        CommandKind::Pause | CommandKind::Resume => result(
            command,
            false,
            "pause/resume requires an in-game integration; nothing was changed",
            None,
        ),
        CommandKind::GetStatus => result(command, true, "the session list is current", None),
    }
}

fn result(
    command: &CommandRequest,
    success: bool,
    message: &str,
    log_excerpt: Option<String>,
) -> CommandResult {
    CommandResult::text(
        command.id.clone(),
        success,
        redact(message),
        log_excerpt.map(|text| redact(&text)),
    )
}

fn result_with_screenshot(
    command: &CommandRequest,
    success: bool,
    message: &str,
    screenshot: ScreenshotAttachment,
) -> CommandResult {
    CommandResult {
        screenshot: Some(screenshot),
        ..CommandResult::text(command.id.clone(), success, redact(message), None)
    }
}

fn result_with_chat(
    command: &CommandRequest,
    success: bool,
    message: &str,
    chat: ChatHistory,
) -> CommandResult {
    CommandResult {
        chat: Some(chat),
        ..CommandResult::text(command.id.clone(), success, redact(message), None)
    }
}

fn result_with_execution(
    command: &CommandRequest,
    success: bool,
    message: &str,
    mut execution: ExecutionStatus,
) -> CommandResult {
    execution.detail = execution.detail.map(|detail| redact(&detail));
    CommandResult {
        execution: Some(execution),
        ..CommandResult::text(command.id.clone(), success, redact(message), None)
    }
}

/// Verify that the PID still belongs to the process that produced this
/// session. A recycled PID must never receive a command for the old game.
fn session_is_current(sys: &mut System, session: &LocalSession) -> bool {
    let pid = Pid::from_u32(session.snapshot.pid);
    sys.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
    sys.process(pid).is_some_and(|process| {
        process.start_time() as i64 == session.snapshot.started_at.timestamp()
    })
}

fn select_session<'a>(
    sessions: &'a [LocalSession],
    requested: Option<&str>,
) -> Option<&'a LocalSession> {
    match requested {
        Some(id) => sessions.iter().find(|session| session.snapshot.id == id),
        None if sessions.len() == 1 => sessions.first(),
        None => None,
    }
}

fn valid_installation(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty()
        && value.chars().count() <= 96
        && value
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.' | '(' | ')'))
}

fn read_log(path: &Path, lines: usize) -> Result<String, std::io::Error> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = fs::File::open(path)?;
    let len = file.metadata()?.len();
    let start = len.saturating_sub(96 * 1024);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    file.take(96 * 1024).read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    Ok(tail_text(&text, clamp_log_lines(lines)))
}

fn stop_process(sys: &mut System, pid: u32, started_at: DateTime<Utc>) -> bool {
    let pid = Pid::from_u32(pid);
    sys.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
    let Some(process) = sys.process(pid) else {
        return false;
    };
    // A recycled PID with a different start time is not the session the
    // operator selected.
    if (process.start_time() as i64) != started_at.timestamp() {
        return false;
    }
    process
        .kill_with(Signal::Term)
        .unwrap_or_else(|| process.kill())
}

fn list_mods(game_dir: &Path) -> Vec<String> {
    let Some(dir) = mods_dir(game_dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("jar"))
        })
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .take(100)
        .collect();
    names.sort();
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Capabilities;

    fn args(line: &str) -> Vec<String> {
        line.split(' ').map(str::to_string).collect()
    }

    #[test]
    fn detects_a_real_minecraft_command_line() {
        let args = args(
            "java -cp minecraft.jar net.minecraft.client.main.Main --version 1.21.11 --gameDir C:/games/SpiderClient/.minecraft --username Steve",
        );
        assert!(looks_like_minecraft(&args));
        assert_eq!(arg_value(&args, "--version").as_deref(), Some("1.21.11"));
        assert_eq!(
            arg_value(&args, "--gameDir").as_deref(),
            Some("C:/games/SpiderClient/.minecraft")
        );
        assert_eq!(arg_value(&args, "--username").as_deref(), Some("Steve"));
    }

    #[test]
    fn ignores_unrelated_java_and_tools() {
        assert!(!looks_like_minecraft(&args(
            "java -jar server.jar --gameDir /srv"
        )));
        assert!(!looks_like_minecraft(&args("java -jar build-tool.jar")));
    }

    #[test]
    fn detects_modern_forge_and_neoforge_bootstrap() {
        let forge = args(
            "java cpw.mods.bootstraplauncher.BootstrapLauncher --launchTarget forgeclient --fml.forgeVersion 54.0.0 --gameDir C:/games/forge --username Steve",
        );
        assert!(looks_like_minecraft(&forge));
        let neo = args(
            "java cpw.mods.bootstraplauncher.BootstrapLauncher --launchTarget neoforgeclient --fml.neoForgeVersion 21.1.0 --gameDir C:/games/neo --username Steve",
        );
        assert!(looks_like_minecraft(&neo));
    }

    #[test]
    fn detects_quilt_and_fabric_knot_clients() {
        let fabric = args(
            "java net.fabricmc.loader.impl.launch.knot.KnotClient --gameDir C:/games/fabric --version 1.21.11",
        );
        let quilt = args(
            "java org.quiltmc.loader.impl.launch.knot.KnotClient --gameDir C:/games/quilt --version 1.21.11",
        );
        assert!(looks_like_minecraft(&fabric));
        assert!(looks_like_minecraft(&quilt));
    }

    #[test]
    fn session_ids_change_with_process_identity() {
        let config = AgentConfig {
            version: 1,
            server_url: "https://example.com".into(),
            device_id: "device-a".into(),
            device_token: crate::config::DeviceToken::new("x".repeat(32)).unwrap(),
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
            chat_retention_seconds: crate::bridge::DEFAULT_CHAT_RETENTION_SECONDS,
            chat_max_messages: crate::bridge::DEFAULT_CHAT_MAX_MESSAGES,
            poll_seconds: 5,
            capabilities: Capabilities::default(),
        };
        let one = session_id(
            &config,
            Pid::from_u32(100),
            1_700_000_000,
            Path::new("C:/a"),
        );
        let two = session_id(
            &config,
            Pid::from_u32(101),
            1_700_000_000,
            Path::new("C:/a"),
        );
        assert_ne!(one, two);
        assert_eq!(one.len(), 32);
    }

    #[test]
    fn two_instances_in_the_same_folder_parse_as_distinct_sessions() {
        let first = parse_minecraft_args(&args(
            "java net.fabricmc.loader.impl.launch.knot.KnotClient --gameDir C:/minecraft --version fabric-loader-0.16.9-1.21.11 --username Alex",
        ))
        .unwrap();
        let second = parse_minecraft_args(&args(
            "java net.fabricmc.loader.impl.launch.knot.KnotClient --gameDir C:/minecraft --version fabric-loader-0.16.9-1.21.11 --username Alex",
        ))
        .unwrap();
        assert_eq!(first, second);
        // The launcher passes the same folder and name for both; identity comes
        // from the process, not from these fields.
        let config = AgentConfig {
            version: 1,
            server_url: "https://example.com".into(),
            device_id: "device-a".into(),
            device_token: crate::config::DeviceToken::new("x".repeat(32)).unwrap(),
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
            chat_retention_seconds: crate::bridge::DEFAULT_CHAT_RETENTION_SECONDS,
            chat_max_messages: crate::bridge::DEFAULT_CHAT_MAX_MESSAGES,
            poll_seconds: 5,
            capabilities: Capabilities::default(),
        };
        let one = session_id(&config, Pid::from_u32(700), 1_700_000_000, &first.game_dir);
        let two = session_id(&config, Pid::from_u32(701), 1_700_000_000, &second.game_dir);
        assert_ne!(one, two);
    }

    #[test]
    fn an_unknown_username_is_reported_as_unknown_not_invented() {
        let parsed = parse_minecraft_args(&args(
            "java net.minecraft.client.main.Main --gameDir C:/minecraft --version 1.21.11",
        ))
        .unwrap();
        assert!(parsed.username.is_none());
        assert_eq!(parsed.version.as_deref(), Some("1.21.11"));
    }

    #[test]
    fn a_command_line_without_a_game_folder_is_not_a_session() {
        assert!(parse_minecraft_args(&args("java -jar tool.jar --username Alex")).is_none());
    }

    #[test]
    fn equals_style_game_dir_is_recognised() {
        let parsed = parse_minecraft_args(&args(
            "java net.minecraft.client.main.Main --gameDir=C:/minecraft --version=1.21.11",
        ))
        .unwrap();
        assert_eq!(parsed.game_dir, PathBuf::from("C:/minecraft"));
        assert_eq!(parsed.version.as_deref(), Some("1.21.11"));
    }

    #[test]
    fn a_log_from_a_previous_run_is_not_attributed_to_a_new_session() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("logs")).unwrap();
        let log = dir.path().join("logs").join("latest.log");
        std::fs::write(&log, "old run\n").unwrap();
        // Pretend the file was last written two hours before the current run.
        let started =
            DateTime::<Utc>::from(std::time::SystemTime::now()) + chrono::Duration::hours(2);
        assert_eq!(log_for_session(dir.path(), started), None);
        assert_eq!(
            log_for_session(dir.path(), Utc::now()),
            Some(log),
            "a current log must still be used"
        );
        assert_eq!(
            log_for_session(&dir.path().join("missing"), Utc::now()),
            None
        );
    }

    #[test]
    fn log_tail_handles_invalid_utf8_missing_files_and_truncation() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("latest.log");
        assert!(read_log(&log, 10).is_err());

        // Invalid UTF-8 must be replaced, not treated as a fatal read error.
        let mut bytes = b"[12:00:00] before\n[12:00:01] ".to_vec();
        bytes.extend_from_slice(&[0xff, 0xfe, b'\n']);
        std::fs::write(&log, bytes).unwrap();
        let excerpt = read_log(&log, 10).unwrap();
        assert!(excerpt.contains("before"));
        assert!(excerpt.contains('\u{fffd}'));

        // A large log is read from the bounded tail, newest lines last.
        let mut large = String::new();
        for index in 0..5_000 {
            large.push_str(&format!("line {index}\n"));
        }
        std::fs::write(&log, large).unwrap();
        let excerpt = read_log(&log, 5).unwrap();
        assert!(excerpt.contains("line 4999"));
        assert!(!excerpt.contains("line 0\n"));
        assert!(excerpt.len() <= crate::protocol::MAX_LOG_BYTES + 4);
    }

    #[test]
    fn session_selection_requires_an_explicit_id_when_several_are_running() {
        let session = |id: &str| LocalSession {
            snapshot: SessionSnapshot {
                id: id.into(),
                profile: "Minecraft".into(),
                username: None,
                mc_version: None,
                loader: None,
                pid: 10,
                started_at: Utc::now(),
                uptime_seconds: 0,
                memory_bytes: None,
                cpu_percent: None,
                mods_count: None,
                baritone_detected: false,
                bridge: None,
                state: "running".into(),
                capabilities: Capabilities::default(),
            },
            log_path: None,
            game_dir: PathBuf::new(),
        };
        let sessions = vec![session("a"), session("b")];
        assert!(select_session(&sessions, None).is_none());
        assert_eq!(
            select_session(&sessions, Some("b")).map(|s| s.snapshot.id.as_str()),
            Some("b")
        );
        assert_eq!(
            select_session(&sessions[..1], None).map(|s| s.snapshot.id.as_str()),
            Some("a")
        );
        assert!(select_session(&sessions, Some("missing")).is_none());
    }

    #[test]
    fn installation_names_cannot_escape_into_a_shell() {
        assert!(valid_installation("Survival 1.21"));
        assert!(!valid_installation("../../Windows/System32"));
        assert!(!valid_installation("name; rm -rf /"));
        assert!(!valid_installation(""));
    }

    fn coordinate_session(profile: &str, id: &str) -> LocalSession {
        LocalSession {
            snapshot: SessionSnapshot {
                id: id.into(),
                profile: profile.into(),
                username: Some("Steve".into()),
                mc_version: Some("1.21.1".into()),
                loader: Some("Fabric".into()),
                pid: 10,
                started_at: Utc::now(),
                uptime_seconds: 60,
                memory_bytes: None,
                cpu_percent: None,
                mods_count: None,
                baritone_detected: false,
                bridge: None,
                state: "running".into(),
                capabilities: Capabilities::default(),
            },
            log_path: None,
            game_dir: PathBuf::new(),
        }
    }

    fn bridge_sample(state: crate::bridge::GameState, with_position: bool) -> crate::bridge::BridgeTelemetry {
        crate::bridge::BridgeTelemetry {
            schema: crate::bridge::ENDPOINT_SCHEMA,
            game: crate::bridge::BridgeGame {
                mc_version: "1.21.1".into(),
                loader: "fabric".into(),
                loader_version: "0.20.1".into(),
                bridge_version: "0.1.0".into(),
            },
            state,
            player: Some(crate::bridge::BridgePlayer {
                name: "Steve".into(),
                health: Some(20.0),
                food_level: Some(20),
            }),
            world: with_position.then(|| crate::bridge::BridgeWorld {
                dimension: "minecraft:the_nether".into(),
            }),
            position: with_position.then(|| crate::bridge::BridgePosition {
                x: 12.5,
                y: 64.0,
                z: -30.25,
                yaw: 90.0,
                pitch: -12.5,
            }),
            performance: None,
            baritone: crate::bridge::BridgeBaritone {
                detected: false,
                integration: "unavailable".into(),
            },
            captured_at: Utc::now() - chrono::Duration::seconds(2),
            uptime_seconds: 60,
        }
    }

    #[test]
    fn coordinates_message_identifies_the_instance_and_live_values() {
        let session = coordinate_session("Survival", "abcdef123456");
        let message = coordinates_message(
            &session,
            &bridge_sample(crate::bridge::GameState::InWorld, true),
        )
        .unwrap();
        assert!(message.contains("Survival"));
        assert!(message.contains("abcdef12"), "the short session id is shown");
        assert!(message.contains("X 12.50"));
        assert!(message.contains("Y 64.00"));
        assert!(message.contains("Z -30.25"));
        assert!(message.contains("minecraft:the_nether"));
        assert!(message.contains("1.21.1"));
        assert!(message.contains("sampled 2s ago"));
    }

    #[test]
    fn coordinates_message_never_invents_a_position() {
        let session = coordinate_session("Survival", "session-a");
        let menu = coordinates_message(
            &session,
            &bridge_sample(crate::bridge::GameState::Menu, false),
        )
        .unwrap_err();
        assert!(menu.contains("not in a world"));
        assert!(menu.contains("no coordinates"));

        let loading = coordinates_message(
            &session,
            &bridge_sample(crate::bridge::GameState::NoPlayer, false),
        )
        .unwrap_err();
        assert!(loading.contains("still loading"));

        let inconsistent = coordinates_message(
            &session,
            &bridge_sample(crate::bridge::GameState::InWorld, false),
        )
        .unwrap_err();
        assert!(inconsistent.contains("nothing was assumed"));
    }

    #[test]
    fn coordinates_message_removes_markdown_from_a_profile_name() {
        let session = coordinate_session("Survival **bold** | test", "abcdef123456");
        let message = coordinates_message(
            &session,
            &bridge_sample(crate::bridge::GameState::InWorld, true),
        )
        .unwrap();
        // The name is sanitized; the only bold markers are the formatter's own.
        assert!(message.contains("**Survival bold  test**"));
        assert!(!message.contains("bold**"));
        assert!(!message.contains(" | "));
        assert!(message.contains("Survival bold  test"));
    }

    #[test]
    fn loader_is_detected_from_the_profile() {
        assert_eq!(
            detect_loader("fabric-1.21", None).as_deref(),
            Some("Fabric")
        );
        assert_eq!(
            detect_loader("neoforge-1.21", None).as_deref(),
            Some("NeoForge")
        );
        assert_eq!(
            detect_loader("1.21", Some("1.21")).as_deref(),
            Some("Vanilla")
        );
    }

    #[tokio::test]
    async fn a_stop_marker_interrupts_a_long_poll_wait() {
        let dir = tempfile::tempdir().unwrap();
        let stop = dir.path().join("agent.stop");
        let writer = stop.clone();
        let task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            std::fs::write(writer, b"stop").unwrap();
        });

        let started = std::time::Instant::now();
        assert!(wait_or_stop(120, &stop).await);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the stop marker must not wait for the poll interval"
        );
        task.await.unwrap();
    }

    #[tokio::test]
    async fn an_existing_stop_marker_returns_immediately() {
        let dir = tempfile::tempdir().unwrap();
        let stop = dir.path().join("agent.stop");
        std::fs::write(&stop, b"stop").unwrap();

        let started = std::time::Instant::now();
        assert!(wait_or_stop(120, &stop).await);
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
