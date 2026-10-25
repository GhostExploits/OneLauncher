# Contributing to SpiderClient

SpiderClient is a GPL-3.0-only fork of OneLauncher. Contributions are welcome
when they keep the launcher reliable, secure, and honest about what it can do.

## Branch and review rules

- The active development branch for the desktop/client work is
  `main-2-(1st-ai)`.
- Do not force-push, rewrite, or discard another contributor's work.
- Workstreams that use a separate branch (for example `main-2-(2nd-ai)`) must
  not be merged or rebased without the owner's explicit approval.
- Do not commit, push, publish releases, or change signing/updater secrets
  without authorization.

## Before opening a change

1. Inspect the current implementation; do not add a second copy of an existing
   feature.
2. Keep internal package names (`oneclient_*`) and existing user-data paths
   stable unless a migration is included and tested.
3. Preserve upstream copyright, attribution, and the restricted Polyfrost
   branding rules in [ATTRIBUTION.md](./ATTRIBUTION.md).
4. Do not add placeholder toggles or commands that return invented data. If an
   integration is unavailable, surface a truthful explanation.

## Build and test

Rust 1.97 or newer is required.

```sh
cargo check -p oneclient_app
cargo test -p oneclient_app --lib
cargo test -p spider_remote
cargo build -p spider_remote --bins
```

Format the code you touched:

```sh
cargo fmt -p oneclient_app
cargo fmt -p spider_remote
```

Do not reformat unrelated files in the same change.

## Remote and security changes

Read [docs/REMOTE.md](./docs/REMOTE.md) before touching the agent, backend, or
protocol.

- Never commit `DISCORD_BOT_TOKEN`, a `.env` with real values, a device bearer
  token, a pairing code, account JSON, or a production database.
- `.env.example` contains placeholder names only.
- Add tests for unauthorized access, token handling, redaction, and command
  expiry whenever those paths change.
- The desktop client must not depend on the backend being online.
- No remote command may become a generic shell, arbitrary file access, or
  unrestricted process-control endpoint.

If you discover a vulnerability, do not open a public issue with exploit
details. Report it privately to the project owner.

## UI expectations

SpiderClient uses Freya, not a web frontend. Follow the existing components and
state model. Keep the palette monochrome/charcoal with the restrained steel
accent, preserve keyboard focus, and provide loading, empty, error, disabled,
success, and offline states. Original artwork is preferred over upstream
branding assets.

## Licensing

By contributing, you agree that your contribution is licensed under
GPL-3.0-only and can be distributed as part of SpiderClient. Retain any
third-party notices that apply to code you adapt.
