# Security Policy

## Reporting

Report suspected vulnerabilities privately to the SpiderClient owner. Do not put
tokens, device credentials, account files, or exploit details in a public issue,
pull request, screenshot, or Discord channel.

## Secrets

These must never be committed, pasted into support messages, or shipped inside a
desktop build:

- `DISCORD_BOT_TOKEN`
- Discord application private secrets
- Microsoft/Minecraft access or refresh tokens
- SpiderClient device tokens
- pairing codes
- production database files
- private signing keys

The backend reads `DISCORD_BOT_TOKEN` from its host environment. The desktop
client never asks for it. `.env` files are ignored by Git; `.env.example`
contains names and placeholders only.

If a Discord bot token is exposed, revoke and regenerate it in the Discord
Developer Portal immediately. Rotating the token invalidates the leaked value
even if it was copied elsewhere. Then review the backend audit log for unexpected
pairings or commands.

## Remote access boundary

The remote stack is optional. See [docs/REMOTE.md](./docs/REMOTE.md) for:

- the device pairing and revocation flow,
- the per-device bearer credential,
- Discord Ed25519 interaction verification and replay window,
- command confirmation requirements,
- redaction and bounded log handling,
- the supported/unsupported feature matrix.

The agent only makes outbound HTTPS requests. It does not open an inbound port on
the gaming PC, hide from Task Manager, or provide a generic shell.

## Before deploying the backend

- Terminate TLS at a maintained reverse proxy in front of `SPIDER_BIND`.
- Keep the bot token in the server's secret manager, not in the repository.
- Use a dedicated Discord application and least-privilege bot permissions.
- Set `SPIDER_OWNER_DISCORD_IDS` and, when appropriate,
  `SPIDER_ALLOWED_GUILD_IDS`.
- Leave `SPIDER_ALLOW_LAUNCH` and `SPIDER_ALLOW_STOP` disabled until you need
  them.
- Back up the SQLite database and restrict it to the service account.
- Review the code and threat model before exposing the service to the internet.
