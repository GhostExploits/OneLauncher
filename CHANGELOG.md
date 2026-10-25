## Unreleased

### Features

- Rebranded the product surface as SpiderClient with original monochrome artwork,
  updated Windows/macOS/Linux metadata, an About page with project credits, and
  a restrained charcoal-and-steel palette.
- Added the SpiderClient Remote stack: a per-device outbound agent, a trusted
  Discord backend with signed interactions and pairing, session-specific
  monitoring, redacted logs, and explicitly confirmed launch/stop actions.
- Added a Remote screen that reads the local agent status, starts/stops a bundled
  agent executable, and never asks for or stores the Discord bot token.
- Added `docs/REMOTE.md`, `.env.example`, and tests for pairing, revocation,
  capability escalation, Discord signatures, replay windows, redaction, session
  identity, command delivery, and alerts.
- Added the SpiderBridge Fabric 1.21.1 companion mod and an authenticated
  loopback telemetry bridge, so `/coords` returns live in-game coordinates and
  `/status` reports whether the mod is connected and whether its sample is
  fresh. The mod is client-only, never contacts Discord or the backend, and
  reports no position outside a world.
- Added opt-in two-way SpiderBridge control. `/chat` returns a bounded, in-memory
  window of chat the client actually received (never a `latest.log` scrape);
  `/say` submits a line through the vanilla chat path; `/mc-command` submits a
  command only after strict grammar and per-category policy checks. Every action
  stays bound to an explicit device and session, and the mod queues work to the
  client thread instead of blocking the render thread.
- Added `/command-logs`, an owner-scoped command audit in the backend SQLite
  database. Lifecycle rows record queued/agent-received/submitted-unverified/
  rejected/failed/unknown outcomes, category, and confirmation state. Chat
  message bodies are never written to the audit rows or result history, and the
  outbound queue copy is cleared as soon as the agent claims the request.
- Added local owner controls on the launcher's Remote screen for the bridge,
  chat, chat sending, Minecraft commands, and each command category. It edits
  `agent.json` atomically while preserving unknown fields and the protected
  device token, shows the effective two-gate state, and asks for an agent restart
  after a change.

### Notes

- Chat, `/say`, and `/mc-command` are off by default and need both the local
  `agent.json` switch and the matching backend environment flag. `/say` and
  `/mc-command` report **submitted, outcome unverified**, never "executed":
  vanilla Minecraft exposes no correlated result, and a timeout after the client
  started an action is reported as **unknown**, not rejection.
- Remote command policy is an explicit grammar with independent read-only,
  disruptive, and administrative categories, not a name-prefix allowlist.
  Selectors, raw JSON, unknown commands, and chat-shaped commands are refused.
  Server permissions and moderation still apply; the client cannot grant
  itself new permissions.
- In-game HUD data and Baritone status/control are not implemented; commands say
  so instead of returning fake data. Baritone presence is detected from the mods
  folder only.
- The SpiderBridge in-game path is covered by Rust and Java tests, including
  policy refusals and truthful outcome reporting, but still needs the manual
  Windows verification in `docs/SPIDERBRIDGE.md`; it has not been launched inside
  a live Minecraft client in this environment. Discord interactions are also
  fixture-tested only, because no bot token is available here by design.
- Internal package names and existing OneClient data paths remain unchanged for
  safe upgrades.

## 2.8.0 (2026-10-09)

### Features

- Added safe multiple minecraft instances launching, only dedicated clusters tho by [rozwader](https://github.com/rozwader)

### Fixes

- Added better informations about errors from mclogs. by [rozwader](https://github.com/rozwader)
- Added status bar showing up if a player is logged out or he has a stale refresh token by [rozwader](https://github.com/rozwader)
- Added a filter for choosing modpack loaders by [rozwader](https://github.com/rozwader)
- SkyBlock content installed without asking is removed from 26.3 clusters once, and turning an optional bundle down removes its mods
- Fixed shield.io badges not rendering properly in markdown viewers by [rozwader](https://github.com/rozwader)

## 2.7.0 (2026-10-05)

### Features

- Code blocks are selectable, and the open link confirmation has a copy button by [LynithDev](https://github.com/LynithDev)
- Game settings have a Game Arguments field for passing extra arguments to Minecraft, globally or per cluster by [Wyvest](https://github.com/Wyvest)
- Game logs hide account tokens, and the game output starts with the launch command ready to paste into a terminal by [LynithDev](https://github.com/LynithDev)
- GitHub hosted mods have a "View in browser" option that opens their repository by [LynithDev](https://github.com/LynithDev)
- The changelog page now shows each GitHub release's notes by [LynithDev](https://github.com/LynithDev)
- The mods folder now syncs with the launcher: jars added, renamed or replaced by hand show up in the Mods list, and jars deleted by hand are switched off instead of being put back by [Wyvest](https://github.com/Wyvest)

- Choose which GPU the game renders on by [LynithDev](https://github.com/LynithDev)

### Fixes

- Icons show on Linux systems with a comma decimal locale by [LynithDev](https://github.com/LynithDev)
- The "Uploaded to mclo.gs" notification no longer repeats when switching back to the Logs tab by [LynithDev](https://github.com/LynithDev)
- Open folder and open in browser work again on Linux by [LynithDev](https://github.com/LynithDev)
- Optional bundles are now offered before the first launch of every new version instead of being installed or skipped silently by [Wyvest](https://github.com/Wyvest)
- System information in the settings sidebar shows a tooltip explaining it can be clicked to copy by [LynithDev](https://github.com/LynithDev)
