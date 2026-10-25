//! Local agent lifecycle and opt-in Windows login startup.
//!
//! Everything here is ordinary, discoverable Windows behaviour: an `HKCU\...\
//! Run` value that points at this launcher with `--agent-only`. It needs no
//! administrator rights, survives a normal sign-out, and is removed by turning
//! the setting off. The agent itself stays an ordinary visible process.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use sysinfo::{ProcessesToUpdate, System};

pub const LOGIN_VALUE_NAME: &str = "SpiderClientAgent";
#[cfg(windows)]
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/// The agent executable placed next to the launcher by the packaging step.
#[must_use]
pub fn agent_binary() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    for name in ["spider-agent.exe", "spider-agent"] {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[must_use]
pub fn running_agent_pid() -> Option<u32> {
    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::All, true);
    sys.processes().iter().find_map(|(pid, process)| {
        process
            .name()
            .to_string_lossy()
            .contains("spider-agent")
            .then(|| pid.as_u32())
    })
}

/// Spawn the agent as a background process. The agent's own loopback port guard
/// rejects a duplicate, so a repeated request is harmless.
pub fn spawn_agent(binary: &Path) -> std::io::Result<()> {
    let mut command = Command::new(binary);
    command
        .arg("run")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Keep the background agent out of the user's face; it is still a
        // normal, visible process in Task Manager.
        command.creation_flags(0x0800_0000);
    }
    command.spawn().map(|_| ())
}

/// Start the agent only when it is paired and not already running.
pub fn start_agent_if_needed() -> Result<bool, String> {
    if running_agent_pid().is_some() {
        return Ok(false);
    }
    if !agent_configured() {
        return Err("the agent has not been paired on this computer".into());
    }
    let binary = agent_binary()
        .ok_or_else(|| "the spider-agent executable is not next to the launcher".to_string())?;
    spawn_agent(&binary).map_err(|err| format!("could not start the agent: {err}"))?;
    Ok(true)
}

/// Read only the non-secret fields needed to decide whether pairing exists.
#[must_use]
pub fn agent_configured() -> bool {
    let Some(path) = agent_config_path() else {
        return false;
    };
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|value| {
            value
                .get("device_id")
                .and_then(|id| id.as_str())
                .map(|id| !id.trim().is_empty())
        })
        .unwrap_or(false)
}

/// Ask a running agent to exit cleanly. This works even when the status file is
/// stale because it targets the local process directly.
pub fn request_agent_stop() -> std::io::Result<bool> {
    if running_agent_pid().is_none() {
        return Ok(false);
    }
    let path = agent_stop_path()
        .ok_or_else(|| std::io::Error::other("the operating system has no local data directory"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, b"stop")?;
    Ok(true)
}

#[must_use]
pub fn agent_config_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("org", "SpiderClient", "SpiderClient")
        .map(|dirs| dirs.config_dir().join("agent.json"))
}

#[must_use]
pub fn agent_status_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("org", "SpiderClient", "SpiderClient")
        .map(|dirs| dirs.data_local_dir().join("agent-status.json"))
}

#[must_use]
pub fn agent_data_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("org", "SpiderClient", "SpiderClient")
        .map(|dirs| dirs.data_local_dir().to_path_buf())
}

fn agent_stop_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("org", "SpiderClient", "SpiderClient")
        .map(|dirs| dirs.data_local_dir().join("agent.stop"))
}

/// The exact command Windows will run at sign-in.
#[must_use]
pub fn login_startup_command() -> Option<String> {
    Some(agent_only_command(&std::env::current_exe().ok()?))
}

/// Quote the launcher path as a single argument before appending the flag. The
/// path commonly contains spaces (`Program Files`, a user profile name), so an
/// unquoted command would start the wrong executable or pass the path pieces as
/// arguments.
fn agent_only_command(exe: &Path) -> String {
    format!("\"{}\" --agent-only", exe.display())
}

/// Create or remove the login startup entry. The entry points at the launcher
/// with `--agent-only`, so it starts the agent without opening the window.
#[cfg(windows)]
pub fn apply_login_startup(enabled: bool) -> Result<(), String> {
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
        RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegSetValueExW,
    };
    use windows::core::PCWSTR;

    let key_path = wide(RUN_KEY);
    let value_name = wide(LOGIN_VALUE_NAME);
    let mut key = HKEY::default();
    // SAFETY: all pointers refer to live local buffers for the duration of the
    // calls, and the key handle is closed below.
    unsafe {
        let status = RegCreateKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(key_path.as_ptr()),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE | KEY_QUERY_VALUE,
            None,
            &mut key,
            None,
        );
        if status != ERROR_SUCCESS {
            return Err(format!(
                "could not open the current user's startup registry key (Windows error {})",
                status.0
            ));
        }

        let result = if enabled {
            let command = login_startup_command()
                .ok_or_else(|| "could not determine the launcher path".to_string())?;
            let mut utf16: Vec<u16> = command.encode_utf16().collect();
            utf16.push(0);
            let bytes = std::slice::from_raw_parts(utf16.as_ptr().cast::<u8>(), utf16.len() * 2);
            let status =
                RegSetValueExW(key, PCWSTR(value_name.as_ptr()), None, REG_SZ, Some(bytes));
            if status == ERROR_SUCCESS {
                Ok(())
            } else {
                Err(format!(
                    "could not write the startup entry (Windows error {})",
                    status.0
                ))
            }
        } else {
            let status = RegDeleteValueW(key, PCWSTR(value_name.as_ptr()));
            // Already absent is the desired state.
            if status == ERROR_SUCCESS || status.0 == 2 {
                Ok(())
            } else {
                Err(format!(
                    "could not remove the startup entry (Windows error {})",
                    status.0
                ))
            }
        };
        let _ = RegCloseKey(key);
        result
    }
}

#[cfg(not(windows))]
pub fn apply_login_startup(_enabled: bool) -> Result<(), String> {
    Err("Windows login startup is only implemented on Windows".into())
}

/// Whether the entry currently exists, used to keep the UI truthful.
#[cfg(windows)]
pub fn login_startup_registered() -> bool {
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, REG_OPTION_NON_VOLATILE, RegCloseKey,
        RegCreateKeyExW, RegQueryValueExW,
    };
    use windows::core::PCWSTR;

    let key_path = wide(RUN_KEY);
    let value_name = wide(LOGIN_VALUE_NAME);
    let mut key = HKEY::default();
    unsafe {
        let status = RegCreateKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(key_path.as_ptr()),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_QUERY_VALUE,
            None,
            &mut key,
            None,
        );
        if status != ERROR_SUCCESS {
            return false;
        }
        let mut size = 0u32;
        let status = RegQueryValueExW(
            key,
            PCWSTR(value_name.as_ptr()),
            None,
            None,
            None,
            Some(&mut size),
        );
        let _ = RegCloseKey(key);
        status == ERROR_SUCCESS && size > 0
    }
}

#[cfg(not(windows))]
pub fn login_startup_registered() -> bool {
    false
}

#[cfg(windows)]
fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_login_command_is_quoted_and_agent_only() {
        let command = login_startup_command().unwrap();
        assert!(command.starts_with('"'));
        assert!(command.ends_with("--agent-only"));
        assert!(!command.contains("DISCORD_BOT_TOKEN"));
        assert!(!command.to_ascii_lowercase().contains("token"));
    }

    #[test]
    fn a_launcher_path_with_spaces_stays_one_argument() {
        let command = agent_only_command(Path::new(
            r"C:\Program Files\Spider Client\oneclient_app.exe",
        ));
        assert_eq!(
            command,
            r#""C:\Program Files\Spider Client\oneclient_app.exe" --agent-only"#
        );
    }

    #[test]
    fn the_stop_request_is_scoped_to_the_local_data_directory() {
        let path = agent_stop_path().unwrap();
        assert_eq!(path.file_name().unwrap(), "agent.stop");
        assert!(
            path.components()
                .any(|component| component.as_os_str() == "SpiderClient"),
            "the stop marker must stay inside the SpiderClient data directory: {}",
            path.display()
        );
    }

    #[test]
    fn agent_config_is_read_from_the_documented_location() {
        let path = agent_config_path().unwrap();
        assert_eq!(path.file_name().unwrap(), "agent.json");
    }
}
