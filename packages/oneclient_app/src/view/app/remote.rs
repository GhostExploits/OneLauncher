use std::path::Path;

use freya::prelude::*;
use serde::Deserialize;

use crate::components::{Button, Icon, IconType, SpiderSprite, open_folder_button, toggle_controlled};
use crate::hooks::use_link_confirm;
use crate::startup::{
    agent_binary, agent_config_path, agent_data_dir, agent_status_path, login_startup_registered,
    request_agent_stop, running_agent_pid, spawn_agent,
};
use crate::theme::colors;
use crate::ui::border_all_color;

const REMOTE_DOCS: &str = "https://github.com/GhostExploits/OneLauncher/blob/main/docs/REMOTE.md";

#[derive(Debug, Deserialize)]
struct AgentStatus {
    agent_version: String,
    device_id: String,
    device_name: String,
    server_url: String,
    online: bool,
    paired: bool,
    last_heartbeat: Option<String>,
    last_error: Option<String>,
    session_count: usize,
    /// Written by newer agents. Defaulted so an older status file still loads.
    #[serde(default)]
    bridge_enabled: bool,
    #[serde(default)]
    bridge_sessions: usize,
    updated_at: String,
}

#[derive(PartialEq)]
pub struct Remote;

impl Component for Remote {
    fn render(&self) -> impl IntoElement {
        let mut refresh = use_state(|| 0u64);
        let action_message = use_state(String::new);
        let capability_message = use_state(String::new);
        let confirm_link = use_link_confirm();
        let mut remote_docs = confirm_link;
        let _ = refresh.read();

        let status = read_status();
        let config = read_config_summary();
        let binary = agent_binary();
        // A running process is stoppable even while its heartbeat is failing or
        // its status file is missing, so the local kill switch never depends on
        // backend reachability or stale state.
        let running = running_agent_pid().is_some();
        let alive = running && status.as_ref().is_some_and(|status| status.online);
        let paired = config
            .as_ref()
            .is_some_and(|config| config.device_id_present)
            || status.as_ref().is_some_and(|status| status.paired);

        let mut start_button = Button::new().primary().small().text("Start agent");
        if !paired || binary.is_none() || running {
            start_button = start_button.disabled(true);
        }
        let binary_for_start = binary.clone();
        let mut start_message = action_message;
        start_button = start_button.on_press(move |_| match binary_for_start.as_ref() {
            Some(binary) => match spawn_agent(binary) {
                Ok(()) => {
                    start_message.set(
                        "Agent start requested. Refresh in a moment to see its heartbeat.".into(),
                    );
                }
                Err(err) => start_message.set(format!("Could not start the agent: {err}")),
            },
            None => {
                start_message.set("The spider-agent executable is not next to the launcher.".into())
            }
        });

        let mut stop_button = Button::new().ghost().small().text("Stop agent");
        if !running {
            stop_button = stop_button.disabled(true);
        }
        let mut stop_message = action_message;
        stop_button = stop_button.on_press(move |_| match request_agent_stop() {
            Ok(true) => stop_message.set(
                "Clean stop requested. The agent exits after its current request, usually within a few seconds."
                    .into(),
            ),
            Ok(false) => stop_message.set("The agent process is no longer running.".into()),
            Err(err) => stop_message.set(format!("Could not request a clean stop: {err}")),
        });

        let docs = Button::new()
            .ghost()
            .small()
            .on_press(move |_| remote_docs.set(Some(REMOTE_DOCS.to_string())))
            .text("Setup & security guide");

        rect()
            .vertical()
            .width(Size::fill())
            .height(Size::fill())
            .padding(Gaps::new(36., 48., 36., 48.))
            .spacing(18.)
            .overflow(Overflow::Clip)
            .child(
                rect()
                    .horizontal()
                    .width(Size::fill())
                    .cross_align(Alignment::Center)
                    .spacing(18.)
                    .child(SpiderSprite::new(58.))
                    .child(
                        rect()
                            .vertical()
                            .width(Size::flex(1.0))
                            .spacing(3.)
                            .child(
                                label()
                                    .text("Remote sessions")
                                    .font_size(32.)
                                    .font_weight(FontWeight::SEMI_BOLD)
                                    .color(colors::fg_primary()),
                            )
                            .child(
                                label()
                                    .text(
                                        "Monitor your own Minecraft sessions from Discord. Optional, device-scoped, and never required to launch a game.",
                                    )
                                    .font_size(13.)
                                    .color(colors::fg_secondary()),
                            ),
                    )
                    .child(
                        rect()
                            .horizontal()
                            .spacing(8.)
                            .child(
                                Button::new()
                                    .secondary()
                                    .small()
                                    .on_press(move |_| {
                                        let next = *refresh.peek() + 1;
                                        refresh.set(next);
                                    })
                                    .child(Icon::new(IconType::RefreshCw01).size(15.))
                                    .text("Refresh"),
                            )
                            .child(docs),
                    ),
            )
            .child(status_card(status.as_ref(), alive, paired, binary.as_deref()))
            .child(
                rect()
                    .horizontal()
                    .width(Size::fill())
                    .spacing(8.)
                    .child(start_button)
                    .child(stop_button)
                    .maybe(!action_message.read().is_empty(), |row| {
                        row.child(
                            label()
                                .text(action_message.read().clone())
                                .font_size(12.)
                                .color(colors::fg_secondary()),
                        )
                    }),
            )
            .child(capabilities_card(
                config.as_ref(),
                capability_message,
                refresh,
            ))
            .child(
                rect()
                    .horizontal()
                    .width(Size::fill())
                    .cross_align(Alignment::Center)
                    .spacing(10.)
                    .child(
                        label()
                            .text(if !cfg!(windows) {
                                "Login startup is available on Windows only."
                            } else if login_startup_registered() {
                                "Windows login startup: enabled as a current-user Run entry (removable in Settings)."
                            } else {
                                "Windows login startup: disabled."
                            })
                            .font_size(11.)
                            .color(colors::fg_secondary()),
                    )
                    .maybe_child(agent_data_dir().map(|dir| open_folder_button(dir))),
            )
            .child(
                rect()
                    .vertical()
                    .width(Size::fill())
                    .spacing(8.)
                    .padding(Gaps::new_all(18.))
                    .corner_radius(CornerRadius::new_all(12.))
                    .background(colors::page_elevated())
                    .border(border_all_color(1., colors::component_border()))
                    .child(
                        label()
                            .text("How the trust boundary works")
                            .font_size(15.)
                            .font_weight(FontWeight::MEDIUM)
                            .color(colors::fg_primary()),
                    )
                    .child(
                        label()
                            .text(
                                "This launcher only controls the local agent. The Discord bot token lives on the trusted backend and is never requested by or stored on this computer. Pairing issues a separate device credential, and every remote command is checked against the device owner.",
                            )
                            .font_size(12.)
                            .color(colors::fg_secondary()),
                    ),
            )
            .child(
                rect()
                    .vertical()
                    .width(Size::fill())
                    .spacing(4.)
                    .padding(Gaps::new(12., 2., 0., 2.))
                    .child(
                        label()
                            .text(format!(
                                "Agent config: {}",
                                agent_config_path()
                                    .map(|path| path.display().to_string())
                                    .unwrap_or_else(|| "unavailable".into())
                            ))
                            .font_size(11.)
                            .color(colors::fg_secondary()),
                    )
                    .child(
                        label()
                            .text(format!(
                                "Status file: {}",
                                agent_status_path()
                                    .map(|path| path.display().to_string())
                                    .unwrap_or_else(|| "unavailable".into())
                            ))
                            .font_size(11.)
                            .color(colors::fg_secondary()),
                    ),
            )
            .into_element()
    }
}

fn status_card(
    status: Option<&AgentStatus>,
    alive: bool,
    paired: bool,
    binary: Option<&Path>,
) -> impl IntoElement {
    let (title, tone, detail) = match (status, paired) {
        (Some(status), _) if alive => (
            format!("{} · online", status.device_name),
            colors::success(),
            format!(
                "Agent {} · {} running session(s) · SpiderBridge {} · last heartbeat {}",
                status.agent_version,
                status.session_count,
                if status.bridge_enabled {
                    format!("{} connected", status.bridge_sessions)
                } else {
                    "disabled".to_string()
                },
                status.updated_at
            ),
        ),
        (Some(status), _) => (
            format!("{} · offline", status.device_name),
            colors::fg_secondary(),
            status
                .last_error
                .clone()
                .unwrap_or_else(|| format!("Last status file update: {}", status.updated_at)),
        ),
        (None, true) => (
            "Paired, agent not running".to_string(),
            colors::code_warn(),
            "The device has a local credential, but no current heartbeat was found.".into(),
        ),
        (None, false) => (
            "Not paired".to_string(),
            colors::fg_secondary(),
            "Pair this computer from Discord or with `spider-agent pair`. The launcher never asks for the Discord bot token.".into(),
        ),
    };

    rect()
        .vertical()
        .width(Size::fill())
        .spacing(8.)
        .padding(Gaps::new_all(18.))
        .corner_radius(CornerRadius::new_all(12.))
        .background(colors::page_elevated())
        .border(border_all_color(1., colors::component_border()))
        .child(
            rect()
                .horizontal()
                .cross_align(Alignment::Center)
                .spacing(10.)
                .child(
                    rect()
                        .width(Size::px(8.))
                        .height(Size::px(8.))
                        .corner_radius(CornerRadius::new_all(4.))
                        .background(tone),
                )
                .child(
                    label()
                        .text(title)
                        .font_size(17.)
                        .font_weight(FontWeight::MEDIUM)
                        .color(colors::fg_primary()),
                )
                .child(
                    label()
                        .text(if binary.is_some() {
                            "agent binary found"
                        } else {
                            "agent binary not found beside launcher"
                        })
                        .font_size(11.)
                        .color(if binary.is_some() {
                            colors::fg_secondary()
                        } else {
                            colors::code_warn()
                        }),
                ),
        )
        .child(
            label()
                .text(detail)
                .font_size(12.)
                .color(colors::fg_secondary()),
        )
        .maybe_child(status.map(|status| {
            label()
                .text(format!(
                    "Device `{}` · backend {}{}",
                    status.device_id,
                    status.server_url,
                    status
                        .last_heartbeat
                        .as_deref()
                        .map(|heartbeat| format!(" · last heartbeat {heartbeat}"))
                        .unwrap_or_default()
                ))
                .font_size(11.)
                .color(colors::fg_secondary())
                .into_element()
        }))
}

/// Local switches that the device owner controls. These are only half of the
/// gate: the trusted backend has independent switches, and Discord still checks
/// that the requester owns this device. Showing both halves matters, because a
/// switch that looks on here can still be refused remotely.
fn capabilities_card(
    config: Option<&ConfigSummary>,
    message: State<String>,
    refresh: State<u64>,
) -> impl IntoElement {
    let available = config.is_some();
    let summary = config.cloned().unwrap_or_default();
    let bridge = available && summary.bridge;
    let chat = bridge && summary.chat;
    let chat_send = bridge && summary.chat_send;
    let commands = bridge && summary.minecraft_command;

    let chat_detail = if chat {
        format!(
            "Enabled. While the agent is connected, the mod keeps at most {} received messages for up to {} seconds in memory. The buffer is cleared when the session ends, and chat is never scraped from latest.log.",
            summary.chat_max_messages, summary.chat_retention_seconds
        )
    } else {
        "Disabled. The mod is not told to collect chat, so Discord receives no history.".to_string()
    };
    let policy_note = |text: &str, active: bool| {
        if active {
            format!("{text} Effective: on.")
        } else {
            format!("{text} Effective: off until Minecraft commands are enabled above.")
        }
    };

    rect()
        .vertical()
        .width(Size::fill())
        .spacing(12.)
        .padding(Gaps::new_all(18.))
        .corner_radius(CornerRadius::new_all(12.))
        .background(colors::page_elevated())
        .border(border_all_color(1., colors::component_border()))
        .child(
            label()
                .text("Local owner controls")
                .font_size(15.)
                .font_weight(FontWeight::MEDIUM)
                .color(colors::fg_primary()),
        )
        .child(
            label()
                .text(
                    "These switches are stored in this computer's agent.json. Changes apply after the agent restarts. Every remote action still needs the backend's own switch, and only this device's Discord owner can request it.",
                )
                .font_size(12.)
                .color(colors::fg_secondary()),
        )
        .maybe_child((!available).then(|| {
            label()
                .text(
                    "agent.json is not available yet. Pair this computer first; toggles stay disabled so the launcher never writes a partial config.",
                )
                .font_size(12.)
                .color(colors::code_warn())
                .into_element()
        }))
        .child(setting_row(
            "SpiderBridge listener",
            "Loopback-only channel between the agent and the SpiderBridge mod. It never contacts Discord or the backend directly.".into(),
            summary.bridge,
            !available,
            ConfigKey::Bridge,
            message,
            refresh,
        ))
        .child(setting_row(
            "Chat history (/chat, /chatlogs)",
            chat_detail,
            summary.chat,
            !available,
            ConfigKey::Chat,
            message,
            refresh,
        ))
        .child(setting_row(
            "Send chat (/say)",
            if chat_send {
                "Submits a line through this client's normal chat. The client accepting it is not proof that other players received it. Effective: on.".to_string()
            } else {
                "Submits a line through this client's normal chat, once the listener and this switch are both on. Effective: off.".to_string()
            },
            summary.chat_send,
            !available,
            ConfigKey::ChatSend,
            message,
            refresh,
        ))
        .child(setting_row(
            "Minecraft commands (/mc-command)",
            "Submits commands your player already has permission to run. Server permissions and moderation still apply, and execution cannot be verified from here.".into(),
            summary.minecraft_command,
            !available,
            ConfigKey::MinecraftCommand,
            message,
            refresh,
        ))
        .child(setting_row(
            "  Category: read-only",
            policy_note(
                "Queries such as list, seed, or time query that do not change state.",
                summary.read_only && commands,
            ),
            summary.read_only,
            !available || !summary.minecraft_command,
            ConfigKey::ReadOnly,
            message,
            refresh,
        ))
        .child(setting_row(
            "  Category: disruptive",
            policy_note(
                "Reversible changes such as time set, weather clear, or gamemode. Discord also requires confirm:true when confirmation is enabled.",
                summary.disruptive && commands,
            ),
            summary.disruptive,
            !available || !summary.minecraft_command,
            ConfigKey::Disruptive,
            message,
            refresh,
        ))
        .child(setting_row(
            "  Category: administrative",
            policy_note(
                "Administration and destructive operations such as op, deop, kick, ban, or kill. Leave this off unless you explicitly need it.",
                summary.administrative && commands,
            ),
            summary.administrative,
            !available || !summary.minecraft_command,
            ConfigKey::Administrative,
            message,
            refresh,
        ))
        .maybe_child((!message.read().is_empty()).then(|| {
            label()
                .text(message.read().clone())
                .font_size(11.)
                .color(colors::fg_secondary())
                .into_element()
        }))
}

#[derive(Clone, Copy)]
enum ConfigKey {
    Bridge,
    Chat,
    ChatSend,
    MinecraftCommand,
    ReadOnly,
    Disruptive,
    Administrative,
}

impl ConfigKey {
    fn field_name(self) -> &'static str {
        match self {
            ConfigKey::Bridge => "SpiderBridge listener",
            ConfigKey::Chat => "chat history",
            ConfigKey::ChatSend => "send chat",
            ConfigKey::MinecraftCommand => "Minecraft commands",
            ConfigKey::ReadOnly => "read-only commands",
            ConfigKey::Disruptive => "disruptive commands",
            ConfigKey::Administrative => "administrative commands",
        }
    }
}

fn setting_row(
    title: &'static str,
    detail: String,
    on: bool,
    disabled: bool,
    key: ConfigKey,
    message: State<String>,
    refresh: State<u64>,
) -> impl IntoElement {
    let mut message = message;
    let mut refresh = refresh;
    let current = on;
    let toggle = move |()| {
        let next = !current;
        match update_config(|config| set_capability(config, key, next)) {
            Ok(()) => message.set(format!(
                "Saved {}. Restart the agent so it reloads agent.json.",
                key.field_name()
            )),
            Err(err) => message.set(format!("Could not save {}: {err}", key.field_name())),
        }
        let next_refresh = *refresh.peek() + 1;
        refresh.set(next_refresh);
    };

    rect()
        .horizontal()
        .width(Size::fill())
        .cross_align(Alignment::Center)
        .spacing(12.)
        .child(
            rect()
                .vertical()
                .width(Size::flex(1.0))
                .spacing(2.)
                .child(
                    label()
                        .text(title)
                        .font_size(13.)
                        .font_weight(FontWeight::MEDIUM)
                        .color(colors::fg_primary()),
                )
                .child(
                    label()
                        .text(detail)
                        .font_size(11.)
                        .color(colors::fg_secondary()),
                ),
        )
        .child(toggle_controlled(on, toggle.into()).disabled(disabled))
}

/// Rewrites only the allow flags. Unknown fields and the protected device
/// credential are preserved; the token is never read into UI state or logged.
fn update_config(update: impl FnOnce(&mut serde_json::Value)) -> Result<(), String> {
    let path = agent_config_path()
        .ok_or_else(|| "the agent config location is unavailable".to_string())?;
    let text = std::fs::read_to_string(&path)
        .map_err(|err| format!("could not read {}: {err}", path.display()))?;
    let mut value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|err| format!("{} is not valid JSON: {err}", path.display()))?;
    if !value.is_object() {
        return Err(format!("{} does not contain a JSON object", path.display()));
    }
    update(&mut value);
    let encoded = serde_json::to_string_pretty(&value)
        .map_err(|err| format!("could not encode the agent config: {err}"))?;

    // Write a sibling temp file, tighten it, then replace atomically. On Unix
    // the replacement keeps a user-only mode; Windows protects the credential
    // itself with DPAPI.
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, encoded)
        .map_err(|err| format!("could not write {}: {err}", temp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if let Err(err) = std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600)) {
            let _ = std::fs::remove_file(&temp);
            return Err(format!("could not protect {}: {err}", temp.display()));
        }
    }
    if let Err(err) = std::fs::rename(&temp, &path) {
        let _ = std::fs::remove_file(&temp);
        return Err(format!("could not replace {}: {err}", path.display()));
    }
    Ok(())
}

fn set_capability(config: &mut serde_json::Value, key: ConfigKey, on: bool) {
    let Some(object) = config.as_object_mut() else {
        return;
    };
    let boolean = serde_json::Value::Bool(on);
    match key {
        ConfigKey::Bridge => {
            object.insert("allow_bridge".into(), boolean);
        }
        ConfigKey::Chat => {
            object.insert("allow_chat".into(), boolean);
        }
        ConfigKey::ChatSend => {
            object.insert("allow_chat_send".into(), boolean);
        }
        ConfigKey::MinecraftCommand => {
            object.insert("allow_minecraft_command".into(), boolean);
        }
        ConfigKey::ReadOnly | ConfigKey::Disruptive | ConfigKey::Administrative => {
            let policy = object
                .entry("command_policy")
                .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
            let Some(policy) = policy.as_object_mut() else {
                return;
            };
            let field = match key {
                ConfigKey::ReadOnly => "read_only",
                ConfigKey::Disruptive => "disruptive",
                ConfigKey::Administrative => "administrative",
                _ => return,
            };
            policy.insert(field.into(), boolean);
        }
    }
}

#[derive(Clone, Default)]
struct ConfigSummary {
    device_id_present: bool,
    bridge: bool,
    chat: bool,
    chat_send: bool,
    minecraft_command: bool,
    read_only: bool,
    disruptive: bool,
    administrative: bool,
    chat_retention_seconds: u32,
    chat_max_messages: u32,
}

fn read_status() -> Option<AgentStatus> {
    let path = agent_status_path()?;
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn read_config_summary() -> Option<ConfigSummary> {
    let path = agent_config_path()?;
    let text = std::fs::read_to_string(path).ok()?;
    // Parse only the fields the UI needs. The credential is never displayed
    // and is not copied into any UI state.
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let flag = |name: &str, fallback: bool| {
        value
            .get(name)
            .and_then(|value| value.as_bool())
            .unwrap_or(fallback)
    };
    let policy = value.get("command_policy");
    let policy_flag = |name: &str| {
        policy
            .and_then(|policy| policy.get(name))
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
    };
    let number = |name: &str, fallback: u32| {
        value
            .get(name)
            .and_then(|value| value.as_u64())
            .map(|value| value.min(u64::from(u32::MAX)) as u32)
            .unwrap_or(fallback)
    };
    Some(ConfigSummary {
        device_id_present: value
            .get("device_id")
            .and_then(|value| value.as_str())
            .is_some_and(|value| !value.is_empty()),
        bridge: flag("allow_bridge", true),
        chat: flag("allow_chat", false),
        chat_send: flag("allow_chat_send", false),
        minecraft_command: flag("allow_minecraft_command", false),
        read_only: policy_flag("read_only"),
        disruptive: policy_flag("disruptive"),
        administrative: policy_flag("administrative"),
        chat_retention_seconds: number("chat_retention_seconds", 300),
        chat_max_messages: number("chat_max_messages", 200),
    })
}
