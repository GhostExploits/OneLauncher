# SpiderBridge

A client-only Fabric mod for Minecraft 1.21.1 that reports read-only game
telemetry to a local [SpiderClient agent](../../packages/spider_remote) over an
authenticated loopback socket.

- It never contacts Discord or the remote backend.
- It has no chat, command-execution, teleport, or Baritone control API.
- It reports no position when the player is not in a world. Zero is not used as
  a substitute for "unknown".
- The agent generates the endpoint credential per run; nothing secret is
  compiled into this JAR.

## Build

Requires JDK 21.

```sh
./gradlew build
```

The mod JAR is written to `build/libs/spiderbridge-0.1.0.jar`.

```sh
./gradlew test
```

Tests exercise the transport, payload bounds, and reconnect logic against a fake
loopback agent. They do not start Minecraft.

## Install

Place the JAR in a Minecraft 1.21.1 Fabric instance's `mods` folder. It requires
Fabric Loader 0.16.0 or newer and Java 21. Fabric API is not required.

Installation and full manual verification are documented in
[`docs/SPIDERBRIDGE.md`](../../docs/SPIDERBRIDGE.md). Remote behavior and the
loopback protocol are documented in [`docs/REMOTE.md`](../../docs/REMOTE.md).

## Licence

GPL-3.0-only. See [`LICENSE`](LICENSE).
