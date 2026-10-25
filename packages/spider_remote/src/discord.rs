//! Discord slash-command routing and embeds.
//!
//! Every interaction is signature-verified by the HTTP layer before this module
//! sees it. Here we authorize the Discord user against the device owner, keep
//! monitoring responses ephemeral, and enqueue only explicitly confirmed
//! actions.

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::backend::{BackendConfig, BackendState};
use crate::protocol::{
    Capabilities, ChatLogsPayload, CommandKind, LaunchPayload, LogsPayload,
    MinecraftCommandPayload, SayPayload, StopPayload, SystemSpecs, truncate_text,
};
use crate::redact::redact;
use crate::store::DeviceRecord;

const EMBED_COLOR: u32 = 0x56_73_92;
const EPHEMERAL: u64 = 64;

#[derive(Debug, Deserialize)]
struct Interaction {
    data: Option<CommandData>,
    member: Option<Member>,
    user: Option<User>,
    token: Option<String>,
    channel_id: Option<String>,
    guild_id: Option<String>,
    application_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Member {
    user: User,
}

#[derive(Debug, Deserialize)]
struct User {
    id: String,
}

#[derive(Debug, Deserialize)]
struct CommandData {
    name: String,
    #[serde(default)]
    options: Vec<CommandOption>,
}

#[derive(Debug, Deserialize)]
struct CommandOption {
    name: String,
    value: Option<Value>,
}

struct Context {
    user_id: String,
    channel_id: String,
    guild_id: Option<String>,
    interaction_token: Option<String>,
    application_id: String,
}

struct Selected {
    device: DeviceRecord,
    session: Option<crate::protocol::SessionSnapshot>,
}

struct Resolved {
    device: DeviceRecord,
    session: Option<crate::protocol::SessionSnapshot>,
}

pub async fn handle_interaction(state: &BackendState, value: Value) -> Option<Value> {
    let kind = value.get("type")?.as_u64()?;
    if kind == 1 {
        return Some(json!({"type": 1}));
    }
    if kind != 2 {
        return Some(ephemeral("Unsupported interaction type."));
    }

    let interaction: Interaction = serde_json::from_value(value).ok()?;
    let data = interaction.data?;
    let user = interaction
        .member
        .as_ref()
        .map(|member| &member.user)
        .or(interaction.user.as_ref())?;
    let channel_id = interaction
        .channel_id
        .clone()
        .unwrap_or_else(|| format!("dm:{}", user.id));
    let context = Context {
        user_id: user.id.clone(),
        channel_id,
        guild_id: interaction.guild_id.clone(),
        interaction_token: interaction.token.clone(),
        application_id: interaction
            .application_id
            .clone()
            .unwrap_or_else(|| state.config.application_id.clone()),
    };

    if !state.config.guild_allowed(context.guild_id.as_deref()) {
        return Some(ephemeral(
            "SpiderClient commands are not enabled in this server.",
        ));
    }

    if data.name == "help" {
        return Some(help_response());
    }

    let devices = match authorized_devices(state, &context.user_id).await {
        Ok(devices) => devices,
        Err(message) => return Some(ephemeral(&message)),
    };
    if devices.is_empty() && !state.config.is_admin(&context.user_id) {
        return Some(ephemeral(
            "No SpiderClient device is paired to this Discord account. Ask the device owner to pair it first.",
        ));
    }

    Some(match data.name.as_str() {
        "devices" => devices_page(state, &devices, option_i64(&data, "page")),
        "sessions" => sessions_page(state, &context, &devices, option_i64(&data, "page")).await,
        "select" => select_device(state, &context, &devices, &data).await,
        "agent" => agent_view(state, &context).await,
        "status" => status_view(state, &context, &devices, option_str(&data, "session")).await,
        "logs" => logs_command(state, &context, &data).await,
        "launch" => launch_command(state, &context, &data).await,
        "stop" => stop_command(state, &context, &data).await,
        "mods" => mods_command(state, &context).await,
        "settings" => settings_view(state, &context).await,
        "specs" => specs_view(state, &context).await,
        "performance" => performance_view(state, &context).await,
        "uptime" => uptime_view(state, &context).await,
        "alerts" => alerts_view(state, &context).await,
        "restart-agent" => unsupported(
            "Remote agent restart is not exposed. Use the Remote screen in SpiderClient to stop and start the agent, or run `spider-agent run` again after stopping it.",
        ),
        "screenshot" | "screen" => screenshot_command(state, &context, &data).await,
        "coords" => coords_command(state, &context, &data).await,
        "chat" | "chatlogs" => chat_command(state, &context, &data).await,
        "say" => say_command(state, &context, &data).await,
        "mc-command" => minecraft_command(state, &context, &data).await,
        "command-logs" => command_logs_command(state, &context, &data).await,
        "baritone-status" => baritone_status(state, &context).await,
        "baritone-control" => unsupported(
            "No documented Baritone control bridge is installed. No control command was sent.",
        ),
        "pause" | "resume" => unsupported(
            "Pause and resume require an in-game integration. The agent does not suspend the game process.",
        ),
        "pair" => pair_command(state, &context, &data).await,
        "unpair" => unpair_command(state, &context, &data).await,
        _ => ephemeral("Unknown command. Try `/help`."),
    })
}

async fn authorized_devices(
    state: &BackendState,
    user_id: &str,
) -> Result<Vec<DeviceRecord>, String> {
    let result = if state.config.is_admin(user_id) {
        state.store.list_all_devices().await
    } else {
        state.store.list_devices(user_id).await
    };
    result.map_err(|err| format!("Could not read the device registry: {err}"))
}

async fn resolve_device(state: &BackendState, context: &Context) -> Result<Resolved, String> {
    let devices = authorized_devices(state, &context.user_id).await?;
    if devices.is_empty() {
        return Err("No SpiderClient device is paired to this Discord account.".into());
    }
    let selection = state
        .store
        .channel_selection(&context.user_id, &context.channel_id)
        .await
        .map_err(|err| format!("Could not read the current selection: {err}"))?;
    let selected_id = selection.as_ref().and_then(|(device, _)| device.clone());
    let selected_session = selection.as_ref().and_then(|(_, session)| session.clone());

    let device = match selected_id {
        Some(id) => devices
            .iter()
            .find(|device| device.id == id)
            .cloned()
            .ok_or_else(|| {
                "The selected device is no longer registered. Run `/devices`.".to_string()
            })?,
        None if devices.len() == 1 => devices[0].clone(),
        None => {
            return Err(
                "Multiple devices are paired. Run `/select device:<id>` or `/devices` first."
                    .into(),
            );
        }
    };

    let session = match selected_session {
        Some(id) => {
            let session = device
                .sessions()
                .iter()
                .find(|session| session.id == id)
                .cloned()
                .ok_or_else(|| {
                    format!(
                        "The selected session `{id}` is no longer running. Use `/sessions` and `/select` again."
                    )
                })?;
            Some(session)
        }
        None if device.sessions().len() == 1 => device.sessions().first().cloned(),
        None => None,
    };

    Ok(Resolved { device, session })
}

async fn resolve_session(
    state: &BackendState,
    context: &Context,
    explicit: Option<&str>,
) -> Result<Selected, String> {
    let resolved = resolve_device(state, context).await?;
    let session = if let Some(id) = explicit {
        Some(
            resolved
                .device
                .sessions()
                .iter()
                .find(|session| session.id == id)
                .cloned()
                .ok_or_else(|| {
                    format!(
                        "Session `{id}` is not running on {}. Use `/sessions`.",
                        resolved.device.name
                    )
                })?,
        )
    } else {
        resolved.session
    };
    if session.is_none() {
        return Err(format!(
            "{} has {} running session(s). Select one with `/select session:<id>` (see `/sessions`).",
            resolved.device.name,
            resolved.device.sessions().len()
        ));
    }
    Ok(Selected {
        device: resolved.device,
        session,
    })
}

async fn select_device(
    state: &BackendState,
    context: &Context,
    devices: &[DeviceRecord],
    data: &CommandData,
) -> Value {
    let Some(device_id) = option_str(data, "device") else {
        return ephemeral("A device id is required. Use `/devices` to find it.");
    };
    let Some(device) = devices.iter().find(|device| device.id == device_id) else {
        return ephemeral("That device is not authorized for this Discord account.");
    };
    let session_id = option_str(data, "session");
    if let Some(id) = &session_id
        && !device.sessions().iter().any(|session| &session.id == id)
    {
        return ephemeral(&format!(
            "Session `{id}` is not currently running on {}. Use `/sessions`.",
            device.name
        ));
    }
    if let Err(err) = state
        .store
        .set_selection(
            &context.user_id,
            &context.channel_id,
            Some(&device.id),
            session_id.as_deref(),
        )
        .await
    {
        return ephemeral(&format!("Could not save the selection: {err}"));
    }
    let session_text = session_id
        .as_deref()
        .map(|id| format!("\nSession: `{id}`"))
        .unwrap_or_else(|| "\nSession: automatic when exactly one is running".into());
    embed_response(
        "Selection saved",
        &format!(
            "Device: **{}** (`{}`)\nOnline: {}{}",
            device.name,
            device.id,
            yes_no(device.online),
            session_text
        ),
        Vec::new(),
    )
}

fn devices_page(_state: &BackendState, devices: &[DeviceRecord], page: Option<i64>) -> Value {
    if devices.is_empty() {
        return embed_response("Devices", "No devices are paired.", Vec::new());
    }
    let per_page = 8usize;
    let page = page.unwrap_or(0).max(0) as usize;
    let total_pages = devices.len().div_ceil(per_page);
    let page = page.min(total_pages.saturating_sub(1));
    let mut fields = Vec::new();
    for device in devices.iter().skip(page * per_page).take(per_page) {
        let last = device
            .last_seen
            .map(|time| relative_time(time, Utc::now()))
            .unwrap_or_else(|| "never".into());
        fields.push(field(
            &device.name,
            &format!(
                "`{}`\n{} · {} session(s)\nAgent: {} · Last seen: {}",
                device.id,
                if device.online { "online" } else { "offline" },
                device.sessions().len(),
                device.agent_version.as_deref().unwrap_or("unknown"),
                last
            ),
            false,
        ));
    }
    embed_response(
        "Paired devices",
        &format!(
            "{} device(s). Page {}/{}.\nDevices can only be controlled by their paired owner.",
            devices.len(),
            page + 1,
            total_pages.max(1)
        ),
        fields,
    )
}

async fn sessions_page(
    state: &BackendState,
    context: &Context,
    devices: &[DeviceRecord],
    page: Option<i64>,
) -> Value {
    let selection = state
        .store
        .channel_selection(&context.user_id, &context.channel_id)
        .await
        .ok()
        .flatten();
    let selected_session = selection.as_ref().and_then(|(_, session)| session.clone());
    let rows: Vec<(&DeviceRecord, &crate::protocol::SessionSnapshot)> = devices
        .iter()
        .flat_map(|device| {
            device
                .sessions()
                .iter()
                .map(move |session| (device, session))
        })
        .collect();
    if rows.is_empty() {
        return embed_response(
            "Sessions",
            "No running Minecraft processes were reported. Launching a game with SpiderClient will make it appear here.",
            Vec::new(),
        );
    }
    let per_page = 8usize;
    let page = page.unwrap_or(0).max(0) as usize;
    let total_pages = rows.len().div_ceil(per_page);
    let page = page.min(total_pages.saturating_sub(1));
    let mut fields = Vec::new();
    for (device, session) in rows.iter().skip(page * per_page).take(per_page) {
        let marker = if selected_session.as_deref() == Some(session.id.as_str()) {
            " · selected"
        } else {
            ""
        };
        fields.push(field(
            &format!(
                "{} · {} · {}",
                device.name,
                session
                    .username
                    .as_deref()
                    .unwrap_or(session.profile.as_str()),
                if device.online { "online" } else { "offline" }
            ),
            &format!(
                "`{}{}`\n{} · {} · PID {}\nUp {} · {} mod(s){}",
                session.id,
                marker,
                session.mc_version.as_deref().unwrap_or("version unknown"),
                session.loader.as_deref().unwrap_or("loader unknown"),
                session.pid,
                format_uptime(session.uptime_seconds),
                session.mods_count.unwrap_or(0),
                if session.baritone_detected {
                    " · Baritone detected"
                } else {
                    ""
                }
            ),
            false,
        ));
    }
    embed_response(
        "Running sessions",
        &format!(
            "{} session(s). Page {}/{}.\nA username is not an identifier; select the session ID.",
            rows.len(),
            page + 1,
            total_pages.max(1)
        ),
        fields,
    )
}

async fn agent_view(state: &BackendState, context: &Context) -> Value {
    let resolved = match resolve_device(state, context).await {
        Ok(resolved) => resolved,
        Err(message) => return ephemeral(&message),
    };
    let device = resolved.device;
    let started = device
        .snapshot
        .as_ref()
        .map(|snapshot| {
            format_uptime(
                (Utc::now() - snapshot.agent_started_at)
                    .num_seconds()
                    .max(0) as u64,
            )
        })
        .unwrap_or_else(|| "unknown".into());
    embed_response(
        "SpiderClient Agent",
        &format!(
            "**{}**\nStatus: {}\nDevice: `{}`\nAgent: {} on {}\nStarted: {} ago\nSessions: {}",
            device.name,
            if device.online { "online" } else { "offline" },
            device.id,
            device.agent_version.as_deref().unwrap_or("unknown"),
            device.platform.as_deref().unwrap_or("unknown"),
            started,
            device.sessions().len()
        ),
        vec![field(
            "Capabilities",
            &capability_list(&device.capabilities),
            false,
        )],
    )
}

async fn status_view(
    state: &BackendState,
    context: &Context,
    _devices: &[DeviceRecord],
    explicit_session: Option<String>,
) -> Value {
    let resolved = match resolve_session(state, context, explicit_session.as_deref()).await {
        Ok(resolved) => resolved,
        Err(message) => return ephemeral(&message),
    };
    let device = resolved.device;
    let Some(session) = resolved.session else {
        return ephemeral("No session is selected.");
    };
    let captured = device
        .snapshot
        .as_ref()
        .map(|snapshot| relative_time(snapshot.captured_at, Utc::now()))
        .unwrap_or_else(|| "unknown".into());
    embed_response(
        "Session status",
        &format!(
            "**{}** on {}\nSession `{}` · PID {}\nState: **{}**\nLast agent update: {}",
            session
                .username
                .as_deref()
                .unwrap_or(session.profile.as_str()),
            device.name,
            session.id,
            session.pid,
            session.state,
            captured
        ),
        vec![
            field(
                "Version",
                &format!(
                    "{} · {}",
                    session.mc_version.as_deref().unwrap_or("?"),
                    session.loader.as_deref().unwrap_or("?")
                ),
                true,
            ),
            field("Uptime", &format_uptime(session.uptime_seconds), true),
            field("Memory", &format_bytes(session.memory_bytes), true),
            field(
                "CPU",
                &session
                    .cpu_percent
                    .map(|cpu| format!("{cpu:.1}%"))
                    .unwrap_or_else(|| "unavailable".into()),
                true,
            ),
            field(
                "Mods",
                &format!(
                    "{} file(s){}",
                    session.mods_count.unwrap_or(0),
                    if session.baritone_detected {
                        " · Baritone detected"
                    } else {
                        ""
                    }
                ),
                false,
            ),
            field(
                "SpiderBridge",
                &bridge_field(&session),
                false,
            ),
        ],
    )
}

/// Human-readable bridge state. Every branch is derived from values the agent
/// actually reported; a missing sample is never described as live.
fn bridge_field(session: &crate::protocol::SessionSnapshot) -> String {
    let Some(bridge) = &session.bridge else {
        return "not connected - install SpiderBridge in this instance, or the agent has its loopback bridge disabled".into();
    };
    let Some(age) = bridge.age_seconds else {
        return "connected, no telemetry sample yet".into();
    };
    let state = match bridge.state.as_str() {
        "in_world" => "in a world",
        "menu" => "at the menu",
        "no_player" => "world loading / no player",
        other => other,
    };
    let mut text = format!(
        "{} - {state} - Minecraft {} ({}), sample {}s ago",
        if bridge.fresh { "live" } else { "STALE" },
        bridge.mc_version,
        bridge.loader,
        age
    );
    if let (true, Some(dimension), Some(position)) =
        (bridge.fresh, bridge.dimension.as_deref(), bridge.position.as_ref())
    {
        text.push_str(&format!(
            "\n`{dimension}` X {:.1} Y {:.1} Z {:.1}",
            position.x, position.y, position.z
        ));
    }
    text
}

async fn screenshot_command(state: &BackendState, context: &Context, data: &CommandData) -> Value {
    let resolved =
        match resolve_session(state, context, option_str(data, "session").as_deref()).await {
            Ok(resolved) => resolved,
            Err(message) => return ephemeral(&message),
        };
    let Some(session) = resolved.session else {
        return ephemeral("No session selected.");
    };
    if !effective_capabilities(state, &resolved.device).screenshot {
        return ephemeral(
            "Screenshot capture is not enabled. The device owner must set `allow_screenshot` in the agent config and the backend must set `SPIDER_ALLOW_SCREENSHOT=1`. No image was captured.",
        );
    }
    if !state.allow_screenshot(&context.user_id, &resolved.device.id) {
        return ephemeral(&format!(
            "A screenshot was requested recently. Wait {} seconds before trying again.",
            crate::protocol::SCREENSHOT_COOLDOWN_SECONDS
        ));
    }
    enqueue_and_defer(
        state,
        context,
        &resolved.device,
        CommandKind::Screenshot,
        Some(session.id.clone()),
        json!({}),
        &format!("screenshot of session {}", session.id),
        false,
        false,
    )
    .await
}

async fn logs_command(state: &BackendState, context: &Context, data: &CommandData) -> Value {
    let resolved =
        match resolve_session(state, context, option_str(data, "session").as_deref()).await {
            Ok(resolved) => resolved,
            Err(message) => return ephemeral(&message),
        };
    let Some(session) = resolved.session else {
        return ephemeral("No session selected.");
    };
    if !effective_capabilities(state, &resolved.device).logs {
        return ephemeral("Log access is disabled for this device.");
    }
    let lines = option_i64(data, "lines").unwrap_or(120).clamp(1, 200) as usize;
    enqueue_and_defer(
        state,
        context,
        &resolved.device,
        CommandKind::GetLogs,
        Some(session.id.clone()),
        json!(LogsPayload { lines }),
        &format!("logs: last {lines} line(s) of session {}", session.id),
        false,
        false,
    )
    .await
}

async fn launch_command(state: &BackendState, context: &Context, data: &CommandData) -> Value {
    let resolved = match resolve_device(state, context).await {
        Ok(resolved) => resolved,
        Err(message) => return ephemeral(&message),
    };
    if !effective_capabilities(state, &resolved.device).launch {
        return ephemeral("Launching is not enabled for this device.");
    }
    if option_bool(data, "confirm") != Some(true) {
        return ephemeral("Set `confirm:true` to launch. No action was taken.");
    }
    let Some(installation) = option_str(data, "installation") else {
        return ephemeral("An installation name is required.");
    };
    enqueue_and_defer(
        state,
        context,
        &resolved.device,
        CommandKind::Launch,
        None,
        json!(LaunchPayload {
            installation: installation.clone(),
            confirm: true
        }),
        &format!("launch installation {installation}"),
        true,
        true,
    )
    .await
}

async fn stop_command(state: &BackendState, context: &Context, data: &CommandData) -> Value {
    let resolved =
        match resolve_session(state, context, option_str(data, "session").as_deref()).await {
            Ok(resolved) => resolved,
            Err(message) => return ephemeral(&message),
        };
    let Some(session) = resolved.session else {
        return ephemeral("No session selected.");
    };
    if !effective_capabilities(state, &resolved.device).stop {
        return ephemeral("Stopping a game is not enabled for this device.");
    }
    if option_bool(data, "confirm") != Some(true) {
        return ephemeral("Set `confirm:true` to stop. No action was taken.");
    }
    enqueue_and_defer(
        state,
        context,
        &resolved.device,
        CommandKind::Stop,
        Some(session.id.clone()),
        json!(StopPayload {
            session_id: session.id.clone(),
            confirm: true
        }),
        &format!("stop session {}", session.id),
        true,
        true,
    )
    .await
}

async fn mods_command(state: &BackendState, context: &Context) -> Value {
    let resolved = match resolve_session(state, context, None).await {
        Ok(resolved) => resolved,
        Err(message) => return ephemeral(&message),
    };
    let Some(session) = resolved.session else {
        return ephemeral("No session selected.");
    };
    enqueue_and_defer(
        state,
        context,
        &resolved.device,
        CommandKind::GetMods,
        Some(session.id.clone()),
        json!({}),
        &format!("list mods for session {}", session.id),
        false,
        false,
    )
    .await
}

async fn coords_command(state: &BackendState, context: &Context, data: &CommandData) -> Value {
    let resolved =
        match resolve_session(state, context, option_str(data, "session").as_deref()).await {
            Ok(resolved) => resolved,
            Err(message) => return ephemeral(&message),
        };
    let Some(session) = resolved.session else {
        return ephemeral("No session selected.");
    };
    if !effective_capabilities(state, &resolved.device).coordinates {
        return ephemeral(
            "Live coordinates are disabled. The agent needs `allow_bridge` (on by default) and this backend needs `SPIDER_ALLOW_COORDINATES=1`.",
        );
    }
    // The agent grants this per session only while the mod's bridge is
    // attached, so a false value means \"no companion mod\", not \"bad request\".
    if !session.capabilities.coordinates {
        return ephemeral(&format!(
            "The SpiderBridge mod is not connected to session `{}` on **{}**. Install the mod in that Minecraft instance, confirm it started, then try again. Nothing was queued.",
            session.id, resolved.device.name
        ));
    }
    enqueue_and_defer(
        state,
        context,
        &resolved.device,
        CommandKind::Coordinates,
        Some(session.id.clone()),
        json!({}),
        &format!("live coordinates for session {}", session.id),
        false,
        false,
    )
    .await
}

async fn chat_command(state: &BackendState, context: &Context, data: &CommandData) -> Value {
    let resolved =
        match resolve_session(state, context, option_str(data, "session").as_deref()).await {
            Ok(resolved) => resolved,
            Err(message) => return ephemeral(&message),
        };
    let Some(session) = resolved.session else {
        return ephemeral("No session selected.");
    };
    let summary = "chat history (read-only)".to_string();
    if !effective_capabilities(state, &resolved.device).chat {
        return deny_with_audit(
            state,
            context,
            &resolved.device,
            Some(&session.id),
            "chat",
            &summary,
            false,
            "Chat history is disabled on this device or by the backend policy.",
        )
        .await;
    }
    if !session.capabilities.chat {
        return ephemeral(&format!(
            "The SpiderBridge mod is not connected to session `{}` on **{}**. No chat was read.",
            session.id, resolved.device.name
        ));
    }
    if !state.allow_action(
        &context.user_id,
        &resolved.device.id,
        "chat",
        crate::protocol::CHAT_COOLDOWN_SECONDS,
    ) {
        return ephemeral(&format!(
            "Chat history was requested recently. Wait {} seconds.",
            crate::protocol::CHAT_COOLDOWN_SECONDS
        ));
    }
    let limit = option_i64(data, "limit")
        .unwrap_or(50)
        .clamp(1, crate::protocol::MAX_CHAT_MESSAGES as i64) as usize;
    let include_system = option_bool(data, "include_system").unwrap_or(true);
    let include_action_bar = option_bool(data, "include_action_bar").unwrap_or(false);
    enqueue_and_defer(
        state,
        context,
        &resolved.device,
        CommandKind::ChatLogs,
        Some(session.id),
        json!(ChatLogsPayload {
            limit,
            include_system,
            include_action_bar
        }),
        &format!("chat history limit={limit}"),
        false,
        false,
    )
    .await
}

async fn say_command(state: &BackendState, context: &Context, data: &CommandData) -> Value {
    let resolved =
        match resolve_session(state, context, option_str(data, "session").as_deref()).await {
            Ok(resolved) => resolved,
            Err(message) => return ephemeral(&message),
        };
    let Some(session) = resolved.session else {
        return ephemeral("No session selected.");
    };
    let Some(message) = option_str(data, "message") else {
        return ephemeral("A `message` option is required.");
    };
    // The audit row records that a chat line was requested, not the line
    // itself: chat bodies must not be persisted anywhere outside the one
    // Discord reply that the owner explicitly asked for.
    let summary = format!("say request ({} characters)", message.chars().count());
    let message = match crate::command_policy::validate_chat_message(&message) {
        Ok(message) => message,
        Err(reason) => {
            return deny_with_audit(
                state,
                context,
                &resolved.device,
                Some(&session.id),
                "say",
                &summary,
                false,
                &reason,
            )
            .await;
        }
    };
    if !effective_capabilities(state, &resolved.device).chat_send {
        return deny_with_audit(
            state,
            context,
            &resolved.device,
            Some(&session.id),
            "say",
            &summary,
            false,
            "Remote chat sending is disabled on this device or by the backend policy.",
        )
        .await;
    }
    if !session.capabilities.chat_send {
        return ephemeral(&format!(
            "The SpiderBridge mod is not connected to session `{}`, so no chat was sent.",
            session.id
        ));
    }
    if !state.allow_action(
        &context.user_id,
        &resolved.device.id,
        "say",
        crate::protocol::SAY_COOLDOWN_SECONDS,
    ) {
        return ephemeral(&format!(
            "A chat message was sent recently. Wait {} seconds.",
            crate::protocol::SAY_COOLDOWN_SECONDS
        ));
    }
    enqueue_and_defer(
        state,
        context,
        &resolved.device,
        CommandKind::Say,
        Some(session.id),
        json!(SayPayload { message }),
        &summary,
        false,
        true,
    )
    .await
}

async fn minecraft_command(state: &BackendState, context: &Context, data: &CommandData) -> Value {
    let resolved =
        match resolve_session(state, context, option_str(data, "session").as_deref()).await {
            Ok(resolved) => resolved,
            Err(message) => return ephemeral(&message),
        };
    let Some(session) = resolved.session else {
        return ephemeral("No session selected.");
    };
    let Some(raw) = option_str(data, "command") else {
        return ephemeral("A `command` option is required.");
    };
    let summary = format!("mc-command {}", redact(&raw));
    let classified = match crate::command_policy::classify(&raw) {
        Ok(classified) => classified,
        Err(reason) => {
            return deny_with_audit(
                state,
                context,
                &resolved.device,
                Some(&session.id),
                "minecraft_command",
                &summary,
                false,
                &reason,
            )
            .await;
        }
    };
    if !state.config.command_policy.allows(classified.category) {
        return deny_with_audit(
            state,
            context,
            &resolved.device,
            Some(&session.id),
            "minecraft_command",
            &summary,
            classified.requires_confirmation(),
            &format!(
                "The backend does not allow the {} command category.",
                classified.category.as_str().replace('_', "-")
            ),
        )
        .await;
    }
    if !effective_capabilities(state, &resolved.device).minecraft_command {
        return deny_with_audit(
            state,
            context,
            &resolved.device,
            Some(&session.id),
            "minecraft_command",
            &summary,
            classified.requires_confirmation(),
            "Remote Minecraft command execution is disabled on this device or by the backend policy.",
        )
        .await;
    }
    if !session.capabilities.minecraft_command {
        return ephemeral(&format!(
            "The SpiderBridge mod is not connected to session `{}`, so no command was submitted.",
            session.id
        ));
    }
    let confirmation_required =
        state.config.require_command_confirmation && classified.requires_confirmation();
    let confirmation_obtained = option_bool(data, "confirm") == Some(true);
    if confirmation_required && !confirmation_obtained {
        return deny_with_audit(
            state,
            context,
            &resolved.device,
            Some(&session.id),
            "minecraft_command",
            &summary,
            true,
            "The request was not confirmed. Repeat it with `confirm:true` to submit this state-changing command.",
        )
        .await;
    }
    if !state.allow_action(
        &context.user_id,
        &resolved.device.id,
        "minecraft_command",
        crate::protocol::MINECRAFT_COMMAND_COOLDOWN_SECONDS,
    ) {
        return ephemeral(&format!(
            "A command was submitted recently. Wait {} seconds.",
            crate::protocol::MINECRAFT_COMMAND_COOLDOWN_SECONDS
        ));
    }
    enqueue_and_defer(
        state,
        context,
        &resolved.device,
        CommandKind::MinecraftCommand,
        Some(session.id),
        json!(MinecraftCommandPayload {
            command: classified.command,
            confirm: true
        }),
        &format!("{summary} [{}]", classified.category.as_str()),
        confirmation_required,
        confirmation_obtained || !confirmation_required,
    )
    .await
}

async fn command_logs_command(
    state: &BackendState,
    context: &Context,
    data: &CommandData,
) -> Value {
    let devices = match authorized_devices(state, &context.user_id).await {
        Ok(devices) => devices,
        Err(message) => return ephemeral(&message),
    };
    let device_filter = option_str(data, "device");
    if let Some(device_id) = &device_filter {
        if !devices.iter().any(|device| &device.id == device_id) {
            return ephemeral("That device is not authorized for this Discord account.");
        }
    }
    let is_admin = state.config.is_admin(&context.user_id);
    let requester = if is_admin {
        option_str(data, "requester")
    } else {
        Some(context.user_id.clone())
    };
    let session_filter = option_str(data, "session");
    let outcome = option_str(data, "outcome");
    if let Some(outcome) = &outcome
        && !matches!(
            outcome.as_str(),
            "queued"
                | "agent_received"
                | "completed"
                | "submitted_unverified"
                | "submitted_verified"
                | "failed"
                | "rejected"
                | "denied"
                | "expired"
                | "unknown"
        )
    {
        return ephemeral(
            "Unknown outcome filter. Use one of: queued, agent_received, completed, submitted_unverified, submitted_verified, failed, rejected, denied, expired, unknown.",
        );
    }
    let minutes = option_i64(data, "minutes").unwrap_or(1_440).clamp(1, 10_080);
    let page = option_i64(data, "page").unwrap_or(0).max(0);

    if option_bool(data, "clear") == Some(true) {
        if option_bool(data, "confirm") != Some(true) {
            return ephemeral("Set `confirm:true` to clear audit history. Nothing was deleted.");
        }
        let target = requester
            .as_deref()
            .unwrap_or(&context.user_id);
        if !is_admin && target != context.user_id {
            return ephemeral("Only an administrator can clear another user's audit history.");
        }
        return match state
            .store
            .clear_audit(target, device_filter.as_deref())
            .await
        {
            Ok(0) => ephemeral("No matching audit rows were found; nothing was deleted."),
            Ok(count) => embed_response(
                "Audit history cleared",
                &format!(
                    "Deleted {count} row(s) for {}.\nAudit rows also expire automatically after {} day(s).",
                    if is_admin {
                        format!("requester `{target}`")
                    } else {
                        "your Discord account".to_string()
                    },
                    state.config.audit_retention_days
                ),
                Vec::new(),
            ),
            Err(err) => ephemeral(&format!("Could not clear audit history: {err}")),
        };
    }

    let filter = crate::store::AuditFilter {
        requester,
        device_id: device_filter,
        session_id: session_filter,
        outcome,
        since: Some(
            Utc::now() - chrono::Duration::minutes(minutes),
        ),
        limit: 10,
        offset: (page as usize) * 10,
    };
    let rows = match state.store.list_audit(&filter).await {
        Ok(rows) => rows,
        Err(err) => return ephemeral(&format!("Could not read audit history: {err}")),
    };
    if rows.is_empty() {
        return embed_response(
            "Command audit",
            &format!(
                "No audit rows matched.\nRetention: {} day(s). Only requests recorded by this backend are shown; nothing is reconstructed.",
                state.config.audit_retention_days
            ),
            Vec::new(),
        );
    }
    let fields = rows
        .iter()
        .take(crate::protocol::MAX_AUDIT_ROWS)
        .map(|row| {
            let when = row.created_at.format("%Y-%m-%d %H:%M:%SZ").to_string();
            let command = row
                .command_id
                .as_deref()
                .map(|id| &id[..8.min(id.len())])
                .unwrap_or("-");
            let mut text = format!(
                "`{}` · session `{}` · {}\nOutcome: **{}** · authorized: {}\nAgent received: {} · submitted: {} · verified: {}",
                command,
                row.session_id.as_deref().unwrap_or("-"),
                row.kind,
                row.outcome,
                yes_no(row.authorized),
                row.agent_received_at
                    .map(|time| time.format("%H:%M:%SZ").to_string())
                    .unwrap_or_else(|| "no".into()),
                row.submitted.map(yes_no).unwrap_or("unknown"),
                row.verified.map(yes_no).unwrap_or("unknown"),
            );
            if row.confirmation_required {
                text.push_str(&format!(
                    "\nConfirmation: required · obtained: {}",
                    yes_no(row.confirmation_obtained)
                ));
            }
            if let Some(error) = &row.authorization_error {
                text.push_str(&format!("\nReason: {}", truncate_text(&redact(error), 200)));
            }
            if let Some(detail) = &row.detail {
                text.push_str(&format!("\n{}", truncate_text(&redact(detail), 200)));
            }
            field(&format!("{when} · {}", row.payload_summary), &text, false)
        })
        .collect();
    embed_response(
        "Command audit",
        &format!(
            "Page {}. Showing {} row(s) from the last {} minute(s).\n`submitted` means the game client accepted the request; `verified` means a reliable event confirmed the outcome. Retention: {} day(s).",
            page + 1,
            rows.len(),
            minutes,
            state.config.audit_retention_days
        ),
        fields,
    )
}

async fn deny_with_audit(
    state: &BackendState,
    context: &Context,
    device: &DeviceRecord,
    session_id: Option<&str>,
    kind: &str,
    payload_summary: &str,
    confirmation_required: bool,
    reason: &str,
) -> Value {
    if let Err(err) = state
        .store
        .record_denied_audit(
            &context.user_id,
            &device.id,
            session_id,
            kind,
            payload_summary,
            confirmation_required,
            reason,
        )
        .await
    {
        tracing::warn!(error = %err, "could not record a denied remote action");
    }
    ephemeral(&format!("{reason}\nNo action was taken."))
}

async fn settings_view(state: &BackendState, context: &Context) -> Value {
    let resolved = match resolve_device(state, context).await {
        Ok(resolved) => resolved,
        Err(message) => return ephemeral(&message),
    };
    let device = resolved.device;
    let backend_policy = state.config.policy_capabilities();
    let effective = effective_capabilities(state, &device);
    let gate = |enabled: bool| if enabled { "enabled" } else { "disabled" };
    let confirmation = if state.config.require_command_confirmation {
        "required for state-changing commands"
    } else {
        "not required (the agent still requires it)"
    };
    embed_response(
        "Remote settings",
        &format!(
            "**{}** · `{}`\nThese are the *effective* settings for this device. A feature works only when the device's own agent config and this backend both allow it. The backend cannot read or change the agent's local `agent.json`.",
            device.name, device.id
        ),
        vec![
            field(
                "Device declared",
                &capability_list(&device.capabilities),
                false,
            ),
            field(
                "Backend policy allows",
                &capability_list(&backend_policy),
                false,
            ),
            field("Effective now", &capability_list(&effective), false),
            field(
                "Minecraft command categories",
                &format!(
                    "Read-only: **{}**\nDisruptive: **{}**\nAdministrative: **{}**\nConfirmation: {}",
                    gate(state.config.command_policy.read_only),
                    gate(state.config.command_policy.disruptive),
                    gate(state.config.command_policy.administrative),
                    confirmation
                ),
                false,
            ),
            field(
                "Chat and audit",
                &format!(
                    "Chat history: **{}**\nChat send: **{}**\nMinecraft command: **{}**\nAudit retention: **{} day(s)**\nChat bodies are relayed to Discord and are not stored in the audit database.",
                    gate(effective.chat),
                    gate(effective.chat_send),
                    gate(effective.minecraft_command),
                    state.config.audit_retention_days
                ),
                false,
            ),
            field(
                "Truthful limits",
                "Chat history is bounded and in-memory in the game; it is never reconstructed from `latest.log`. A submitted command is reported as **submitted, unverified** because vanilla chat does not correlate command output with a request.",
                false,
            ),
        ],
    )
}

async fn specs_view(state: &BackendState, context: &Context) -> Value {
    let resolved = match resolve_device(state, context).await {
        Ok(resolved) => resolved,
        Err(message) => return ephemeral(&message),
    };
    let Some(snapshot) = resolved.device.snapshot else {
        return ephemeral("The agent has not sent a system snapshot yet.");
    };
    embed_response(
        "Computer specifications",
        &format!(
            "Reported by **{}** at {}.",
            resolved.device.name,
            relative_time(snapshot.captured_at, Utc::now())
        ),
        specs_fields(&snapshot.specs),
    )
}

async fn performance_view(state: &BackendState, context: &Context) -> Value {
    let resolved = match resolve_session(state, context, None).await {
        Ok(resolved) => resolved,
        Err(message) => return ephemeral(&message),
    };
    let Some(session) = resolved.session else {
        return ephemeral("No session selected.");
    };
    let Some(snapshot) = resolved.device.snapshot else {
        return ephemeral("The agent has not sent a snapshot yet.");
    };
    let mut fields = vec![
        field("Session memory", &format_bytes(session.memory_bytes), true),
        field(
            "Session CPU",
            &session
                .cpu_percent
                .map(|cpu| format!("{cpu:.1}%"))
                .unwrap_or_else(|| "unavailable".into()),
            true,
        ),
        field(
            "System memory",
            &format_bytes(snapshot.specs.total_memory_bytes),
            true,
        ),
        field(
            "Available RAM",
            &format_bytes(snapshot.specs.available_memory_bytes),
            true,
        ),
    ];
    fields.push(field(
        "Measurement",
        "Values come from the most recent agent heartbeat. They are not a continuous in-game FPS feed.",
        false,
    ));
    embed_response("Performance", &format!("Session `{}`", session.id), fields)
}

async fn uptime_view(state: &BackendState, context: &Context) -> Value {
    let resolved = match resolve_session(state, context, None).await {
        Ok(resolved) => resolved,
        Err(message) => return ephemeral(&message),
    };
    let Some(session) = resolved.session else {
        return ephemeral("No session selected.");
    };
    let agent_uptime = resolved
        .device
        .snapshot
        .as_ref()
        .map(|snapshot| {
            format_uptime(
                (Utc::now() - snapshot.agent_started_at)
                    .num_seconds()
                    .max(0) as u64,
            )
        })
        .unwrap_or_else(|| "unknown".into());
    embed_response(
        "Uptime",
        &format!(
            "Agent: **{}** (since the current agent process started)\nSession `{}`: **{}**\nStarted: {}",
            agent_uptime,
            session.id,
            format_uptime(session.uptime_seconds),
            session.started_at.to_rfc3339()
        ),
        Vec::new(),
    )
}

async fn alerts_view(state: &BackendState, context: &Context) -> Value {
    let resolved = match resolve_device(state, context).await {
        Ok(resolved) => resolved,
        Err(message) => return ephemeral(&message),
    };
    let configured = state.config.alert_channel_id.is_some();
    embed_response(
        "Alerts",
        &format!(
            "Device: **{}**\nAlert channel configured: **{}**\nCooldown: 60 seconds per transition\n\nEvents: agent online/offline, session start/end. A crash is reported only as a session that disappeared; the agent does not claim to know why it ended.",
            resolved.device.name,
            yes_no(configured)
        ),
        Vec::new(),
    )
}

async fn baritone_status(state: &BackendState, context: &Context) -> Value {
    let resolved = match resolve_session(state, context, None).await {
        Ok(resolved) => resolved,
        Err(message) => return ephemeral(&message),
    };
    let Some(session) = resolved.session else {
        return ephemeral("No session selected.");
    };
    let detected = session.baritone_detected
        || session
            .bridge
            .as_ref()
            .is_some_and(|bridge| bridge.baritone_detected);
    if !detected {
        return embed_response(
            "Baritone",
            "No Baritone jar was detected in this session's mods folder.",
            Vec::new(),
        );
    }
    let integration = session
        .bridge
        .as_ref()
        .map(|bridge| bridge.baritone_integration.as_str())
        .unwrap_or("unavailable");
    let detail = if integration == "unavailable" {
        "A Baritone jar was detected. SpiderBridge exposes detection only: no verified public Baritone API is compiled into this build, so status and control are not claimed and no command is sent."
    } else {
        "A Baritone jar was detected and the companion mod reports a compiled-in integration."
    };
    embed_response("Baritone", detail, vec![field("Integration", integration, false)])
}

async fn pair_command(state: &BackendState, context: &Context, data: &CommandData) -> Value {
    if !state.config.is_admin(&context.user_id) {
        return ephemeral("Only a configured SpiderClient administrator can create pairing codes.");
    }
    let owner = option_str(data, "owner").unwrap_or_else(|| context.user_id.clone());
    match state
        .store
        .create_pairing_code(&owner, state.config.pair_ttl_seconds)
        .await
    {
        Ok(code) => embed_response(
            "Device pairing code",
            &format!(
                "```\n{}\n```\nSingle use. Expires {}.\nOn the gaming PC run:\n`spider-agent pair --server https://YOUR_BACKEND --code <code> --name \"Gaming PC\"`\n\nThe bot token is never sent to the agent.",
                code.code,
                relative_time(code.expires_at, Utc::now())
            ),
            Vec::new(),
        ),
        Err(err) => ephemeral(&format!("Could not create a pairing code: {err}")),
    }
}

#[allow(clippy::too_many_arguments)]
async fn enqueue_and_defer(
    state: &BackendState,
    context: &Context,
    device: &DeviceRecord,
    kind: CommandKind,
    session_id: Option<String>,
    payload: Value,
    payload_summary: &str,
    confirmation_required: bool,
    confirmation_obtained: bool,
) -> Value {
    if !state.allow_command(&context.user_id) {
        return ephemeral("Too many remote requests; wait a minute and try again.");
    }
    if !effective_capabilities(state, device).supports(&kind) {
        return ephemeral("This device has not enabled that capability.");
    }
    // A command that waits for an offline agent would sit until the 120-second
    // expiry and then answer with a timeout. Rejecting it now tells the operator
    // what is actually wrong and avoids queuing work the agent cannot claim.
    if !device.online {
        return ephemeral(&format!(
            "The agent on **{}** is offline. Start SpiderClient or the agent on that computer, then try again. Nothing was queued.",
            device.name
        ));
    }
    let Some(token) = context.interaction_token.clone() else {
        return ephemeral("This interaction cannot receive a follow-up.");
    };
    let result = state
        .store
        .enqueue_command(
            &device.id,
            kind.clone(),
            session_id,
            payload,
            payload_summary,
            &context.user_id,
            confirmation_required,
            confirmation_obtained,
            Some(token),
            Some(context.application_id.clone()),
            Some(context.channel_id.clone()),
        )
        .await;
    match result {
        Ok(command) => {
            tracing::info!(
                command_id = %command.id,
                device_id = %device.id,
                actor = %context.user_id,
                kind = kind.as_str(),
                "remote command queued"
            );
            json!({"type": 5, "data": {"flags": EPHEMERAL}})
        }
        Err(err) => ephemeral(&format!("Could not queue the request: {err}")),
    }
}

async fn unpair_command(state: &BackendState, context: &Context, data: &CommandData) -> Value {
    let Some(device_id) = option_str(data, "device") else {
        return ephemeral("A device id is required. Use `/devices`.");
    };
    if option_bool(data, "confirm") != Some(true) {
        return ephemeral("Set `confirm:true` to revoke this device. Nothing was changed.");
    }
    let device = match state.store.get_device(&device_id).await {
        Ok(device) => device,
        Err(_) => return ephemeral("That device is not registered."),
    };
    if device.owner_discord_id != context.user_id && !state.config.is_admin(&context.user_id) {
        return ephemeral("Only the device owner or an administrator can revoke this device.");
    }
    match state.store.revoke_device(&device_id).await {
        Ok(()) => {
            tracing::info!(
                device_id = %device_id,
                actor = %context.user_id,
                "device credential revoked"
            );
            embed_response(
                "Device revoked",
                &format!(
                    "**{}** has been removed and its credential is no longer accepted. Stop the agent on that computer or pair it again with a new code.",
                    device.name
                ),
                Vec::new(),
            )
        }
        Err(err) => ephemeral(&format!("Could not revoke the device: {err}")),
    }
}

fn effective_capabilities(state: &BackendState, device: &DeviceRecord) -> Capabilities {
    device
        .capabilities
        .allowed_by(&state.config.policy_capabilities())
}

fn capability_list(caps: &Capabilities) -> String {
    let labels = caps.labels();
    if labels.is_empty() {
        "none".into()
    } else {
        labels.join(", ")
    }
}

fn help_response() -> Value {
    embed_response(
        "SpiderClient remote commands",
        "All responses are ephemeral. Every command is checked against the device owner and this backend's policy.\n\n**Discovery**\n`/devices` `/sessions` `/select` `/agent` `/pair` `/unpair`\n\n**Monitoring**\n`/status` `/logs` `/mods` `/settings` `/specs` `/performance` `/uptime` `/alerts`\n\n**Live telemetry and chat**\n`/coords` - position reported by the SpiderBridge mod at request time; never cached.\n`/chat` (`/chatlogs`) - bounded in-memory chat history from the mod; opt-in on both the agent and the backend. Never read from `latest.log`.\n`/say` - submits a chat line through the Minecraft client. The bot reports submission, not delivery.\n\n**Screenshots**\n`/screenshot` (`/screen`) - opt-in on the agent and the backend; captures the selected Minecraft window\n\n**Approved actions**\n`/launch` `/stop` - both require `confirm:true`\n`/mc-command` - policy-checked command submission through the client. State-changing categories require `confirm:true`. The result is reported as submitted but unverified.\n`/command-logs` - audit lifecycle for requests this backend recorded; owner-scoped, administrator filters available.\n\n**Truthfully unavailable**\n`/restart-agent` `/baritone-status` (detection only) `/baritone-control` `/pause` `/resume`\nThese require integrations this build does not ship. They return an explanation instead of fake data.",
        Vec::new(),
    )
}

fn unsupported(message: &str) -> Value {
    embed_response("Not available", message, Vec::new())
}

fn ephemeral(message: &str) -> Value {
    json!({
        "type": 4,
        "data": {
            "content": truncate_text(&redact(message), 1900),
            "flags": EPHEMERAL,
            "allowed_mentions": {"parse": []}
        }
    })
}

fn embed_response(title: &str, description: &str, fields: Vec<Value>) -> Value {
    let mut embed = json!({
        "title": truncate_text(title, 256),
        "description": truncate_text(&redact(description), 3800),
        "color": EMBED_COLOR,
    });
    if !fields.is_empty() {
        embed["fields"] = Value::Array(fields.into_iter().take(25).collect());
    }
    json!({
        "type": 4,
        "data": {
            "embeds": [embed],
            "flags": EPHEMERAL,
            "allowed_mentions": {"parse": []}
        }
    })
}

fn field(name: &str, value: &str, inline: bool) -> Value {
    json!({
        "name": truncate_text(name, 100),
        "value": truncate_text(&redact(value), 1000),
        "inline": inline
    })
}

fn option<'a>(data: &'a CommandData, name: &str) -> Option<&'a CommandOption> {
    data.options.iter().find(|option| option.name == name)
}

fn option_str(data: &CommandData, name: &str) -> Option<String> {
    option(data, name)
        .and_then(|option| option.value.as_ref())
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn option_bool(data: &CommandData, name: &str) -> Option<bool> {
    option(data, name)
        .and_then(|option| option.value.as_ref())
        .and_then(Value::as_bool)
}

fn option_i64(data: &CommandData, name: &str) -> Option<i64> {
    option(data, name)
        .and_then(|option| option.value.as_ref())
        .and_then(Value::as_i64)
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

fn format_bytes(value: Option<u64>) -> String {
    match value {
        Some(bytes) => {
            let gib = bytes as f64 / 1024.0 / 1024.0 / 1024.0;
            if gib >= 1.0 {
                format!("{gib:.1} GiB")
            } else {
                format!("{:.0} MiB", bytes as f64 / 1024.0 / 1024.0)
            }
        }
        None => "unavailable".into(),
    }
}

fn format_uptime(seconds: u64) -> String {
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let minutes = (seconds % 3_600) / 60;
    if days > 0 {
        format!("{days}d {hours}h {minutes}m")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    }
}

fn relative_time(time: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let seconds = (now - time).num_seconds().max(0);
    if seconds < 60 {
        format!("{seconds}s ago")
    } else if seconds < 3_600 {
        format!("{}m ago", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h ago", seconds / 3_600)
    } else {
        format!("{}d ago", seconds / 86_400)
    }
}

fn specs_fields(specs: &SystemSpecs) -> Vec<Value> {
    vec![
        field("CPU", specs.cpu.as_deref().unwrap_or("unavailable"), false),
        field(
            "Cores",
            &format!(
                "{} physical / {} logical",
                specs
                    .physical_cores
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "?".into()),
                specs
                    .logical_cores
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "?".into())
            ),
            true,
        ),
        field("Memory", &format_bytes(specs.total_memory_bytes), true),
        field(
            "Available",
            &format_bytes(specs.available_memory_bytes),
            true,
        ),
        field("OS", specs.os.as_deref().unwrap_or("unavailable"), false),
        field(
            "Hostname",
            specs.hostname.as_deref().unwrap_or("unavailable"),
            true,
        ),
        field(
            "GPU",
            specs.gpu.as_deref().unwrap_or("not reported by this agent"),
            true,
        ),
    ]
}

pub fn command_definitions() -> Vec<Value> {
    vec![
        command(
            "help",
            "Show SpiderClient commands and permissions.",
            vec![],
        ),
        command(
            "devices",
            "List paired devices.",
            vec![int_option("page", "Page number, starting at 0.", false)],
        ),
        command(
            "sessions",
            "List running Minecraft sessions on paired devices.",
            vec![int_option("page", "Page number, starting at 0.", false)],
        ),
        command(
            "select",
            "Select a device and optionally a session for this channel.",
            vec![
                string_option("device", "Device id from /devices.", true),
                string_option("session", "Session id from /sessions.", false),
            ],
        ),
        command("agent", "Show agent connectivity and capabilities.", vec![]),
        command(
            "status",
            "Show the selected session's real status.",
            vec![string_option(
                "session",
                "Override the selected session id.",
                false,
            )],
        ),
        command(
            "logs",
            "Request a bounded, redacted game-log excerpt.",
            vec![
                string_option("session", "Override the selected session id.", false),
                int_option("lines", "Number of lines (1-200).", false),
            ],
        ),
        command(
            "launch",
            "Launch a configured installation on the selected device.",
            vec![
                string_option("installation", "Installation folder name.", true),
                bool_option("confirm", "Explicitly confirms the action.", true),
            ],
        ),
        command(
            "stop",
            "Request a stop of the selected game session.",
            vec![
                string_option("session", "Override the selected session id.", false),
                bool_option("confirm", "Explicitly confirms the action.", true),
            ],
        ),
        command(
            "restart-agent",
            "Explain how to restart the agent safely.",
            vec![],
        ),
        command(
            "screenshot",
            "Capture the selected Minecraft window (opt-in).",
            vec![string_option(
                "session",
                "Override the selected session id.",
                false,
            )],
        ),
        command(
            "coords",
            "Show live coordinates from the SpiderBridge mod.",
            vec![string_option(
                "session",
                "Override the selected session id.",
                false,
            )],
        ),
        command(
            "chat",
            "Show bounded in-memory chat history from the SpiderBridge mod.",
            vec![
                string_option("session", "Override the selected session id.", false),
                int_option("limit", "Messages to return (1-200).", false),
                bool_option("include_system", "Include system messages.", false),
                bool_option(
                    "include_action_bar",
                    "Include action-bar messages (usually noisy).",
                    false,
                ),
            ],
        ),
        command(
            "say",
            "Submit a chat line through the selected Minecraft client.",
            vec![
                string_option("message", "Chat text, at most 256 characters.", true),
                string_option("session", "Override the selected session id.", false),
            ],
        ),
        command(
            "mc-command",
            "Submit a policy-checked Minecraft command through the client.",
            vec![
                string_option("command", "Command without a leading slash.", true),
                string_option("session", "Override the selected session id.", false),
                bool_option(
                    "confirm",
                    "Required for state-changing command categories.",
                    false,
                ),
            ],
        ),
        command(
            "command-logs",
            "Show or clear the owner-scoped command audit.",
            vec![
                string_option("device", "Filter by device id.", false),
                string_option("session", "Filter by session id.", false),
                string_option(
                    "requester",
                    "Administrators only: filter by Discord user id.",
                    false,
                ),
                string_option("outcome", "Filter by outcome.", false),
                int_option("minutes", "Look back this many minutes (1-10080).", false),
                int_option("page", "Page number, starting at 0.", false),
                bool_option("clear", "Delete matching rows instead of listing.", false),
                bool_option("confirm", "Required with clear:true.", false),
            ],
        ),
        command("mods", "Show detected mods and Baritone presence.", vec![]),
        command(
            "settings",
            "Show agent capabilities and backend policy.",
            vec![],
        ),
        command("specs", "Show reported computer specifications.", vec![]),
        command(
            "performance",
            "Show available performance readings.",
            vec![],
        ),
        command("uptime", "Show agent and session uptime.", vec![]),
        command(
            "alerts",
            "Show alert configuration and supported events.",
            vec![],
        ),
        command(
            "baritone-status",
            "Show whether Baritone is detected.",
            vec![],
        ),
        command(
            "baritone-control",
            "Request a supported Baritone action.",
            vec![
                string_option("action", "Documented action name.", true),
                bool_option("confirm", "Explicitly confirms the action.", true),
            ],
        ),
        command("pause", "Pause a session when supported.", vec![]),
        command("resume", "Resume a session when supported.", vec![]),
        command(
            "pair",
            "Create a single-use device pairing code (administrators).",
            vec![string_option(
                "owner",
                "Discord user id that will own the device.",
                false,
            )],
        ),
        command(
            "unpair",
            "Revoke a device credential and remove it from the registry.",
            vec![
                string_option("device", "Device id from /devices.", true),
                bool_option("confirm", "Explicitly confirms revocation.", true),
            ],
        ),
    ]
}

fn command(name: &str, description: &str, options: Vec<Value>) -> Value {
    let mut value = json!({
        "name": name,
        "description": description,
        "type": 1,
        "dm_permission": true,
        "options": options,
    });
    if options_empty(&value["options"]) {
        value.as_object_mut().unwrap().remove("options");
    }
    value
}

fn options_empty(value: &Value) -> bool {
    value.as_array().is_some_and(Vec::is_empty)
}

fn string_option(name: &str, description: &str, required: bool) -> Value {
    json!({
        "name": name,
        "description": description,
        "type": 3,
        "required": required,
    })
}

fn bool_option(name: &str, description: &str, required: bool) -> Value {
    json!({
        "name": name,
        "description": description,
        "type": 5,
        "required": required,
    })
}

fn int_option(name: &str, description: &str, required: bool) -> Value {
    json!({
        "name": name,
        "description": description,
        "type": 4,
        "required": required,
        "min_value": 0,
        "max_value": 1000,
    })
}

pub async fn register_commands(config: &BackendConfig) -> anyhow::Result<()> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()?;
    let guild = std::env::var("DISCORD_GUILD_ID")
        .ok()
        .filter(|id| !id.trim().is_empty());
    let url = if let Some(guild) = guild {
        format!(
            "https://discord.com/api/v10/applications/{}/guilds/{}/commands",
            config.application_id, guild
        )
    } else {
        format!(
            "https://discord.com/api/v10/applications/{}/commands",
            config.application_id
        )
    };
    let response = client
        .put(url)
        .header("authorization", format!("Bot {}", config.bot_token))
        .json(&command_definitions())
        .send()
        .await?;
    let status = response.status();
    if !status.is_success() {
        anyhow::bail!(
            "Discord command registration failed with HTTP {}",
            status.as_u16()
        );
    }
    println!(
        "Registered {} SpiderClient slash commands{}.",
        command_definitions().len(),
        if let Ok(guild) = std::env::var("DISCORD_GUILD_ID") {
            format!(" in guild {guild}")
        } else {
            " globally".into()
        }
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{BackendConfig, BackendState};
    use crate::command_policy::CommandPolicy;
    use crate::store::{AuditFilter, Store};

    fn base_config() -> BackendConfig {
        BackendConfig {
            bind: "127.0.0.1:0".into(),
            database_url: "sqlite::memory:".into(),
            bot_token: "test-token-not-real".into(),
            public_key: "00".repeat(32),
            application_id: "1".into(),
            admin_discord_ids: vec!["admin-1".into()],
            allowed_guild_ids: vec![],
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
        }
    }

    async fn state_with(config: BackendConfig) -> BackendState {
        let store = Store::open("sqlite::memory:").await.unwrap();
        BackendState::new(store, config).unwrap()
    }

    async fn test_state() -> BackendState {
        state_with(base_config()).await
    }

    async fn test_state_with_screenshot() -> BackendState {
        let mut config = base_config();
        config.allow_screenshot = true;
        state_with(config).await
    }

    async fn test_state_with_coordinates() -> BackendState {
        let mut config = base_config();
        config.allow_coordinates = true;
        state_with(config).await
    }

    async fn test_state_with_chat() -> BackendState {
        let mut config = base_config();
        config.allow_chat = true;
        config.allow_chat_send = true;
        config.allow_minecraft_command = true;
        state_with(config).await
    }

    async fn paired_device(state: &BackendState) -> (String, String) {
        let code = state
            .store
            .create_pairing_code("owner-1", 600)
            .await
            .unwrap();
        state
            .store
            .claim_pairing_code(&code.code, "Test rig", "test", &Capabilities::default())
            .await
            .unwrap()
    }

    fn interaction(name: &str, user: &str) -> Value {
        json!({
            "type": 2,
            "data": {"name": name, "options": []},
            "user": {"id": user},
            "channel_id": "channel-1",
            "token": "interaction-token",
            "application_id": "1"
        })
    }

    #[tokio::test]
    async fn owner_can_list_their_device_but_a_stranger_cannot() {
        let state = test_state().await;
        let (device_id, _) = paired_device(&state).await;
        let owner = handle_interaction(&state, interaction("devices", "owner-1"))
            .await
            .unwrap();
        assert_eq!(owner["data"]["flags"], EPHEMERAL);
        assert!(owner.to_string().contains(&device_id));

        let stranger = handle_interaction(&state, interaction("devices", "stranger"))
            .await
            .unwrap();
        assert!(!stranger.to_string().contains(&device_id));
        assert!(
            stranger["data"]["content"]
                .as_str()
                .unwrap()
                .contains("No SpiderClient device")
        );
    }

    #[tokio::test]
    async fn non_admin_cannot_create_pairing_codes_or_control_a_device() {
        let state = test_state().await;
        let _ = paired_device(&state).await;
        let pair = handle_interaction(&state, interaction("pair", "owner-1"))
            .await
            .unwrap();
        assert!(
            pair["data"]["content"]
                .as_str()
                .unwrap()
                .contains("administrator")
        );

        let launch = handle_interaction(
            &state,
            json!({
                "type": 2,
                "data": {
                    "name": "launch",
                    "options": [
                        {"name": "installation", "value": "Survival"},
                        {"name": "confirm", "value": true}
                    ]
                },
                "user": {"id": "owner-1"},
                "channel_id": "channel-1",
                "token": "interaction-token",
                "application_id": "1"
            }),
        )
        .await
        .unwrap();
        assert!(
            launch["data"]["content"]
                .as_str()
                .unwrap()
                .contains("not enabled")
        );
    }

    #[test]
    fn definitions_are_unique_and_within_discord_limits() {
        let definitions = command_definitions();
        let mut names = std::collections::HashSet::new();
        for definition in &definitions {
            let name = definition["name"].as_str().unwrap();
            assert!(names.insert(name.to_string()), "duplicate command {name}");
            assert!(name.len() <= 32);
            assert!(definition["description"].as_str().unwrap().len() <= 100);
        }
        assert!(definitions.iter().any(|command| command["name"] == "stop"));
        assert!(definitions.len() <= 100);
    }

    #[test]
    fn actions_require_confirmation() {
        for name in ["launch", "stop"] {
            let command = command_definitions()
                .into_iter()
                .find(|command| command["name"] == name)
                .unwrap();
            let confirm = command["options"]
                .as_array()
                .unwrap()
                .iter()
                .find(|option| option["name"] == "confirm")
                .unwrap();
            assert_eq!(confirm["required"], true);
        }
    }

    #[test]
    fn help_is_ephemeral() {
        let response = help_response();
        assert_eq!(response["data"]["flags"], EPHEMERAL);
    }

    fn bridge_snapshot(fresh: bool) -> crate::protocol::BridgeSnapshot {
        crate::protocol::BridgeSnapshot {
            state: "in_world".into(),
            mc_version: "1.21.1".into(),
            loader: "fabric".into(),
            bridge_version: "0.1.0".into(),
            dimension: Some("minecraft:overworld".into()),
            position: Some(crate::protocol::PositionSnapshot {
                x: 100.5,
                y: 70.0,
                z: -200.25,
                yaw: 45.0,
                pitch: -10.0,
            }),
            player_name: Some("Alex".into()),
            health: Some(20.0),
            food_level: Some(20),
            fps: Some(144),
            age_seconds: Some(if fresh { 1 } else { 400 }),
            fresh,
            baritone_detected: false,
            baritone_integration: "unavailable".into(),
        }
    }

    async fn record_running_session(
        state: &BackendState,
        device_id: &str,
        capabilities: Capabilities,
    ) {
        record_running_session_with_bridge(state, device_id, capabilities.clone(), capabilities, None)
            .await;
    }

    /// Device capabilities come from the agent; session capabilities describe
    /// what that particular instance can do right now (for example whether the
    /// companion mod is attached).
    async fn record_running_session_with_bridge(
        state: &BackendState,
        device_id: &str,
        capabilities: Capabilities,
        session_capabilities: Capabilities,
        bridge: Option<crate::protocol::BridgeSnapshot>,
    ) {
        let snapshot = crate::protocol::DeviceSnapshot {
            protocol: crate::protocol::PROTOCOL_VERSION,
            agent_version: "test".into(),
            platform: "windows".into(),
            specs: crate::protocol::SystemSpecs::default(),
            sessions: vec![crate::protocol::SessionSnapshot {
                id: "session-1".into(),
                profile: "Minecraft".into(),
                username: Some("Alex".into()),
                mc_version: Some("1.21.11".into()),
                loader: Some("Fabric".into()),
                pid: 4242,
                started_at: Utc::now(),
                uptime_seconds: 60,
                memory_bytes: None,
                cpu_percent: None,
                mods_count: None,
                baritone_detected: false,
                bridge,
                state: "running".into(),
                capabilities: session_capabilities,
            }],
            capabilities,
            captured_at: Utc::now(),
            agent_started_at: Utc::now(),
            log_excerpt_supported: true,
        };
        state
            .store
            .record_heartbeat(device_id, &snapshot)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn screenshot_requires_both_sides_to_opt_in() {
        let state = test_state_with_screenshot().await;
        let (device_id, _) = paired_device(&state).await;
        // The agent reported a session but did not enable capture itself.
        record_running_session(&state, &device_id, Capabilities::default()).await;
        let response = handle_interaction(
            &state,
            json!({
                "type": 2,
                "data": {"name": "screenshot", "options": []},
                "user": {"id": "owner-1"},
                "channel_id": "channel-1",
                "token": "interaction-token",
                "application_id": "1"
            }),
        )
        .await
        .unwrap();
        assert!(
            response["data"]["content"]
                .as_str()
                .unwrap()
                .contains("not enabled"),
            "a device that has not enabled screenshots must be refused"
        );
    }

    #[tokio::test]
    async fn enabled_screenshot_is_deferred_and_rate_limited() {
        let state = test_state_with_screenshot().await;
        let (device_id, _) = paired_device(&state).await;
        let mut capabilities = Capabilities::default();
        capabilities.screenshot = true;
        record_running_session(&state, &device_id, capabilities.clone()).await;

        let interaction = || {
            json!({
                "type": 2,
                "data": {"name": "screenshot", "options": []},
                "user": {"id": "owner-1"},
                "channel_id": "channel-1",
                "token": "interaction-token",
                "application_id": "1"
            })
        };
        let first = handle_interaction(&state, interaction()).await.unwrap();
        assert_eq!(first["type"], 5, "a capture request must be deferred");
        let second = handle_interaction(&state, interaction()).await.unwrap();
        assert_eq!(second["type"], 4);
        assert!(
            second["data"]["content"]
                .as_str()
                .unwrap()
                .contains("requested recently")
        );

        // The queued request is bound to the session that was selected when the
        // operator asked, not to whatever appears later.
        let pending = state
            .store
            .claim_pending_commands(&device_id, 8)
            .await
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].session_id.as_deref(), Some("session-1"));
        assert_eq!(pending[0].kind, CommandKind::Screenshot);
    }

    #[tokio::test]
    async fn an_offline_agent_is_rejected_before_a_command_is_queued() {
        let state = test_state_with_screenshot().await;
        let (device_id, _) = paired_device(&state).await;
        let mut capabilities = Capabilities::default();
        capabilities.screenshot = true;
        record_running_session(&state, &device_id, capabilities).await;
        state.store.mark_offline(&device_id).await.unwrap();

        let response = handle_interaction(
            &state,
            json!({
                "type": 2,
                "data": {"name": "screenshot", "options": []},
                "user": {"id": "owner-1"},
                "channel_id": "channel-1",
                "token": "interaction-token",
                "application_id": "1"
            }),
        )
        .await
        .unwrap();
        assert_eq!(response["type"], 4);
        assert!(
            response["data"]["content"]
                .as_str()
                .unwrap()
                .contains("offline"),
            "an offline agent must be reported instead of waiting for the timeout"
        );
        let pending = state
            .store
            .claim_pending_commands(&device_id, 8)
            .await
            .unwrap();
        assert!(
            pending.is_empty(),
            "an offline agent must not receive a queued command"
        );
    }

    #[test]
    fn formatting_helpers_are_bounded() {
        assert_eq!(format_uptime(90), "1m");
        assert_eq!(format_uptime(3_700), "1h 1m");
        assert_eq!(format_bytes(Some(1024 * 1024 * 1024)), "1.0 GiB");
    }

    #[tokio::test]
    async fn coords_are_refused_until_the_backend_policy_opts_in() {
        // Default `test_state` has allow_coordinates = false, like a stock
        // deployment. Even a connected bridge must not be exposed.
        let state = test_state().await;
        let (device_id, _) = paired_device(&state).await;
        let mut capabilities = Capabilities::default();
        capabilities.coordinates = true;
        record_running_session_with_bridge(
            &state,
            &device_id,
            capabilities.clone(),
            capabilities,
            Some(bridge_snapshot(true)),
        )
        .await;

        let response = handle_interaction(&state, interaction("coords", "owner-1"))
            .await
            .unwrap();
        assert_eq!(response["type"], 4);
        assert!(response["data"]["content"]
            .as_str()
            .unwrap()
            .contains("SPIDER_ALLOW_COORDINATES"));
        let pending = state
            .store
            .claim_pending_commands(&device_id, 8)
            .await
            .unwrap();
        assert!(
            pending.is_empty(),
            "a policy refusal must not queue a command"
        );
    }

    #[tokio::test]
    async fn coords_report_a_missing_mod_instead_of_queuing_a_request() {
        let state = test_state_with_coordinates().await;
        let (device_id, _) = paired_device(&state).await;
        // The agent advertises the bridge feature, but no mod is attached to
        // this session, so the per-session capability stays false.
        let mut capabilities = Capabilities::default();
        capabilities.coordinates = true;
        record_running_session_with_bridge(
            &state,
            &device_id,
            capabilities,
            Capabilities::default(),
            None,
        )
        .await;

        let response = handle_interaction(&state, interaction("coords", "owner-1"))
            .await
            .unwrap();
        assert_eq!(response["type"], 4);
        let content = response["data"]["content"].as_str().unwrap();
        assert!(content.contains("SpiderBridge"));
        assert!(content.contains("Nothing was queued"));
        let pending = state
            .store
            .claim_pending_commands(&device_id, 8)
            .await
            .unwrap();
        assert!(pending.is_empty(), "no request without a live mod");
    }

    #[tokio::test]
    async fn coords_queue_a_live_request_for_the_selected_session() {
        let state = test_state_with_coordinates().await;
        let (device_id, _) = paired_device(&state).await;
        let mut capabilities = Capabilities::default();
        capabilities.coordinates = true;
        record_running_session_with_bridge(
            &state,
            &device_id,
            capabilities.clone(),
            capabilities,
            Some(bridge_snapshot(true)),
        )
        .await;

        let response = handle_interaction(&state, interaction("coords", "owner-1"))
            .await
            .unwrap();
        assert_eq!(response["type"], 5, "the reply is deferred");
        assert_eq!(response["data"]["flags"], EPHEMERAL);
        let pending = state
            .store
            .claim_pending_commands(&device_id, 8)
            .await
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].kind, CommandKind::Coordinates);
        assert_eq!(pending[0].session_id.as_deref(), Some("session-1"));
    }

    #[tokio::test]
    async fn status_shows_bridge_details_but_never_stale_coordinates() {
        let state = test_state_with_coordinates().await;
        let (device_id, _) = paired_device(&state).await;
        let mut capabilities = Capabilities::default();
        capabilities.coordinates = true;
        record_running_session_with_bridge(
            &state,
            &device_id,
            capabilities.clone(),
            capabilities.clone(),
            Some(bridge_snapshot(true)),
        )
        .await;
        let live = handle_interaction(&state, interaction("status", "owner-1"))
            .await
            .unwrap();
        let live_text = live["data"]["embeds"][0].to_string();
        assert!(live_text.contains("100.5"));
        assert!(live_text.contains("minecraft:overworld"));

        // Same session, but the sample is now old. `/status` may still say the
        // mod is connected; it must not print the position as if it were live.
        record_running_session_with_bridge(
            &state,
            &device_id,
            capabilities.clone(),
            capabilities,
            Some(bridge_snapshot(false)),
        )
        .await;
        let stale = handle_interaction(&state, interaction("status", "owner-1"))
            .await
            .unwrap();
        let stale_text = stale["data"]["embeds"][0].to_string();
        assert!(stale_text.contains("STALE"));
        assert!(!stale_text.contains("100.5"));
    }

    #[tokio::test]
    async fn status_distinguishes_a_missing_bridge_from_no_sample() {
        let state = test_state_with_coordinates().await;
        let (device_id, _) = paired_device(&state).await;
        record_running_session(&state, &device_id, Capabilities::default()).await;

        let response = handle_interaction(&state, interaction("status", "owner-1"))
            .await
            .unwrap();
        let text = response["data"]["embeds"][0].to_string();
        assert!(text.contains("not connected"));
    }

    fn chat_capabilities() -> Capabilities {
        let mut capabilities = Capabilities::default();
        capabilities.chat = true;
        capabilities.chat_send = true;
        capabilities.minecraft_command = true;
        capabilities
    }

    fn interaction_with(name: &str, user: &str, options: Value) -> Value {
        json!({
            "type": 2,
            "data": {"name": name, "options": options},
            "user": {"id": user},
            "channel_id": "channel-1",
            "token": "interaction-token",
            "application_id": "1"
        })
    }

    async fn audit_rows(state: &BackendState, requester: &str) -> Vec<crate::store::AuditRow> {
        state
            .store
            .list_audit(&AuditFilter {
                requester: Some(requester.into()),
                device_id: None,
                session_id: None,
                outcome: None,
                since: None,
                limit: 50,
                offset: 0,
            })
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn chat_is_refused_until_the_backend_and_the_device_opt_in() {
        // Backend policy off: a connected bridge is still not exposed.
        let state = test_state().await;
        let (device_id, _) = paired_device(&state).await;
        record_running_session(&state, &device_id, chat_capabilities()).await;
        let response = handle_interaction(&state, interaction("chat", "owner-1"))
            .await
            .unwrap();
        assert_eq!(response["type"], 4);
        assert!(response["data"]["content"]
            .as_str()
            .unwrap()
            .contains("disabled"));
        assert!(
            state
                .store
                .claim_pending_commands(&device_id, 8)
                .await
                .unwrap()
                .is_empty(),
            "a policy refusal must not queue a read"
        );
        let rows = audit_rows(&state, "owner-1").await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].outcome, "denied");
        assert!(!rows[0].authorized);

        // Backend on, but the agent has not enabled chat itself.
        let state = test_state_with_chat().await;
        let (device_id, _) = paired_device(&state).await;
        record_running_session(&state, &device_id, Capabilities::default()).await;
        let response = handle_interaction(&state, interaction("chat", "owner-1"))
            .await
            .unwrap();
        assert!(response["data"]["content"]
            .as_str()
            .unwrap()
            .contains("disabled"));

        // Both device capabilities on, but no mod on this session.
        let state = test_state_with_chat().await;
        let (device_id, _) = paired_device(&state).await;
        record_running_session_with_bridge(
            &state,
            &device_id,
            chat_capabilities(),
            Capabilities::default(),
            None,
        )
        .await;
        let response = handle_interaction(&state, interaction("chat", "owner-1"))
            .await
            .unwrap();
        let text = response["data"]["content"].as_str().unwrap();
        assert!(text.contains("SpiderBridge"));
        assert!(text.contains("No chat was read"));
    }

    #[tokio::test]
    async fn chat_history_queues_a_bounded_read_request_once() {
        let state = test_state_with_chat().await;
        let (device_id, _) = paired_device(&state).await;
        let capabilities = chat_capabilities();
        record_running_session(&state, &device_id, capabilities).await;

        let response = handle_interaction(
            &state,
            interaction_with(
                "chat",
                "owner-1",
                json!([
                    {"name": "limit", "value": 900},
                    {"name": "include_system", "value": false}
                ]),
            ),
        )
        .await
        .unwrap();
        assert_eq!(response["type"], 5, "the read is deferred");
        assert_eq!(response["data"]["flags"], EPHEMERAL);

        let pending = state
            .store
            .claim_pending_commands(&device_id, 8)
            .await
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].kind, CommandKind::ChatLogs);
        assert_eq!(pending[0].session_id.as_deref(), Some("session-1"));
        assert_eq!(pending[0].payload["limit"], 200);
        assert_eq!(pending[0].payload["include_system"], false);

        let limited = handle_interaction(&state, interaction("chat", "owner-1"))
            .await
            .unwrap();
        assert_eq!(limited["type"], 4);
        assert!(limited["data"]["content"]
            .as_str()
            .unwrap()
            .contains("requested recently"));
    }

    #[tokio::test]
    async fn say_refuses_unusable_text_and_queues_a_submission() {
        let state = test_state_with_chat().await;
        let (device_id, _) = paired_device(&state).await;
        let capabilities = chat_capabilities();
        record_running_session(&state, &device_id, capabilities).await;

        let invalid = handle_interaction(
            &state,
            interaction_with(
                "say",
                "owner-1",
                json!([{"name": "message", "value": "hello\nop Alex"}]),
            ),
        )
        .await
        .unwrap();
        assert_eq!(invalid["type"], 4);
        assert!(invalid["data"]["content"]
            .as_str()
            .unwrap()
            .contains("control"));
        assert!(
            state
                .store
                .claim_pending_commands(&device_id, 8)
                .await
                .unwrap()
                .is_empty()
        );

        let accepted = handle_interaction(
            &state,
            interaction_with(
                "say",
                "owner-1",
                json!([{"name": "message", "value": "hello from Discord"}]),
            ),
        )
        .await
        .unwrap();
        assert_eq!(accepted["type"], 5);
        let pending = state
            .store
            .claim_pending_commands(&device_id, 8)
            .await
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].kind, CommandKind::Say);
        assert_eq!(pending[0].payload["message"], "hello from Discord");
        let audit = state
            .store
            .list_audit(&crate::store::AuditFilter {
                requester: Some("owner-1".into()),
                limit: 10,
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(
            audit
                .iter()
                .all(|row| !row.payload_summary.contains("hello from Discord")),
            "a chat body must not be written to the audit table"
        );
    }

    #[tokio::test]
    async fn minecraft_commands_are_scoped_by_category_and_confirmation() {
        // Read-only is off by default, so even `list` is refused.
        let state = test_state_with_chat().await;
        let (device_id, _) = paired_device(&state).await;
        let capabilities = chat_capabilities();
        record_running_session(&state, &device_id, capabilities.clone()).await;
        let refused = handle_interaction(
            &state,
            interaction_with(
                "mc-command",
                "owner-1",
                json!([{"name": "command", "value": "list"}]),
            ),
        )
        .await
        .unwrap();
        assert_eq!(refused["type"], 4);
        assert!(refused["data"]["content"]
            .as_str()
            .unwrap()
            .contains("read-only"));

        // Enabling read-only allows it and submits without confirmation.
        let mut config = base_config();
        config.allow_minecraft_command = true;
        config.command_policy = CommandPolicy {
            read_only: true,
            ..CommandPolicy::default()
        };
        let state = state_with(config).await;
        let (device_id, _) = paired_device(&state).await;
        record_running_session(&state, &device_id, capabilities.clone()).await;
        let allowed = handle_interaction(
            &state,
            interaction_with(
                "mc-command",
                "owner-1",
                json!([{"name": "command", "value": "list"}]),
            ),
        )
        .await
        .unwrap();
        assert_eq!(allowed["type"], 5);
        let pending = state
            .store
            .claim_pending_commands(&device_id, 8)
            .await
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].kind, CommandKind::MinecraftCommand);
        assert_eq!(pending[0].payload["command"], "list");
        assert_eq!(pending[0].payload["confirm"], true);

        // A state-changing command needs confirmation even when allowed.
        let mut config = base_config();
        config.allow_minecraft_command = true;
        config.command_policy = CommandPolicy {
            read_only: true,
            disruptive: true,
            administrative: false,
        };
        let state = state_with(config).await;
        let (device_id, _) = paired_device(&state).await;
        record_running_session(&state, &device_id, capabilities.clone()).await;
        let unconfirmed = handle_interaction(
            &state,
            interaction_with(
                "mc-command",
                "owner-1",
                json!([{"name": "command", "value": "time set day"}]),
            ),
        )
        .await
        .unwrap();
        assert_eq!(unconfirmed["type"], 4);
        assert!(unconfirmed["data"]["content"]
            .as_str()
            .unwrap()
            .contains("not confirmed"));
        assert!(
            state
                .store
                .claim_pending_commands(&device_id, 8)
                .await
                .unwrap()
                .is_empty()
        );
        let rows = audit_rows(&state, "owner-1").await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].outcome, "denied");
        assert!(rows[0].confirmation_required);
        assert!(!rows[0].confirmation_obtained);

        let confirmed = handle_interaction(
            &state,
            interaction_with(
                "mc-command",
                "owner-1",
                json!([
                    {"name": "command", "value": "time set day"},
                    {"name": "confirm", "value": true}
                ]),
            ),
        )
        .await
        .unwrap();
        assert_eq!(confirmed["type"], 5);
        let pending = state
            .store
            .claim_pending_commands(&device_id, 8)
            .await
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].payload["command"], "time set day");
        assert_eq!(pending[0].payload["confirm"], true);

        // Administrative commands stay refused even with confirmation while
        // that independent category switch is off.
        let administrative = handle_interaction(
            &state,
            interaction_with(
                "mc-command",
                "owner-1",
                json!([
                    {"name": "command", "value": "op Alex"},
                    {"name": "confirm", "value": true}
                ]),
            ),
        )
        .await
        .unwrap();
        assert_eq!(administrative["type"], 4);
        assert!(administrative["data"]["content"]
            .as_str()
            .unwrap()
            .contains("administrative"));
    }

    #[tokio::test]
    async fn command_logs_are_owner_scoped_and_clearable() {
        let state = test_state_with_chat().await;
        let (device_id, _) = paired_device(&state).await;
        let capabilities = chat_capabilities();
        record_running_session(&state, &device_id, capabilities).await;

        // One denied request creates an audit row for the owner.
        handle_interaction(
            &state,
            interaction_with(
                "say",
                "owner-1",
                json!([{"name": "message", "value": "bad\nline"}]),
            ),
        )
        .await
        .unwrap();
        assert_eq!(audit_rows(&state, "owner-1").await.len(), 1);

        let owner = handle_interaction(&state, interaction("command-logs", "owner-1"))
            .await
            .unwrap();
        let text = owner["data"]["embeds"][0].to_string();
        assert!(text.contains("denied"));
        assert!(text.contains("say"));

        // A stranger with their own paired device sees nothing from the owner.
        let code = state
            .store
            .create_pairing_code("stranger", 600)
            .await
            .unwrap();
        state
            .store
            .claim_pairing_code(
                &code.code,
                "Other rig",
                "test",
                &Capabilities::default(),
            )
            .await
            .unwrap();
        let stranger = handle_interaction(&state, interaction("command-logs", "stranger"))
            .await
            .unwrap();
        assert!(!stranger.to_string().contains("denied"));
        assert!(stranger["data"]["embeds"][0]
            .to_string()
            .contains("No audit rows"));

        // Clearing needs an explicit confirmation and only removes our rows.
        let unconfirmed = handle_interaction(
            &state,
            interaction_with(
                "command-logs",
                "owner-1",
                json!([{"name": "clear", "value": true}]),
            ),
        )
        .await
        .unwrap();
        assert!(unconfirmed["data"]["content"]
            .as_str()
            .unwrap()
            .contains("confirm:true"));
        assert_eq!(audit_rows(&state, "owner-1").await.len(), 1);

        let cleared = handle_interaction(
            &state,
            interaction_with(
                "command-logs",
                "owner-1",
                json!([
                    {"name": "clear", "value": true},
                    {"name": "confirm", "value": true}
                ]),
            ),
        )
        .await
        .unwrap();
        assert!(cleared["data"]["embeds"][0]
            .to_string()
            .contains("Deleted 1 row"));
        assert!(audit_rows(&state, "owner-1").await.is_empty());
    }

    #[tokio::test]
    async fn command_logs_reject_an_unknown_outcome_filter() {
        let state = test_state_with_chat().await;
        let (device_id, _) = paired_device(&state).await;
        record_running_session(&state, &device_id, chat_capabilities()).await;
        let response = handle_interaction(
            &state,
            interaction_with(
                "command-logs",
                "owner-1",
                json!([{"name": "outcome", "value": "made-up"}]),
            ),
        )
        .await
        .unwrap();
        assert_eq!(response["type"], 4);
        assert!(response["data"]["content"]
            .as_str()
            .unwrap()
            .contains("Unknown outcome filter"));
    }
}
