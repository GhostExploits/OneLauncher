use std::path::PathBuf;

use anyhow::Context;
use spider_remote::config;
use spider_remote::protocol::PROTOCOL_VERSION;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "spider_remote=info".into()),
        )
        .with_target(false)
        .init();

    if let Err(err) = run_cli().await {
        eprintln!("spider-agent: {err:#}");
        std::process::exit(1);
    }
}

async fn run_cli() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("pair") => pair(args).await,
        Some("run") => run(args).await,
        Some("status") => status(args),
        Some("unpair") => unpair(args),
        Some("config-path") => {
            println!("{}", config::config_path()?.display());
            Ok(())
        }
        Some("version") | Some("--version") | Some("-V") => {
            println!("spider-agent {}", spider_remote::AGENT_VERSION);
            println!("protocol {PROTOCOL_VERSION}");
            Ok(())
        }
        Some("help") | Some("--help") | Some("-h") | None => {
            print_help();
            Ok(())
        }
        Some(other) => anyhow::bail!("unknown command `{other}` (try `spider-agent help`)"),
    }
}

fn print_help() {
    println!(
        "spider-agent {}\n\
         \n\
         USAGE:\n\
           spider-agent pair --server <https://backend> --code <code> --name <device name> [--launcher <path>]\n\
           spider-agent run [--config <path>]\n\
           spider-agent status\n\
           spider-agent unpair\n\
           spider-agent config-path\n\
         \n\
         Pairing stores a per-device bearer token under the current user's config\n\
         directory (DPAPI-protected on Windows, mode 0600 on Unix). The Discord bot\n\
         token is never requested or stored by the agent.",
        spider_remote::AGENT_VERSION
    );
}

async fn pair(mut args: impl Iterator<Item = String>) -> anyhow::Result<()> {
    let mut server = None;
    let mut code = None;
    let mut name = None;
    let mut launcher = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--server" => server = args.next(),
            "--code" => code = args.next(),
            "--name" => name = args.next(),
            "--launcher" => launcher = args.next().map(PathBuf::from),
            "--help" | "-h" => {
                print_help();
                return Ok(());
            }
            other => anyhow::bail!("unknown option `{other}`"),
        }
    }
    let server = server.context("--server is required (https://...)")?;
    let code = code.context("--code is required")?;
    let name = name.unwrap_or_else(default_device_name);
    let launcher = launcher.or_else(find_launcher_sibling);
    let path = spider_remote::agent::pair(server, code, name, launcher).await?;
    println!("Paired successfully. Agent config: {}", path.display());
    println!("Run `spider-agent run` to start monitoring.");
    println!("Launch and stop capabilities are disabled until you opt in in the config file.");
    Ok(())
}

async fn run(mut args: impl Iterator<Item = String>) -> anyhow::Result<()> {
    let mut path = config::config_path()?;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => {
                path = PathBuf::from(args.next().context("--config needs a path")?);
            }
            "--help" | "-h" => {
                print_help();
                return Ok(());
            }
            other => anyhow::bail!("unknown option `{other}`"),
        }
    }
    let config = config::load(&path)?;
    // Rewrite through the current serializer so a legacy plaintext credential
    // is upgraded to the platform's protected form. A read-only config is not
    // fatal; the loaded credential still works for this run.
    if let Err(err) = config::save(&config) {
        tracing::warn!(error = %err, "could not refresh the agent config format");
    }
    println!(
        "SpiderClient Agent starting for `{}` -> {}",
        config.device_name, config.server_url
    );
    spider_remote::agent::run(config).await?;
    Ok(())
}

fn status(mut args: impl Iterator<Item = String>) -> anyhow::Result<()> {
    if args.next().is_some() {
        anyhow::bail!("status takes no arguments");
    }
    let config_path = config::config_path()?;
    let status_path = config::status_path()?;
    println!("Config: {}", config_path.display());
    if !config_path.exists() {
        println!("State: not paired");
        return Ok(());
    }
    let config = config::load(&config_path)?;
    println!("Device: {} ({})", config.device_name, config.device_id);
    println!("Server: {}", config.server_url);
    if status_path.exists() {
        let text = std::fs::read_to_string(&status_path)?;
        let value: serde_json::Value = serde_json::from_str(&text)?;
        println!(
            "Online: {}",
            value
                .get("online")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        );
        if let Some(error) = value.get("last_error").and_then(|v| v.as_str()) {
            println!("Last error: {error}");
        }
        println!("Status file: {}", status_path.display());
    } else {
        println!("State: paired, agent has not run yet");
    }
    Ok(())
}

fn unpair(mut args: impl Iterator<Item = String>) -> anyhow::Result<()> {
    if args.next().is_some() {
        anyhow::bail!("unpair takes no arguments");
    }
    let path = config::config_path()?;
    if !path.exists() {
        println!("The agent is not paired.");
        return Ok(());
    }
    std::fs::remove_file(&path).with_context(|| format!("could not remove {}", path.display()))?;
    println!("Removed the local agent credential. Stop any running agent process.");
    println!("Also revoke the device from Discord with `/unpair` if the backend is still running.");
    Ok(())
}

fn default_device_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "Gaming PC".to_string())
}

fn find_launcher_sibling() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    for name in ["oneclient_app.exe", "SpiderClient.exe", "oneclient_app"] {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}
