package org.spiderclient.spiderbridge.game;

import java.time.Instant;
import java.util.Locale;

/**
 * One immutable sample of game state.
 *
 * <p>Every field is either a fact the client APIs expose or absent. In
 * particular {@link #position()} is {@code null} whenever the player is not in a
 * world: this mod never substitutes zeroes for "unknown".
 */
public record Telemetry(
        String mcVersion,
        String loader,
        String loaderVersion,
        String bridgeVersion,
        GameState state,
        Player player,
        World world,
        Position position,
        Baritone baritone,
        Performance performance,
        Instant capturedAt,
        long uptimeSeconds) {

    public Telemetry {
        if (mcVersion == null || mcVersion.isBlank() || mcVersion.length() > 64) {
            throw new IllegalArgumentException("minecraft version is invalid");
        }
        if (loader == null || loader.isBlank() || loader.length() > 64) {
            throw new IllegalArgumentException("loader is invalid");
        }
        if (loaderVersion == null || loaderVersion.isBlank() || loaderVersion.length() > 64) {
            throw new IllegalArgumentException("loader version is invalid");
        }
        if (bridgeVersion == null || bridgeVersion.isBlank() || bridgeVersion.length() > 64) {
            throw new IllegalArgumentException("bridge version is invalid");
        }
        if (state == null || baritone == null || capturedAt == null) {
            throw new IllegalArgumentException("telemetry is missing a required field");
        }
        if (position != null && (world == null || state != GameState.IN_WORLD)) {
            throw new IllegalArgumentException("a position requires a loaded world");
        }
        if (player != null && (player.name() == null || player.name().isBlank())) {
            throw new IllegalArgumentException("player name is empty");
        }
    }

    public enum GameState {
        MENU("menu"),
        IN_WORLD("in_world"),
        NO_PLAYER("no_player");

        private final String wire;

        GameState(String wire) {
            this.wire = wire;
        }

        public String wire() {
            return wire;
        }
    }

    public record Player(String name, Float health, Integer foodLevel) {
    }

    public record World(String dimension) {
    }

    public record Position(double x, double y, double z, float yaw, float pitch) {
        public Position {
            if (!Double.isFinite(x) || !Double.isFinite(y) || !Double.isFinite(z)
                    || !Float.isFinite(yaw) || !Float.isFinite(pitch)) {
                throw new IllegalArgumentException("position contains a non-finite value");
            }
            if (Math.abs(x) > 30_000_000.0 || Math.abs(z) > 30_000_000.0
                    || y < -4096.0 || y > 4096.0) {
                throw new IllegalArgumentException("position is outside the playable world");
            }
        }
    }

    public record Baritone(boolean detected, String integration) {
        public Baritone {
            if (integration == null || integration.length() > 32) {
                throw new IllegalArgumentException("baritone integration label is invalid");
            }
        }
    }

    /** Read from the client's own frame counter; absent when the client does not expose it. */
    public record Performance(Integer fps, Float frameTimeMs) {
        public Performance {
            if (fps != null && (fps < 0 || fps > 20_000)) {
                throw new IllegalArgumentException("fps is out of range");
            }
            if (frameTimeMs != null && (!Float.isFinite(frameTimeMs) || frameTimeMs < 0.0f
                    || frameTimeMs > 10_000.0f)) {
                throw new IllegalArgumentException("frame time is out of range");
            }
        }
    }

    /** Human-readable coordinate line for local diagnostics, never a fabricated value. */
    public String describeCoordinates() {
        if (position == null) {
            return switch (state) {
                case MENU -> "not in a world (menu)";
                case NO_PLAYER -> "world loading, no player entity";
                default -> "no position reported";
            };
        }
        return String.format(Locale.ROOT, "X %.2f Y %.2f Z %.2f (%s)",
                position.x(), position.y(), position.z(), world == null ? "unknown dimension" : world.dimension());
    }
}
