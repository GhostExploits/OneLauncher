<div align="center">

<img src="packages/oneclient_app/assets/spider-mark.svg" width="88" alt="SpiderClient spider mark" />

# SpiderClient

**A focused Minecraft launcher for real sessions.**

</div>

SpiderClient is Zach's fork of [OneLauncher](https://github.com/Polyfrost/OneLauncher),
built for predictable installs, isolated mod profiles, useful diagnostics, and
optional remote monitoring of your own Minecraft sessions.

The launcher remains fully usable without a Discord backend, an account on a
website, or the background agent. Remote access is opt-in and device-scoped.

## Project credits

- **Zach** — project owner and client development.
- **Faris** — website development and collaboration.

SpiderClient builds on the work of **Polyfrost and OneLauncher contributors**.
The upstream copyright notices, GPL-3.0-only licensing, and third-party notices
remain in effect; see [ATTRIBUTION.md](./ATTRIBUTION.md). The SpiderClient
spider mark and launcher artwork are original additions created for this fork.

## What is in the workspace

The mature launcher packages keep their upstream names for compatibility:

- `oneclient_app` — Freya desktop application.
- `oneclient_auth` — Microsoft, Ely.by, custom, and offline account flows.
- `oneclient_mc` — Minecraft manifests, installs, arguments, and profiles.
- `oneclient_core` — launch orchestration, clusters, Java, logs, recovery.
- `oneclient_content` — mods, modpacks, resource packs, shaders, dependencies.
- `oneclient_db` — SQLite persistence and migrations.
- `oneclient_discord` — existing local Rich Presence integration.
- `spider_remote` — SpiderClient Agent, remote protocol, and Discord backend.

Renaming the internal packages would break migrations, import formats, updater
paths, and user data for no user-visible benefit. Product-facing identity is
SpiderClient; internal package and protocol identifiers stay compatible.

## Build and run

Requirements: **Rust 1.97 or newer** (the workspace is tested with the current
stable toolchain), a C++ toolchain suitable for Skia on Windows, and the normal
Freya platform dependencies on Linux/macOS.

```sh
cargo run -p oneclient_app
cargo test -p oneclient_app
```

Release build:

```sh
cargo build --release -p oneclient_app
```

Packaging uses [cargo-packager](https://github.com/crabnebula-dev/cargo-packager);
configuration lives in `packages/oneclient_app/Cargo.toml`:

```sh
cargo packager --release -p oneclient_app --formats nsis
```

## Remote monitoring

See [docs/REMOTE.md](./docs/REMOTE.md) for the security model, backend
deployment, device pairing, Discord command registration, and the exact
capabilities supported by the agent.

The short version:

1. Run the trusted backend on a server, with `DISCORD_BOT_TOKEN` and
   `DISCORD_PUBLIC_KEY` supplied by the host's secret manager.
2. Register slash commands once with `spider-remote register`.
3. Create a one-time pairing code for your Discord user ID.
4. Run `spider-agent pair` on the gaming PC, then `spider-agent run` or use
   **Remote → Start agent** in the launcher.
5. The bot token is **never** shipped to, requested by, or stored in the
   desktop client.

## Release infrastructure

Automatic updates and the changelog intentionally point at this fork's release
channel (`GhostExploits/OneLauncher`), not the upstream OneClient channel.
Before publishing a SpiderClient installer, the existing release workflow must
publish a signed `latest.json` and `changelog.json` with the matching minisign
key. Until that exists, update checks fail harmlessly instead of installing an
upstream OneClient build over SpiderClient. Do not point these constants back at
the upstream channel in a branded build.

## Licensing

The repository is licensed under **GPL-3.0-only**. See [LICENSE](./LICENSE) and
[ATTRIBUTION.md](./ATTRIBUTION.md). Some upstream branding assets are
all-rights-reserved by Polyfrost and are not reused by SpiderClient's new
artwork.

## Contributing

Keep changes focused, run `cargo fmt` and the relevant tests, and never commit
tokens, account files, or private device credentials. See
[CONTRIBUTING.md](./CONTRIBUTING.md).
