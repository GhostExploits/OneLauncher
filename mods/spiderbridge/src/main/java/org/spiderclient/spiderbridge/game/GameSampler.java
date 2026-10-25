package org.spiderclient.spiderbridge.game;

import net.minecraft.SharedConstants;
import net.minecraft.client.Minecraft;
import net.minecraft.client.player.LocalPlayer;
import net.minecraft.world.level.Level;

import java.lang.management.ManagementFactory;
import java.time.Instant;
import java.util.Locale;

/**
 * Reads a small, fixed set of values from the running client.
 *
 * <p>Only vanilla APIs are used. Everything here runs on the client thread, so
 * it does no I/O, no allocation-heavy iteration, and no logging per tick. The
 * caller samples once per second.
 */
public final class GameSampler {
    private final String mcVersion;
    private final String loader;
    private final String loaderVersion;
    private final String bridgeVersion;
    private final String baritoneIntegration;
    private final boolean baritoneDetected;

    public GameSampler(String mcVersion, String loader, String loaderVersion, String bridgeVersion,
                       boolean baritoneDetected) {
        this.mcVersion = mcVersion;
        this.loader = loader;
        this.loaderVersion = loaderVersion;
        this.bridgeVersion = bridgeVersion;
        this.baritoneDetected = baritoneDetected;
        // No verified public Baritone API is compiled into this build, so only
        // presence is reported. Claiming more would be a fabrication.
        this.baritoneIntegration = "unavailable";
    }

    public static String currentMinecraftVersion() {
        return SharedConstants.getCurrentVersion().getName();
    }

    public Telemetry sample() {
        Minecraft minecraft = Minecraft.getInstance();
        Level level = minecraft.level;
        LocalPlayer player = minecraft.player;

        Telemetry.GameState state;
        Telemetry.Player playerInfo = null;
        Telemetry.World world = null;
        Telemetry.Position position = null;

        if (level != null && player != null) {
            state = Telemetry.GameState.IN_WORLD;
            String name = player.getGameProfile().getName();
            if (name != null && !name.isBlank()) {
                playerInfo = new Telemetry.Player(name, health(player.getHealth()),
                        food(player.getFoodData().getFoodLevel()));
            }
            world = new Telemetry.World(level.dimension().location().toString());
            position = new Telemetry.Position(player.getX(), player.getY(), player.getZ(),
                    player.getYRot(), player.getXRot());
        } else if (level != null) {
            // A world is loaded but the player entity does not exist yet.
            state = Telemetry.GameState.NO_PLAYER;
            world = new Telemetry.World(level.dimension().location().toString());
        } else {
            state = Telemetry.GameState.MENU;
        }

        long uptime = ManagementFactory.getRuntimeMXBean().getUptime() / 1_000L;
        Telemetry.Performance performance = null;
        int fps = minecraft.getFps();
        long frameNs = minecraft.getFrameTimeNs();
        if (fps >= 0 && fps <= 20_000) {
            float frameMs = frameNs > 0 ? frameNs / 1_000_000.0f : 0.0f;
            performance = new Telemetry.Performance(fps, frameMs);
        }
        return new Telemetry(mcVersion, loader, loaderVersion, bridgeVersion, state, playerInfo, world,
                position, new Telemetry.Baritone(baritoneDetected, baritoneIntegration), performance,
                Instant.now(), uptime);
    }

    private static Float health(float value) {
        return Float.isFinite(value) && value >= 0.0f && value <= 1024.0f ? value : null;
    }

    private static Integer food(int value) {
        return value >= 0 && value <= 20 ? value : null;
    }

    /** Non-secret diagnostics line shown in the game log once at startup. */
    public String describeEnvironment() {
        return String.format(Locale.ROOT, "Minecraft %s, %s %s, bridge %s, Baritone detected=%s",
                mcVersion, loader, loaderVersion, bridgeVersion, baritoneDetected);
    }
}
