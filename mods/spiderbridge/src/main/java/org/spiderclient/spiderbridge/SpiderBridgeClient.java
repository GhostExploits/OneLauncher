package org.spiderclient.spiderbridge;

import net.fabricmc.api.ClientModInitializer;
import net.fabricmc.loader.api.FabricLoader;
import net.minecraft.client.Minecraft;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import org.spiderclient.spiderbridge.bridge.BridgeLink;
import org.spiderclient.spiderbridge.bridge.BridgePaths;
import org.spiderclient.spiderbridge.bridge.BridgeService;
import org.spiderclient.spiderbridge.bridge.Endpoint;
import org.spiderclient.spiderbridge.bridge.Messages;
import org.spiderclient.spiderbridge.bridge.Protocol;
import org.spiderclient.spiderbridge.game.ActionQueue;
import org.spiderclient.spiderbridge.game.ChatBuffer;
import org.spiderclient.spiderbridge.game.GameActions;
import org.spiderclient.spiderbridge.game.GameSampler;

import java.io.File;
import java.nio.file.Path;
import java.util.concurrent.atomic.AtomicInteger;

/**
 * Client entry point.
 *
 * <p>Ordering that matters: this initializer only starts the background
 * thread. The tick hook captures a sample at most once per second and hands it
 * to the service through an atomic reference. No socket code ever runs on the
 * render thread.
 */
public final class SpiderBridgeClient implements ClientModInitializer {
    private static final Logger LOG = LoggerFactory.getLogger("spiderbridge");
    private static final AtomicInteger tickCounter = new AtomicInteger();
    private static volatile GameSampler sampler;
    private static volatile BridgeService service;
    private static volatile GameActions actions;
    private static volatile long lastSampleErrorLog;
    /** Captured on the client init thread so the bridge thread never touches Minecraft. */
    private static volatile long gamePid;
    private static volatile String gameDir = ".";
    private static volatile String mcVersion = "unknown";

    @Override
    public void onInitializeClient() {
        Path endpoint = BridgePaths.resolveDefault();
        String loaderVersion = FabricLoader.getInstance()
                .getModContainer("fabricloader")
                .map(container -> container.getMetadata().getVersion().getFriendlyString())
                .orElse("unknown");
        boolean baritone = FabricLoader.getInstance().isModLoaded("baritone")
                || FabricLoader.getInstance().isModLoaded("baritone-api");
        mcVersion = GameSampler.currentMinecraftVersion();
        gamePid = ProcessHandle.current().pid();
        Minecraft minecraft = Minecraft.getInstance();
        File directory = minecraft == null ? null : minecraft.gameDirectory;
        gameDir = directory == null
                ? System.getProperty("user.dir", ".")
                : directory.getAbsolutePath();
        GameSampler localSampler = new GameSampler(
                mcVersion,
                Protocol.LOADER,
                loaderVersion,
                Protocol.MOD_VERSION,
                baritone);
        sampler = localSampler;

        GameActions localActions = new GameActions(
                new ActionQueue(Protocol.MAX_PENDING_ACTIONS), new ChatBuffer());
        actions = localActions;

        BridgeService localService = new BridgeService(endpoint, SpiderBridgeClient::helloFor,
                localActions, localActions.queue(), new BridgeLink.Logger() {
            @Override
            public void info(String message) {
                LOG.info("[SpiderBridge] {}", message);
            }

            @Override
            public void debug(String message) {
                LOG.debug("[SpiderBridge] {}", message);
            }
        });
        service = localService;
        localService.start();

        Runtime.getRuntime().addShutdownHook(new Thread(() -> {
            BridgeService current = service;
            if (current != null) {
                current.stopAndJoin(2_000);
            }
        }, "SpiderBridge-shutdown"));

        LOG.info("[SpiderBridge] {}; looking for the agent at {}", localSampler.describeEnvironment(), endpoint);
    }

    /** Called from the client tick mixin. Must stay cheap and non-blocking. */
    public static void onClientTick() {
        // Action drain runs every tick: a 250 ms sample cadence must not add
        // latency to a chat line or command that is already waiting.
        GameActions localActions = actions;
        if (localActions != null) {
            try {
                localActions.drain();
            } catch (Throwable err) {
                logActionError(err);
            }
        }

        GameSampler localSampler = sampler;
        BridgeService localService = service;
        if (localSampler == null || localService == null) {
            return;
        }
        if (Math.floorMod(tickCounter.incrementAndGet(), Protocol.SAMPLE_INTERVAL_TICKS) != 0) {
            return;
        }
        try {
            localService.publish(localSampler.sample());
        } catch (Throwable err) {
            // A telemetry bug must never crash the game. Log at most once a minute.
            long now = System.currentTimeMillis();
            if (now - lastSampleErrorLog > 60_000) {
                lastSampleErrorLog = now;
                LOG.warn("[SpiderBridge] could not sample the game state", err);
            }
        }
    }

    /**
     * Called by the chat mixin on the client thread. The buffer is disabled
     * unless the agent opted in, so this is a cheap no-op otherwise.
     */
    public static void recordChat(String kind, String sender, String text) {
        GameActions localActions = actions;
        if (localActions != null) {
            localActions.recordChat(kind, sender, text);
        }
    }

    /** Called when Minecraft itself is closing. Non-blocking by design. */
    public static void onGameClose() {
        GameActions localActions = actions;
        if (localActions != null) {
            localActions.disable();
        }
        BridgeService localService = service;
        if (localService != null) {
            localService.requestStop();
        }
    }

    private static void logActionError(Throwable err) {
        long now = System.currentTimeMillis();
        if (now - lastSampleErrorLog > 60_000) {
            lastSampleErrorLog = now;
            LOG.warn("[SpiderBridge] could not run a queued game action", err);
        }
    }

    private static Messages.Hello helloFor(Endpoint endpoint) {
        // Values were captured once during client init; the bridge thread does
        // not read Minecraft state.
        return new Messages.Hello(Protocol.VERSION, endpoint.secret(), gamePid, gameDir,
                Protocol.MOD_VERSION, mcVersion);
    }
}
