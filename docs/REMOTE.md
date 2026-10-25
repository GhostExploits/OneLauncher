# SpiderClient Remote

SpiderClient Remote lets the owner of a paired computer inspect and, when
explicitly enabled, start or stop Minecraft sessions, read bounded in-game chat,
send chat, and submit policy-checked Minecraft commands from Discord.

It is intentionally split into four trust domains:

```
 Discord  ──signed interactions──▶  Trusted backend  ◀──outbound TLS──  SpiderClient Agent
                                      │  SQLite registry
                                      │  bot token (server only)
                                      ▼
                              optional website API
```

## Non-negotiable security rules

1. `DISCORD_BOT_TOKEN` exists **only** on the trusted backend host. It is read
   from the environment, never written to the repository, never sent to an
   agent, never included in a client binary, and never printed by a command.
2. The desktop launcher never asks for the bot token.
3. A device ID is not a credential. Every agent request carries a 256-bit
   per-device bearer token. The backend stores only its SHA-256 hash.
4. Pairing codes are single-use, 10 minutes by default, rate-limited, and
   hashed at rest.
5. Discord interaction signatures are verified with the application public key,
   and timestamps outside a five-minute window are rejected.
6. Commands are scoped to the interaction's Discord user, the selected device,
   the selected session, the backend policy, and the agent's own allowlist.
7. `/launch` and `/stop` require `confirm:true`. `/unpair` requires
   `confirm:true`.
8. Monitoring responses are ephemeral. The backend does not publish session
   status through a public endpoint.
9. Chat collection is off by default. When enabled, the mod keeps a bounded
   in-memory buffer of messages the client actually received, never a scrape of
   `latest.log`; it is cleared when collection is disabled or the bridge
   disconnects. Chat bodies are relayed to Discord once and are never stored in
   the command audit database; the outbound queue copy is cleared when the agent
   claims it, and the agent does not write it to disk.
10. Remote Minecraft commands are matched against an explicit grammar and an
    owner-selected category (read-only, disruptive, administrative), not a
    command-name prefix. Selectors, raw JSON, unknown commands, and
    chat-shaped commands are refused. The client can only use permissions the
    player already has; the backend cannot grant or bypass server permissions.
11. There is no generic shell, PowerShell, file browser, or arbitrary command
    runner behind Discord.
12. If the backend or the gaming PC is offline, no local game action can occur.
    A sleeping computer cannot keep farming. The bot reports that truth.
13. "Submitted" is not "executed". Vanilla chat does not expose a correlated
    result, so `/say` and `/mc-command` results stay **submitted, unverified**
    unless a reliable event exists. A timeout after the client started an action
    is reported as **unknown**, never as a rejection.

## What is implemented

| Capability | Status |
|---|---|
| Device pairing, ownership, revocation | Implemented |
| Agent heartbeat, online/offline tracking | Implemented |
| Multiple sessions per device with stable IDs | Implemented |
| Session selection by device and session ID | Implemented |
| Status, specs, performance, uptime, agent info | Implemented from reported snapshots |
| Bounded, redacted log excerpts | Implemented |
| Mod count and Baritone jar detection | Implemented from the instance `mods` directory |
| Launch an installation through the launcher CLI | Implemented, opt-in on backend and agent |
| Process stop (termination signal, not a guaranteed in-game save) | Implemented, opt-in on backend and agent |
| Screenshots of the selected Minecraft window | Implemented, opt-in on backend and agent; see limits below |
| Live coordinates from the SpiderBridge Fabric mod | Implemented, opt-in on backend; loopback-only, live request/reply; see below |
| Bounded in-memory chat history (`/chat`, `/chatlogs`) | Implemented, opt-in on backend and agent; never read from `latest.log` |
| Send chat through the client (`/say`) | Implemented, opt-in on backend and agent; reports submission, not delivery |
| Policy-checked Minecraft commands (`/mc-command`) | Implemented, opt-in on backend and agent; strict allowlist and per-category switches |
| Owner-scoped command audit (`/command-logs`) | Implemented; lifecycle rows only, chat bodies are never persisted |
| Launcher Remote screen: start agent, clean local stop, status, capability switches | Implemented |
| Alerts for agent online/offline and session start/end | Implemented via `SPIDER_ALERT_CHANNEL_ID` |
| Windows sign-in autostart for the agent | Implemented, opt-in and reversible from Settings |
| In-game HUD, Baritone control API | **Not implemented.** Baritone presence is detection only |
| Tray icon for the agent | **Not implemented.** Status is in `agent-status.json` and `/agent` |

Do not interpret "process is running" as proof that farming is progressing.
Coordinates come from the game only when the SpiderBridge mod is installed and
connected; there is no log-scraping fallback.

## Build

```sh
cargo build --release -p spider_remote --bins
```

Artifacts:

- `target/release/spider-agent` (`spider-agent.exe` on Windows)
- `target/release/spider-remote` (`spider-remote.exe` on Windows)

The release and nightly installers build both `oneclient_app` and
`spider-agent` and package them side by side. The launcher's **Remote** screen
looks for `spider-agent` next to its own executable, so a normal install can
start the local agent without a separate download. If you build only
`oneclient_app` yourself, copy `spider-agent` next to it or the screen will
truthfully report that the agent binary is missing.

## Backend deployment

The backend must be reachable by every agent over HTTPS. It does not expose an
inbound port on the gaming PC; the agent only makes outbound requests.

1. Copy the release binary to the server.
2. Create the environment from `.env.example` **on that server**, or use the
   platform's secret manager. Do not commit `.env`.
3. Put a TLS-terminating reverse proxy (Caddy, nginx, or the platform ingress)
   in front of `SPIDER_BIND`. Example Caddy:

   ```caddy
   remote.example.com {
       reverse_proxy 127.0.0.1:8787
   }
   ```

4. Run:

   ```sh
   set -a; . ./.env; set +a   # or export variables in your service manager
   ./spider-remote doctor
   ./spider-remote register
   ./spider-remote serve
   ```

5. Invite the Discord application with the `applications.commands` and `bot`
   scopes. The bot needs permission to send messages in the alert channel.
6. Copy the **Public Key** from the Discord Developer Portal into
   `DISCORD_PUBLIC_KEY`. Set the Interactions Endpoint URL to
   `https://remote.example.com/interactions`. Discord sends a signed PING; the
   backend answers with PONG, so Discord can verify the endpoint.

`spider-remote register` performs a bulk overwrite of this application's global
commands. Set `DISCORD_GUILD_ID` to register in one guild while testing (guild
commands appear immediately; global commands can take time to propagate).

### systemd sketch

```ini
[Unit]
Description=SpiderClient trusted remote backend
After=network-online.target

[Service]
User=spiderclient
WorkingDirectory=/opt/spiderclient
EnvironmentFile=/etc/spiderclient/remote.env
ExecStart=/opt/spiderclient/spider-remote serve
Restart=on-failure
RestartSec=3
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ReadWritePaths=/var/lib/spiderclient

[Install]
WantedBy=multi-user.target
```

Set `SPIDER_DATABASE_URL=sqlite:///var/lib/spiderclient/spider_remote.db` for
that unit. The token in `/etc/spiderclient/remote.env` must be readable only by
the service account. This is a deployment sketch, not a claim that a production
hardening review has been completed.

## Pairing a device

On the backend host (or with an administrator in Discord):

```sh
./spider-remote pair --owner 123456789012345678
```

or use `/pair` in Discord as a configured administrator. Both return a
single-use code. The code is not the bot token.

On the gaming PC:

```sh
./spider-agent pair \
  --server https://remote.example.com \
  --code ABCD-EFGH-JKLM \
  --name "Gaming PC"

./spider-agent run
```

The agent writes its own credential to the current user's config directory.
On Windows the token is encrypted with the current user's DPAPI key first, so
copying `agent.json` to another account or machine does not reveal it; a legacy
plaintext config still loads once and is rewritten in protected form on the
next start. On Unix the file is created with mode `0600`. Do not copy the file
into a backup you share.

The agent binds `127.0.0.1:45817` as a cross-platform single-instance guard. A
second agent exits instead of registering the same device twice.

To allow actions, edit the agent config and set `"allow_launch": true` and/or
`"allow_stop": true`, **and** set `SPIDER_ALLOW_LAUNCH=1` /
`SPIDER_ALLOW_STOP=1` on the backend. Both gates must be open.

Screenshots have their own opt-in: set `"allow_screenshot": true` in
`agent.json` **and** `SPIDER_ALLOW_SCREENSHOT=1` on the backend. Both gates must
be open or `/screenshot` explains that capture is disabled and sends no image.

Live coordinates use the SpiderBridge companion mod. The agent starts its
loopback listener when `"allow_bridge": true` (the default). The backend must
also set `SPIDER_ALLOW_COORDINATES=1`, and the mod must be installed in the
selected Minecraft instance. Every layer is required; `/coords` names the layer
that is missing instead of inventing data.

Chat and command features are separately gated. The launcher's **Remote** screen
can toggle the local half for you and shows the effective state; changing it
requires an agent restart. The backend half is set on the backend host only:

| Feature | `agent.json` | Backend environment |
|---|---|---|
| Read chat history | `"allow_bridge": true`, `"allow_chat": true` | `SPIDER_ALLOW_CHAT=1` |
| Send chat (`/say`) | `"allow_bridge": true`, `"allow_chat_send": true` | `SPIDER_ALLOW_CHAT_SEND=1` |
| Submit Minecraft commands | `"allow_bridge": true`, `"allow_minecraft_command": true` | `SPIDER_ALLOW_MC_COMMAND=1` |
| Read-only command category | `"command_policy": {"read_only": true}` | `SPIDER_MC_ALLOW_READ_ONLY=1` |
| Disruptive command category | `"command_policy": {"disruptive": true}` | `SPIDER_MC_ALLOW_DISRUPTIVE=1` |
| Administrative category | `"command_policy": {"administrative": true}` | `SPIDER_MC_ALLOW_ADMIN=1` |
| Require `confirm:true` for state changes | agent refuses any command payload without `confirm` | `SPIDER_REQUIRE_MC_CONFIRM=1` (default) |

`/settings` shows both halves for the selected device: what the device declared,
what the backend allows, and what is therefore effective. The backend cannot
read or modify the device's `agent.json`; a switch that looks on in Discord does
nothing if the local half is off.

## Screenshots

`/screenshot` captures the **Minecraft window owned by the selected session's
process**, not the desktop.

- It finds the largest visible top-level window owned by that process. If the
  window is missing, minimized, has no readable frame, or is too large, the
  command fails and reports why. There is no whole-desktop fallback.
- The image is downscaled so its longest edge is at most 1920 pixels, encoded in
  memory as PNG (falling back to JPEG), and rejected if the encoded image is
  larger than 6 MiB. A blank or fully transparent frame is treated as a failed
  capture, not as a screenshot.
- A per-user/device cooldown of 10 seconds limits repeated captures.
- The agent binds a request to a session ID. Before capture it verifies that the
  PID still belongs to the same process start time, so a recycled PID cannot
  return a different game.
- The backend validates the image type, filename, dimensions, and decoded size
  again, relays it once as an ephemeral Discord follow-up, and strips the image
  before storing the command result. Screenshot bytes are not written to SQLite.
- The image may contain private on-screen content. Enable this only after
  considering who can see the Discord channel and who can invoke commands.

The real Windows capture path has not been exercised against a running Minecraft
window in this development environment. Encoding, size limits, authorization,
and failure paths are covered by tests; a live in-game check is still required.

## Live coordinates (SpiderBridge)

[`docs/SPIDERBRIDGE.md`](SPIDERBRIDGE.md) is the operator guide: building the
mod, installing it, reading the endpoint file, and verifying every state on a
real Windows PC.

How it fits together:

```
Minecraft + SpiderBridge  --127.0.0.1 only-->  spider-agent  --HTTPS-->  backend  -->  Discord /coords
```

- The mod never talks to Discord or the backend. It opens one outbound TCP
  connection to a port the agent bound on `127.0.0.1`; the agent is the only
  remote-facing process.
- The listener uses an ephemeral port. The agent writes `bridge.json` in its
  per-user local data directory with the port and a 256-bit secret generated
  **for that agent run**. The file is deleted when the agent shuts down. No
  reusable secret is compiled into the JAR.
- `bridge.json` is inside the same operating-system user's trust boundary.
  Another process running as the same user can read it. This is the documented
  limit of a loopback bridge; it is not a multi-tenant secret.
- `/coords` performs a fresh request/reply against the mod (3-second timeout).
  It never returns the heartbeat cache or a log line. If the game is not in a
  world, the reply says so and contains no coordinates. Zero is never
  substituted for "unknown".
- Telemetry older than 10 seconds is marked stale. `/status` may still say the
  mod is connected, but it does not print stale coordinates as live.
- A connection is bound to the exact Minecraft process: the mod reports its PID
  and game directory, and the agent matches them against the session list it
  already tracks. A wrong PID, a different folder, a wrong secret, or a
  mismatched protocol is refused with a specific error.
- Two Minecraft instances can connect at once; each gets its own session ID and
  its own telemetry. When a session exits, its link and its coordinates are
  removed.
- The mod's request handler answers `status` from an immutable snapshot captured
  on the client thread. It also answers `chat` from a bounded in-memory buffer
  and accepts `chat_send`/`command`, which it queues to the client thread. It has
  no arbitrary command executor and no Baritone control. Unknown request kinds
  are refused.

## Chat and Minecraft commands

The same SpiderBridge mod provides three more opt-in features. They share the
loopback channel and the two-gate rule described above; none of them exist
without the mod, the agent, and the backend.

### Chat history (`/chat`, `/chatlogs`)

- Collection is **off by default**. The mod starts buffering only after the agent
  sends its `welcome` for a session; it stops and clears on disable, disconnect,
  or when the retention window expires.
- The buffer is bounded by `chat_retention_seconds` (30–3600, default 300) and
  `chat_max_messages` (20–500, default 200). A reply contains at most 200
  messages and 32 KiB of text; the protocol validator enforces both.
- Messages come from the vanilla `ChatListener` on the client thread, not from
  `latest.log`. There is no log scraper and no file of past chat.
- System lines are included by default; pass `include_system:false` to hide
  them. Action-bar lines are excluded unless `include_action_bar:true` is set.
  The reply carries the session ID, collection state, and a dropped count when
  the buffer discarded older entries.
- History is session-scoped. Two Minecraft instances never share a buffer, and a
  reopened bridge connection starts empty. A closed or replaced session is
  rejected rather than answered from another instance.
- Chat bodies are shown once in Discord and are **not** written to the audit
  database or to disk by the agent.

### Sending chat (`/say`)

- `/say` submits one line through the client's normal chat path. The line must
  appear in Minecraft; echoing it only in Discord is a bug.
- A message beginning with `/` is refused: `/say` is chat, not a command path.
  Control characters, newlines, and text over the limit are refused before
  queueing.
- The result is reported as **submitted, outcome unverified**. Vanilla chat has
  no correlated receipt, so delivery to other players or the server is never
  claimed.

### Minecraft commands (`/mc-command`)

- Commands are matched against an explicit grammar, not a name prefix. The
  category is derived from the subcommand and argument shape, and anything
  unrecognised is refused before it reaches the client.
- Categories are independent: read-only (queries), disruptive (reversible world
  or player changes), and administrative/destructive (op, ban, kick, kill, ...).
  Each needs its own switch on both sides.
- Selectors (`@a`, `@e`, ...), raw JSON, unknown commands, and chat-shaped
  commands (`say`, `msg`, `tell`, `tellraw`, ...) are refused. Use `/say` for
  chat.
- When `SPIDER_REQUIRE_MC_CONFIRM=1` (the default), state-changing categories
  additionally need `confirm:true` in the Discord invocation.
- The client can only use permissions its player already has. Server
  permissions, plugins, and moderation still apply; the backend cannot grant or
  bypass them.
- The result is **submitted, outcome unverified** for the same reason as
  `/say`. A refusal before the client thread claims the work is **rejected**
  (nothing was sent). A timeout after the client started it is **unknown**, not
  a rejection, because the command may already have run.

### Command audit (`/command-logs`)

Every `/say` and `/mc-command` request writes lifecycle rows to the backend's
SQLite `command_audit` table: queued, agent received, and the final
submitted-unverified / rejected / failed / unknown outcome, with the category,
whether confirmation was required or obtained, and the requester. Chat message
bodies are not persisted: a `/say` row records that a line was requested and its
length, not its text. `/mc-command` rows record the validated command text,
category, and outcome. Rows are scoped to their requester: an owner sees their
own history, an administrator can filter by requester or device, and
`/command-logs clear:true confirm:true` removes matching rows for the caller.
Retention is `SPIDER_AUDIT_RETENTION_DAYS` (1–90, default 7).

Discord applies short per-user cooldowns so a stuck key cannot flood the game:
2 seconds for `/chat`, 3 seconds for `/say`, and 3 seconds for `/mc-command`.

## Starting and stopping the agent locally

The **Remote** screen in the launcher is the local control surface. It shows
whether the device is paired, whether `spider-agent` is next to the launcher,
the current heartbeat, and the exact config/status paths.

- **Start agent** launches `spider-agent run` as a normal background process.
  It is visible in Task Manager and to security tooling; nothing is hidden or
  renamed. Start is disabled until the device is paired.
- **Stop agent** writes a small `agent.stop` marker into the local data
  directory. The agent checks for it between requests and every second while
  waiting, writes an offline status, removes the marker, and exits. It does not
  wait for the backend and works while the network is down.
- If the process is wedged, end `spider-agent` in Task Manager. No credential is
  lost. A marker left behind by a crash or reboot is deleted at the next start,
  so it cannot prevent the agent from running.
- Stopping the agent does not stop Minecraft, and stopping a game session does
  not stop the agent. Remote `/launch` and `/stop` are separate, opt-in
  capabilities and remain unavailable unless both gates are enabled.

### Start with Windows

Settings → Launcher has two independent options:

- **Start Agent with SpiderClient** starts the paired agent when the launcher
  opens. It does nothing if the agent is not paired or is already running.
- **Start Agent at Windows Login** creates a normal per-user entry in
  `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` named `SpiderClientAgent`.
  The value is `"<launcher path>" --agent-only`, so sign-in starts only the agent
  and does not open the launcher window. It needs no administrator rights.

Turning the login option off removes the value. This is a standard, visible
startup mechanism: it appears in Windows startup settings and Task Manager,
uses the current user's registry hive, and does not copy, rename, or hide the
binary. The launcher refreshes the recorded path each time it starts, so moving
the install does not leave a stale command. The Remote screen shows whether the
entry currently exists. On non-Windows builds the setting is shown as
unavailable rather than silently saved, because this mechanism is the Windows
registry only.

## Two devices

1. Pair device A with a new code from `/pair` or `spider-remote pair`.
2. Pair device B with a **different** code.
3. In Discord, run `/devices` and note the two device IDs.
4. Run `/sessions` to see each device's sessions.
5. Run `/select device:<id>` before monitoring that device.
6. If a device has several Minecraft processes, `/select` with the exact
   `session:<id>` from `/sessions`. A username is not a unique session ID.
7. When a selected session exits, `/status` reports that it is no longer
   running; select another session. The bot never silently falls back to
   "whichever process was enumerated first."

Device owners see only their own devices. A configured administrator sees all
devices. An unguessable device ID alone cannot authorize a command.

## Revocation and removal

- Discord: `/unpair device:<id> confirm:true` removes the device, its commands,
  and its selection. Only the owner or an administrator may do this.
- Agent PC: stop `spider-agent`, then run `spider-agent unpair` to remove the
  local credential.
- Rotate the Discord bot token immediately if it is ever pasted into a chat,
  log, screenshot, or repository. A leaked bot token is a full bot compromise,
  independent of SpiderClient's device credentials.
- To disable the entire remote feature, stop the backend and do not run the
  agent. The launcher remains functional.

## Agent files

Config (contains the device credential; do not share):

- Windows: `%APPDATA%\SpiderClient\SpiderClient\config\agent.json` — the
  `device_token` value is DPAPI-protected for the current Windows account.
- Linux/macOS: use `spider-agent config-path`; the path follows the operating
  system's per-user config convention and the file is written with mode `0600`.

Status (safe to inspect; no credential): `agent-status.json` in the
platform-specific local data directory. It is a point-in-time snapshot, so
check `updated_at` before treating `online` as current. The Remote screen in the
launcher shows the exact path on this computer. Bridge fields
(`bridge_enabled`, `bridge_endpoint`, `bridge_sessions`) never include the
secret.

Bridge endpoint (contains a per-run secret; do not share): `bridge.json` in the
same local data directory. On Windows that is
`%LOCALAPPDATA%\SpiderClient\SpiderClient\data\bridge.json`. It exists only
while the agent runs, and the SpiderBridge mod re-reads it on every reconnect so
an agent restart is picked up without restarting the game.

The exact config path can be printed with `spider-agent config-path`.

## Protocol

`PROTOCOL_VERSION = 1`. All agent traffic is JSON over HTTPS:

- `POST /v1/pair/claim`
- `POST /v1/agent/heartbeat`
- `POST /v1/agent/command-result`
- `POST /interactions` (Discord only)
- `GET /health`

Commands are delivered once, expire after 120 seconds, are bound to the
authenticated device, and carry an explicit command ID. Results are idempotent.
A command for a device whose last heartbeat marked it offline is refused
immediately with an ephemeral explanation instead of being queued until it
expires. A device that goes offline after queuing still receives the timeout
follow-up, so a deferred reply never hangs. Device capabilities are intersected
with backend policy on every heartbeat, so a modified agent cannot grant itself
launch/stop by editing its config.

### Loopback bridge protocol

`BRIDGE_PROTOCOL = 2`; newline-delimited UTF-8 JSON, maximum 64 KiB per line,
on an ephemeral `127.0.0.1` port. This protocol is local only and is never sent
over the network. A version 1 peer is refused explicitly rather than served a
different contract.

Client to agent:

```json
{"type":"hello","protocol":2,"secret":"<64 hex chars>","pid":4242,
 "game_dir":"C:/games/instance","bridge_version":"0.1.0","mc_version":"1.21.1"}
{"type":"telemetry","telemetry":{...}}
{"type":"reply","id":"<request id>","ok":true,"telemetry":{...},"data":{...}}
{"type":"bye"}
```

Agent to client:

```json
{"type":"welcome","protocol":2,"session_id":"<session id>",
 "chat_collection":false,"chat_retention_seconds":300,"chat_max_messages":200, ...}
{"type":"request","id":"<request id>","kind":"status","payload":{}}
{"type":"request","id":"<request id>","kind":"chat","payload":{"limit":50,"include_system":false}}
{"type":"request","id":"<request id>","kind":"chat_send","payload":{"message":"hello"}}
{"type":"request","id":"<request id>","kind":"command","payload":{"command":"list","category":"read_only","confirm":false}}
{"type":"error","code":"unauthorized|protocol_mismatch|unknown_session|session_mismatch","message":"..."}
```

`welcome` is where the owner's chat opt-in reaches the mod: the mod collects
nothing while `chat_collection` is false. Reply `data` is capped at 48 KiB and is
validated by the requesting method, never trusted blindly.

The agent enforces a handshake timeout, an idle timeout, a message rate limit
(40 messages per second), a connection cap, a hard line-size cap, one outstanding
request per session with a 5-second reply timeout, and telemetry validation
(finite numbers, plausible world bounds, and no position without a loaded
world). At most 8 actions are pending for the mod; older pending actions are
rejected rather than queued without bound.

## Website integration contract

Faris's website must not receive Discord credentials, device tokens, or
Minecraft account data. A future website API should use a separate
`SPIDER_WEBSITE_API_TOKEN` and expose only:

```json
{
  "protocol": 1,
  "devices": [
    {
      "id": "opaque-device-id",
      "name": "Gaming PC",
      "online": true,
      "session_count": 2,
      "capabilities": ["sessions", "logs", "performance"]
    }
  ]
}
```

No usernames, log contents, screenshots, tokens, or private session details
belong on a public website. A website endpoint is not implemented yet; the
environment variable and this contract are the integration point.

## Known limitations

- Coordinates require the SpiderBridge Fabric mod for Minecraft 1.21.1 in the
  selected instance, a running agent with `allow_bridge`, and
  `SPIDER_ALLOW_COORDINATES=1` on the backend. Without all three, `/coords`
  explains which layer is missing. The mod's transport and payloads are covered
  by automated tests, but this build has not been launched inside a live
  Minecraft client in this development environment; see
  [`docs/SPIDERBRIDGE.md`](SPIDERBRIDGE.md) for the manual check.
- `PROTOCOL_VERSION` (backend ↔ agent) is still 1. The loopback bridge is
  protocol 2; these are independent version numbers.
- Chat, `/say`, and `/mc-command` are covered by Rust and Java tests, including
  policy refusals and truthful outcome reporting, but this build has not been
  launched inside a live Minecraft client in this development environment. Mixin
  injection, actual chat receipt, and actual command execution still need the
  manual checks in [`docs/SPIDERBRIDGE.md`](SPIDERBRIDGE.md).
- Command outcomes stay **submitted, unverified** because vanilla Minecraft does
  not expose a correlated result. A timeout after the client thread started an
  action is reported as **unknown**, never as a rejection.
- A live Discord interaction was not executed in this development environment
  (no bot token is available here, by design). Discord command gating, queueing,
  and formatting are fixture-tested only.
- In-game HUD data and a Baritone control API have no implementation. Baritone is
  detected from the mods folder and reported as presence only; a detected jar is
  not proof that Baritone is running or configured.
- SpiderBridge also samples FPS and frame time, but this build does not display
  them as a live feed; `/performance` still reports the agent heartbeat's
  process memory, not an in-game frame graph.
- The bridge trusts other processes running as the same operating-system user:
  they can read `bridge.json`. It is a local convenience channel, not a
  security boundary between applications of one user.
- The bridge only works while both the agent and the game are running. The agent
  publishes a new endpoint on every start; the mod reconnects with backoff.
- Screenshot capture is Windows-only. It requires a visible, non-minimized
  Minecraft window owned by the selected process. It has not been tested against
  a live game window in this environment.
- Alerts are transition-based and use a fixed 60-second deduplication window.
  Alert destination is configured by environment, not per device.
- The agent polls every 5 seconds rather than maintaining a push socket. This is
  intentional: it is simple, bounded, and reconnects cleanly via exponential
  backoff.
- The agent has no tray icon. Start it from the launcher Remote screen, from
  Windows sign-in if explicitly enabled, or with `spider-agent run`.
- The backend has not been penetration-tested. Review the code and threat model
  before exposing it to the internet.
