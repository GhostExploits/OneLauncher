# SpiderBridge manual verification (Windows)

This guide verifies the real, in-game path between Minecraft and the
SpiderClient agent. It is deliberately written so that **no Discord bot token
ever appears** on the gaming PC, in a command, or in a file.

SpiderBridge is a client-only Fabric mod for **Minecraft 1.21.1**. It connects
only to the local agent over `127.0.0.1`. It never talks to Discord or to the
remote backend.

By default it sends **read-only telemetry** and answers a `status` request with
the latest immutable sample. When the device owner opts in, it can also:

- keep a bounded, in-memory buffer of chat the client actually received;
- submit one chat line per request through the vanilla client;
- submit one strictly policy-checked Minecraft command through the vanilla
  client.

There is no arbitrary command executor, no shell, no Baritone control, and no
log scraper. Every action is queued to the Minecraft client thread by the
bridge thread; the bridge thread never calls into the game.

## 1. What you need

- Windows 10 or 11.
- JDK 21 (the build uses Java 21; Loom 1.17.x does not support a newer-only
  toolchain requirement here).
- Minecraft **1.21.1** with **Fabric Loader 0.16.0 or newer**.
- This repository checked out locally.
- An already paired `spider-agent` (pairing is its own flow; this guide does not
  create or display credentials).
- Optional for the Discord checks: a backend you already run, with the relevant
  gates set there by its owner. Do not paste `DISCORD_BOT_TOKEN` anywhere on
  this PC.

## 2. Build the mod

From the repository root:

```powershell
cd mods\spiderbridge
.\gradlew.bat build
```

Expected result:

```text
BUILD SUCCESSFUL
```

The installable file is:

```text
mods\spiderbridge\build\libs\spiderbridge-0.1.0.jar
```

Automated tests for the same project:

```powershell
.\gradlew.bat test
```

They cover payload encoding, bounds, reconnect backoff, framing, the real
loopback socket conversation against a fake agent, the bounded chat buffer, and
the action queue's ordering/cancel/timeout rules. They do **not** start
Minecraft, so mixin injection and actual chat delivery remain manual checks.

## 3. Install the mod

1. Put `spiderbridge-0.1.0.jar` in the selected instance's `mods` folder.
2. Do not add a second copy.
3. Start that instance through the launcher (or normally) while the agent runs.

The mod is client-only. It declares `minecraft ~1.21.1`, Fabric Loader, and
Java 21 as requirements and does not depend on Fabric API.

## 4. Start the agent and find the endpoint

Start the agent from the launcher's **Remote** screen, or run it in a terminal so
you can read its log:

```powershell
target\debug\spider-agent.exe run
```

(Use the release binary if that is what you built.)

While it runs, the endpoint file exists at:

```text
%LOCALAPPDATA%\SpiderClient\SpiderClient\data\bridge.json
```

Check it without printing the secret:

```powershell
$bridge = "$env:LOCALAPPDATA\SpiderClient\SpiderClient\data\bridge.json"
Get-Item $bridge | Select-Object FullName, Length, LastWriteTime
(Get-Content $bridge -Raw | ConvertFrom-Json) |
  Select-Object schema, protocol, port, agent_version, agent_pid, updated_at
```

Expected: `schema = 1`, `protocol = 2`, a port between 1024 and 65535, the
running agent's version and PID. **Do not** run `Get-Content $bridge` in full and
do not paste its `secret` into a chat, an issue, or a screenshot. The mod reads
this file itself.

The status file next to it must never contain the secret:

```powershell
$status = "$env:LOCALAPPDATA\SpiderClient\SpiderClient\data\agent-status.json"
Get-Content $status -Raw | ConvertFrom-Json |
  Select-Object bridge_enabled, bridge_endpoint, bridge_sessions, updated_at
```

`bridge_enabled` should be `true` unless `"allow_bridge": false` was set in
`agent.json`. `bridge_endpoint` is the path, not the credential.

## 5. Confirm the mod connected

In the Minecraft log:

```powershell
Get-Content "<instance>\logs\latest.log" -Tail 200 |
  Select-String "SpiderBridge"
```

Expected lines, once each at startup:

```text
[SpiderBridge] Minecraft 1.21.1, fabric <loader>, bridge 0.1.0, Baritone detected=false
[SpiderBridge] connected to session <session id> for protocol 2 on 127.0.0.1:<port> ...
```

If the agent is not running yet, the mod logs that it is waiting for the
endpoint file and retries with backoff (1 s, 2 s, 4 s, ... capped at 30 s). Start
the agent and the connection should appear without restarting Minecraft. This
also proves that the mod re-reads `bridge.json` instead of caching an old
secret. A protocol 1 peer is refused explicitly, not served a different
contract.

## 6. Telemetry state matrix

Run these checks in order. `/status` and `/coords` are Discord commands; the
local agent-status checks work without Discord.

| Situation | What to expect |
|---|---|
| Agent running, Minecraft closed | `/coords` says the mod is not connected. `/status` shows no bridge for that device. No coordinates. |
| Minecraft open at the main menu | `/coords` says the player is not in a world and returns **no** X/Y/Z. |
| World loading, no player yet | `/coords` says the world is still loading; still no X/Y/Z. |
| Normal single-player or server world | `/coords` returns X/Y/Z, dimension, yaw/pitch, and a sample age under 10 s. Compare with the F3 screen. |
| Minecraft exits | The link disappears. `/coords` returns to "not connected"; the agent stays online. |
| Agent restarts while the game stays open | The mod reconnects within about 30 s and logs the new session. Old coordinates are not reused. |
| Two Minecraft instances | `/sessions` lists both; `/coords` with each session id returns that instance's own position. |
| Telemetry paused (see below) | `/status` marks the sample `STALE` and does not print its coordinates as live. |

### Compare against the real game

In a world, open the F3 debug screen and note X/Y/Z. Run `/coords` for that
session. The values must match within rounding; the bridge samples once per
second.

### Verify that a missing world is not zero

1. Quit to the title screen (do not close the game).
2. Run `/coords` for the same session.
3. The reply must say the player is not in a world. It must **not** show
   `X 0 Y 0 Z 0`, and `/status` must not show a position.

### Verify staleness

Minimize risk: save the world first. Then suspend the game process briefly.
These cmdlets require PowerShell 7 (`pwsh`), not Windows PowerShell 5.1:

```powershell
# Find the exact process. Do not kill it.
Get-Process javaw, java |
  Select-Object Id, ProcessName, StartTime, MainWindowTitle
```

Use the PID that belongs to the selected session, suspend it for about 15
seconds, then resume:

```powershell
$pidToPause = <PID from the session list>
Suspend-Process -Id $pidToPause
Start-Sleep -Seconds 15
Resume-Process -Id $pidToPause
```

During the pause, `/status` should keep saying the mod is connected but mark the
sample `STALE` and omit coordinates. After the resume, a fresh sample replaces
it. If you do not want to suspend the game, simply wait for the agent to lose the
game process and confirm the link is removed instead.

### Verify two instances

Start two installations that both have the mod. Confirm `/sessions` lists two
session IDs and that `/coords` with each explicit session id reports positions
from the matching window. Moving in one instance must not change the other's
reported coordinates.

## 7. Enable chat and commands (owner opt-in)

All three features are off by default and need **both** the local and remote
switches. The launcher's **Remote** screen now shows these switches and their
effective state; it edits `agent.json` for you and tells you to restart the
agent. You can also edit the file directly.

| Feature | `agent.json` | Backend environment |
|---|---|---|
| Read chat history | `"allow_bridge": true`, `"allow_chat": true` | `SPIDER_ALLOW_CHAT=1` |
| Send chat (`/say`) | `"allow_bridge": true`, `"allow_chat_send": true` | `SPIDER_ALLOW_CHAT_SEND=1` |
| Submit Minecraft commands | `"allow_bridge": true`, `"allow_minecraft_command": true` | `SPIDER_ALLOW_MC_COMMAND=1` |
| Read-only command category | `"command_policy": {"read_only": true, ...}` | `SPIDER_MC_ALLOW_READ_ONLY=1` |
| Disruptive command category | `"command_policy": {"disruptive": true, ...}` | `SPIDER_MC_ALLOW_DISRUPTIVE=1` |
| Administrative category | `"command_policy": {"administrative": true, ...}` | `SPIDER_MC_ALLOW_ADMIN=1` |
| Require `confirm:true` for state changes | enabled by default on the backend | `SPIDER_REQUIRE_MC_CONFIRM=1` (default) |

`command_policy` is independent per category. Chat history is bounded by
`chat_retention_seconds` (30–3600) and `chat_max_messages` (20–500); the
protocol returns at most 200 messages per reply. Chat history is stored only in
memory in the game process, is cleared when collection is disabled or the bridge
disconnects, and is never written to the agent database or scraped from
`latest.log`.

`/say` refuses a message beginning with `/`; use `/mc-command` for commands.
`/mc-command` requires an explicit allowlisted command shape. Selectors (`@a`,
`@e`, ...), raw JSON, unknown commands, and chat-shaped commands (`say`, `msg`,
`tell`, `tellraw`, ...) are refused before anything reaches the client.

## 8. Verify chat history

1. Enable chat collection (launcher toggle, or `allow_chat` plus the backend
   gate), then restart the agent.
2. Join a world. Have another player send a distinctive line, or send one
   yourself in the in-game chat box.
3. Run `/chat limit:10` for the selected session. The reply must contain the
   exact line you just saw, in order, with its sender. Compare it with the
   in-game chat box.
4. Run `/chat` again immediately. A short cooldown (2 seconds) returns a
   "requested recently" message instead of spamming the game.
5. Run `/chat include_system:false` and confirm server join/leave-style system
   lines disappear. Run `/chat` again without the option and confirm they return
   (system lines are included by default). Action-bar lines stay hidden unless
   you add `include_action_bar:true`.
6. Disable chat collection, restart the agent, and run `/chat`. The reply must
   say the feature is disabled and contain **no** messages, not old history.
7. While the game stays open, stop the agent and start it again. The new
   connection must start with an empty history. A reopened connection never
   resurrects a previous session's chat.
8. In a second Minecraft instance, send a different line. `/chat` for each
   explicit session id must return only that instance's messages.

The reply may say `collection_enabled: true` and include a `dropped` count when
the bounded buffer discarded old messages. Those are honest bounded-buffer
facts, not a complete server log.

## 9. Verify `/say` and `/mc-command`

### `/say`

1. Run `/say message:"spider check 1"` for the selected session.
2. The line must appear in the **Minecraft chat box** as a message sent by your
   player. If it only appears in Discord, the feature is broken.
3. The Discord reply must say **submitted, outcome unverified**. Vanilla chat
   has no request-ID correlation, so delivery to other players or the server is
   never claimed.
4. Send a message beginning with `/` (for example `/say message:"/op Alex"`).
   It must be refused as chat, not executed as a command.
5. Send a message containing a newline. It must be refused before queueing.
6. Quit to the title screen and run `/say` again. It must fail with "no player
   in a world" and must not claim submission.

### `/mc-command`

1. With the read-only category enabled, run `/mc-command command:list`.
   The command runs in-game and its output appears in Minecraft; the Discord
   reply says **submitted, outcome unverified** because vanilla does not
   correlate command output with the request.
2. Run a command outside the allowlist (for example `definitelynotacommand`).
   Nothing is sent; the reply names the policy refusal.
3. With only read-only enabled, run `time set day`. It must be refused as a
   disruptive category before it reaches the client.
4. Enable disruptive commands, then run `time set day` without `confirm:true`.
   It must be refused as unconfirmed. Run it again with `confirm:true`; the
   in-game time must change and the reply must remain unverified.
5. Try `op <your name>` with only read-only/disruptive enabled. It must be
   refused as administrative. Only with the administrative category explicitly
   on may it be submitted, and the server may still deny it.
6. Run `/mc-command` with an `@a` selector. It must be refused.
7. Disconnect the selected instance, then run `/mc-command`. The agent must
   reject the stale session and must not fall back to another instance.
8. Run `/command-logs` and confirm the lifecycle rows (queued, agent_received,
   submitted_unverified/rejected/unknown) for the requests you just made. Chat
   message bodies must not appear in the audit output or database.

A timeout while the game is frozen is reported as **unknown**, never as a
rejection, because the client may already have submitted the line. A refusal
before the client thread claims the action is reported as **rejected** and
nothing was sent.

## 10. Baritone and unsupported features

- `/baritone-status` reports whether a Baritone jar was found in the instance's
  `mods` folder. `Integration` is `unavailable`: this build compiles no Baritone
  API and sends no Baritone command.
- There is no HUD overlay, no teleport control, no arbitrary command executor,
  and no shell.
- `/restart-agent`, `/pause`, and `/resume` remain unsupported and say so.

Do not treat a detected Baritone jar as proof that Baritone is running or that
its settings are safe. It is file presence only.

## 11. Clean shutdown

Close Minecraft normally. The mod sends `bye`, stops its daemon thread, clears
the in-memory chat buffer, and the agent removes the link and its telemetry.
Then stop the agent through the launcher (or let it exit); it deletes
`bridge.json`.

Check that no bridge file is left behind:

```powershell
Test-Path "$env:LOCALAPPDATA\SpiderClient\SpiderClient\data\bridge.json"
```

Expected after a clean agent exit: `False`.

## 12. If something does not work

| Symptom | Likely cause | Check |
|---|---|---|
| Minecraft does not start | Wrong game version, missing Loader, or Java mismatch | `logs/latest.log` for a Fabric/Loader error; the JAR targets 1.21.1 and Java 21 |
| Mod loaded, never connects | Agent not running, or `allow_bridge` disabled | `bridge.json` exists; `agent-status.json` has `bridge_enabled: true` |
| `unauthorized` in the mod log | Stale or edited `bridge.json` | Restart the agent so it publishes a new endpoint; the mod re-reads it |
| `unknown_session` | The game was not started as a process the agent tracks | Start the instance through the launcher; confirm `/sessions` lists it |
| `session_mismatch` | The mod's game directory differs from the tracked one | Use one instance per game directory; do not copy the JAR between running instances |
| `protocol_mismatch` | Mod and agent are from different builds | Both sides must speak bridge protocol 2 |
| `/coords` says the backend policy is off | The backend gate is not `1` | The owner sets it there; never send a token to the gaming PC |
| `/chat` says disabled | `allow_chat` or `SPIDER_ALLOW_CHAT` is off | Check the launcher Remote screen's effective state; restart the agent after a local change |
| `/chat` returns no history although enabled | The mod connected after the messages were received, or the buffer was cleared | Collection only starts when the agent's welcome arrives; old lines are never reconstructed |
| `/say` is refused | `allow_chat_send`/backend gate off, the text starts with `/`, contains a control character, or no player is in a world | The reply names the refusal; check the gates |
| `/mc-command` is refused | Master switch, category switch, confirmation, allowlist, or server permission | Compare `/settings` with the launcher's effective state |
| Command outcome is `unknown` | The game thread did not confirm within the action timeout | Treat it as possibly submitted; `/command-logs` records the outcome honestly |
| Coordinates look frozen | The client is not ticking (paused/suspended) or the sample is stale | `/status` shows `STALE`; resume the game |

## 13. What has and has not been verified

Verified automatically in this repository:

- Rust: loopback listener integration tests, authentication, protocol mismatch,
  session binding by PID and game directory, two simultaneous sessions, session
  exit, stale samples, rate limiting, size limits, live request/reply, a chat
  round trip with session attribution, action replies for submitted/uncertain/
  rejected, and rejection of a position without a world.
- Rust: strict command grammar and category policy, chat validation, Discord
  gating/queueing/confirmation, execution formatting that never calls an
  uncertain outcome a rejection, and command-audit lifecycle/scoping with chat
  bodies absent from persistence.
- Java: payload encoding, no-position-without-world, bounds, framing, a real
  socket conversation with a fake agent using the documented messages, the
  bounded chat buffer, and the action queue's ordering/cancellation/timeout
  rules.
- The mod builds against the real Minecraft 1.21.1 Mojang-mapped artifact, and
  the client tick/close and `ChatListener` mixin targets were checked against
  the actual mapped and intermediary class files with `javap`.

Not verified here:

- A live Minecraft client was not launched in this development environment, so
  the final in-game handshake, mixin injection, chat receipt, chat submission,
  and command execution have not been observed end to end.
- No real Discord interaction was executed (no bot token is available to this
  environment, by design).
- Baritone integration is detection only and was not tested against a running
  Baritone build.

Completing sections 3 through 11 on a real Windows PC closes the first two gaps.
