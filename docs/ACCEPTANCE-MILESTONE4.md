# Milestone 4 acceptance record

Scope: two-way Minecraft remote management through SpiderBridge, the agent, and
the trusted backend. Branch `main-2-(1st-ai)` at `a51d75b2`. All work is
uncommitted; nothing was reset, cleaned, committed, pushed, merged, or released.

Status vocabulary:

- **Fixture-verified** — automated tests and real local builds exercise the
  behavior. Test doubles are named below; no live Minecraft client or Discord
  interaction was used.
- **Live-unverified** — the implementation exists and is fixture-verified, but
  the stated live path was not observed in this environment. The manual
  procedure is in [`SPIDERBRIDGE.md`](SPIDERBRIDGE.md) or
  [`REMOTE.md`](REMOTE.md).

| # | Acceptance item | Evidence | Status |
|---|---|---|---|
| 1 | Work only on `main-2-(1st-ai)`, preserve uncommitted work, no forbidden git operations | `git branch --show-current` = `main-2-(1st-ai)`; `HEAD` = `a51d75b2`; `git status` shows the existing modifications plus untracked `packages/spider_remote/`, `mods/`, `docs/`. No reset/clean/switch/commit/push/merge/release was run. | Pass |
| 2 | No Discord token or credential in the mod, launcher, source, commands, or docs; token stays in backend environment | `DISCORD_BOT_TOKEN` is read only in `backend.rs` (`BackendConfig::from_env`). The mod has no HTTP client and no token field. `agent-status.json` exposes only bridge enablement, endpoint path, and session count. Docs contain placeholders only. | Pass (source audit) |
| 3 | The mod never contacts Discord or the backend; all remote access goes through the existing authenticated agent | The mod opens one outbound TCP connection to `127.0.0.1` and speaks the bridge protocol. Only `spider-agent` performs outbound HTTPS with the per-device bearer token; the backend never dials the gaming PC. | Pass (architecture + tests) |
| 4 | Reuse the existing protocol, device security, pairing, and session registry | Backend↔agent `PROTOCOL_VERSION` remains 1. Pairing, DPAPI-protected device token, hashed bearer storage, session IDs, selection, and command queue are the Milestone 3 components. The only new wire contract is bridge protocol 2 on the existing loopback link. | Pass |
| 5 | Bridge is loopback-only, authenticated, ephemeral-secret, no LAN bind, bounded, rate-limited, non-blocking, clean shutdown, no arbitrary executor | `BridgeServer` binds `127.0.0.1` on an ephemeral port; 64 KiB line cap, 48 KiB reply-data cap, 5 s request timeout, 10 s staleness, 40 messages/s, 8 connections, 8 pending actions with a 4 s action timeout. `bridge.json` is user-only and removed on drop. The mod queues actions to the client tick; the bridge thread never calls the game. Unknown request kinds are refused. Test doubles: a fake agent socket in `BridgeLinkTest` and fake mod peers in `bridge.rs`. | Pass (loopback/live socket fixture) |
| 6 | No invented Minecraft APIs; verify against real 1.21.1 artifacts or report unsupported | `ClientPacketListener.sendChat(String)`/`sendCommand(String)` and `ChatListener` handlers were checked with `javap` against the mapped 1.21.1 artifact. Baritone control and in-game HUD remain explicitly unsupported in code, Discord replies, and docs. The mod builds against the real mapped artifact. | Pass (API check + build); live mixin injection Live-unverified |
| 7 | Unknown coordinates/chat/command output unavailable, not zero; fresh vs stale; submitted vs verified | `BridgeTelemetry` rejects non-finite/implausible values and a position without a loaded world. `/status` marks samples older than 10 s `STALE` and prints no coordinates. Action results carry `submitted_unverified`, `rejected`, `failed`, or `unknown`; tests assert a timeout after start is never rendered as rejection or verification. | Pass (fixture) |
| 8 | Chat monitoring off by default, owner-visible, configurable, bounded, session-separated, no `latest.log` scraping | Defaults are `allow_chat=false` on both sides. The launcher Remote screen shows/toggles the local half and the effective state. `ChatBuffer` is in-memory, 20–500 messages, 30–3600 s retention, 200 messages / 32 KiB per reply, cleared on disable/disconnect. The `ChatListener` mixin is the only source; no file/log reader exists. `/say` audit rows record only a length, and the outbound queue copy is cleared when the agent claims it. Two-instance and reconnect tests assert session separation. | Pass (fixture); live chat receipt Live-unverified |
| 9 | Screenshot targets the selected Minecraft window, session-scoped and timestamped, rejects stale/unauthorized, honors cooldown/limits, cleans up, reports failures | `capture_game_window(pid)` uses the selected process's largest visible window with no desktop fallback; the agent re-verifies PID plus process start time. The result names the session and an RFC 3339 capture time. Backend cooldown is 10 s; encoding is capped at 1920 px and 6 MiB; a blank frame is a failure. Screenshot bytes are relayed once and never written to SQLite. Tests cover encoding, size, invalid/mismatched frames, non-Windows limitation, relay-without-persistence, and rate limiting. | Pass (fixture); real window capture Live-unverified |
| 10 | `/say` uses the real client chat path; `/mc-command` uses real client command submission; server permissions/moderation respected; no unverified execution claims | `GameActions` calls `sendChat` and `sendCommand` on the client thread; `/say` refuses a leading `/`; command text is validated before queueing. Both results are `submitted_unverified` (timeout after start = `unknown`). Docs and Discord formatting state that server permissions, plugins, and moderation still apply. | Pass (fixture); live submission Live-unverified |
| 11 | Explicit chat/read-only/disruptive/administrative policy; explicit validation, not prefix-only; explicit device + session; ambiguity requires selection; stale/closed/replaced sessions rejected | `command_policy.rs` uses an explicit grammar, refuses selectors/raw JSON/unknown/chat-shaped commands, and keeps category switches independent. The backend requires `confirm:true` for state-changing categories by default (`SPIDER_REQUIRE_MC_CONFIRM`), and the agent refuses any command payload that is not confirmed. `resolve_device` requires `/select` when multiple devices exist; an explicit session is required when several run. A selected session that exited is rejected with no fallback; a reconnecting mod replaces the older socket; stale sessions lose their link. | Pass (fixture) |
| 12 | GPL-3.0-only attribution, no restricted Polyfrost branding, retain `oneclient_*` internal names and release naming, automated tests and real builds | `LICENSE`, `ATTRIBUTION.md`, and package metadata retain GPL-3.0-only attribution; user-facing surfaces use SpiderClient. Internal package/data names and release workflow naming are unchanged. Verification below. | Pass |

## Verification commands and results

Run from the repository root unless noted.

```text
cargo test -p spider_remote
  → 117 passed; 0 failed; 0 ignored

cargo check -p spider_remote --all-targets
  → Finished (no errors)

cargo check --workspace
  → Finished (no errors; one pre-existing polyio archive warning)

cargo build -p oneclient_app
  → Finished, dev profile

mods\spiderbridge> .\gradlew.bat test build
  → BUILD SUCCESSFUL

Java test report totals
  → BridgeLinkTest 11, EndpointTest 4, MessagesTest 6, SocketLinesTest 5,
    ActionQueueTest 7, ChatBufferTest 7, TelemetryTest 5
  → 45 passed, 0 failures, 0 errors
```

## What remains manual

These are the only unverified paths, and they require a real Windows gaming PC
and a real Discord application:

1. Mixin injection and the in-game handshake on Minecraft 1.21.1.
2. Chat receipt into the buffer and `/chat` showing the same lines.
3. `/say` appearing in the Minecraft chat box.
4. `/mc-command` actually running in-game and reflecting server permissions.
5. Live screenshot of the selected Minecraft window.
6. A signed Discord interaction against a running backend.

The exact steps are in sections 3–11 of [`SPIDERBRIDGE.md`](SPIDERBRIDGE.md) and
in the deployment and command sections of [`REMOTE.md`](REMOTE.md). No bot token
is required on, or should ever be copied to, the gaming PC.

## Claim boundary

The code paths, bounds, policy decisions, and truthfulness rules above are
tested with fixtures and real builds. This record does **not** claim that a live
Minecraft client or a live Discord interaction was observed. Milestone 4 is
fixture-complete; the six manual checks above close the remaining gap.
