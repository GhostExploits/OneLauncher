use anyhow::Context;
use spider_remote::backend::{self, BackendConfig};
use spider_remote::store::Store;

#[actix_web::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "spider_remote=info,actix_web=info".into()),
        )
        .with_target(false)
        .init();

    if let Err(err) = run_cli().await {
        eprintln!("spider-remote: {err:#}");
        std::process::exit(1);
    }
}

async fn run_cli() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("serve") | None => serve().await,
        Some("pair") => pair(args).await,
        Some("register") => register().await,
        Some("doctor") => doctor(),
        Some("version") | Some("--version") | Some("-V") => {
            println!("spider-remote {}", spider_remote::AGENT_VERSION);
            Ok(())
        }
        Some("help") | Some("--help") | Some("-h") => {
            print_help();
            Ok(())
        }
        Some(other) => anyhow::bail!("unknown command `{other}` (try `spider-remote help`)"),
    }
}

fn print_help() {
    println!(
        "spider-remote {}\n\
         \n\
         USAGE:\n\
           spider-remote serve                 Run the trusted backend\n\
           spider-remote register              Register slash commands with Discord\n\
           spider-remote pair --owner <id>     Create a single-use device pairing code\n\
           spider-remote doctor                Validate environment without printing secrets\n\
         \n\
         Required environment for serve/register:\n\
           DISCORD_BOT_TOKEN, DISCORD_PUBLIC_KEY, DISCORD_APPLICATION_ID\n\
         Optional:\n\
           SPIDER_OWNER_DISCORD_IDS, SPIDER_ALLOWED_GUILD_IDS, SPIDER_ALERT_CHANNEL_ID,\n\
           SPIDER_ALLOW_LAUNCH=1, SPIDER_ALLOW_STOP=1, SPIDER_BIND, SPIDER_DATABASE_URL,\n\
           SPIDER_WEBSITE_API_TOKEN, DISCORD_GUILD_ID (guild-scoped command registration)",
        spider_remote::AGENT_VERSION
    );
}

async fn serve() -> anyhow::Result<()> {
    let config = BackendConfig::from_env().map_err(anyhow::Error::msg)?;
    backend::serve(config).await
}

async fn register() -> anyhow::Result<()> {
    let config = BackendConfig::from_env().map_err(anyhow::Error::msg)?;
    spider_remote::discord::register_commands(&config).await
}

async fn pair(mut args: impl Iterator<Item = String>) -> anyhow::Result<()> {
    let mut owner = None;
    let mut ttl = 600i64;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--owner" => owner = args.next(),
            "--ttl" => {
                ttl = args
                    .next()
                    .context("--ttl needs seconds")?
                    .parse()
                    .context("--ttl must be an integer")?
            }
            "--help" | "-h" => {
                print_help();
                return Ok(());
            }
            other => anyhow::bail!("unknown option `{other}`"),
        }
    }
    let owner = owner.context("--owner <discord user id> is required")?;
    if !owner.chars().all(|c| c.is_ascii_digit()) {
        anyhow::bail!("owner must be a Discord numeric user id");
    }
    let database =
        std::env::var("SPIDER_DATABASE_URL").unwrap_or_else(|_| "sqlite://spider_remote.db".into());
    let store = Store::open(&database).await?;
    let code = store.create_pairing_code(&owner, ttl).await?;
    println!("SpiderClient pairing code (single use):");
    println!("  {}", code.code);
    println!("Owner Discord ID: {owner}");
    println!("Expires: {}", code.expires_at.to_rfc3339());
    println!("This code is shown once. It is not the Discord bot token.");
    Ok(())
}

fn doctor() -> anyhow::Result<()> {
    let config = BackendConfig::from_env().map_err(anyhow::Error::msg)?;
    let summary = serde_json::to_string_pretty(&config.public_summary())?;
    println!("{summary}");
    println!("Secret presence checks passed; no secret values were printed.");
    Ok(())
}
