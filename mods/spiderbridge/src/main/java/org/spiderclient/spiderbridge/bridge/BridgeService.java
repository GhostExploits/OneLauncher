package org.spiderclient.spiderbridge.bridge;

import org.spiderclient.spiderbridge.game.ActionQueue;
import org.spiderclient.spiderbridge.game.Telemetry;

import java.nio.file.Path;
import java.util.Optional;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.concurrent.atomic.AtomicReference;
import java.util.function.Function;

/**
 * Owns the single background bridge thread and the reconnect loop.
 *
 * <p>Nothing here runs on the Minecraft thread. The tick hook only calls
 * {@link #publish(Telemetry)}, which stores an immutable sample; the network
 * thread reads it. That keeps every socket operation off the render thread.
 */
public final class BridgeService {
    private final Path endpointPath;
    private final Function<Endpoint, Messages.Hello> hello;
    private final BridgeLink link;
    private final BridgeLink.Logger logger;
    private final AtomicReference<Telemetry> latest = new AtomicReference<>();
    private final AtomicBoolean running = new AtomicBoolean(false);
    private volatile Thread thread;
    private volatile String lastStatus = "not started";

    public BridgeService(Path endpointPath, Function<Endpoint, Messages.Hello> hello,
                         BridgeLink.ChatSource chat, ActionQueue actions, BridgeLink.Logger logger) {
        this.endpointPath = endpointPath;
        this.hello = hello;
        this.logger = logger;
        this.link = new BridgeLink(latest::get, chat, actions, logger);
    }

    public synchronized void start() {
        if (running.get()) {
            return;
        }
        running.set(true);
        lastStatus = "waiting for the agent";
        Thread worker = new Thread(this::runLoop, "SpiderBridge");
        worker.setDaemon(true);
        thread = worker;
        worker.start();
    }

    public void publish(Telemetry sample) {
        latest.set(sample);
    }

    public Telemetry latest() {
        return latest.get();
    }

    /** Non-blocking: the network thread notices within one read timeout. */
    public void requestStop() {
        running.set(false);
        lastStatus = "stopping";
    }

    /** Used by the JVM shutdown hook, which is not the game thread. */
    public void stopAndJoin(long timeoutMs) {
        requestStop();
        Thread worker = thread;
        if (worker != null) {
            try {
                worker.join(timeoutMs);
            } catch (InterruptedException err) {
                Thread.currentThread().interrupt();
            }
        }
        lastStatus = "stopped";
    }

    public boolean isRunning() {
        return running.get();
    }

    public String describe() {
        Telemetry sample = latest.get();
        String sampleText = sample == null ? "no sample yet" : sample.describeCoordinates();
        return lastStatus + "; endpoint " + endpointPath + "; " + sampleText;
    }

    private void runLoop() {
        ReconnectPolicy policy = new ReconnectPolicy();
        String lastLogged = "";
        while (running.get()) {
            Optional<Endpoint> found = EndpointFile.tryRead(endpointPath);
            if (found.isEmpty()) {
                lastStatus = "agent endpoint not found";
                logOnce(lastLogged, "waiting for the agent to publish " + endpointPath);
                lastLogged = "missing";
                if (!sleep(2_000)) {
                    break;
                }
                continue;
            }
            Endpoint endpoint = found.get();
            if (endpoint.protocol() != Protocol.VERSION) {
                lastStatus = "bridge protocol mismatch";
                String message = "the agent uses bridge protocol " + endpoint.protocol()
                        + " but this mod uses " + Protocol.VERSION;
                logOnce(lastLogged, message);
                lastLogged = message;
                if (!sleep(10_000)) {
                    break;
                }
                continue;
            }
            Messages.Hello helloMessage;
            try {
                helloMessage = hello.apply(endpoint);
            } catch (RuntimeException err) {
                lastStatus = "game state unavailable for handshake";
                logger.info("cannot build the bridge handshake yet: " + err.getMessage());
                if (!sleep(2_000)) {
                    break;
                }
                continue;
            }

            lastStatus = "connecting to " + endpoint.describe();
            BridgeLink.Result result = link.runOnce(endpoint, helloMessage, running);
            if (!running.get() || result.kind() == BridgeLink.Kind.CLOSED) {
                break;
            }
            lastStatus = result.detail();
            logOnce(lastLogged, result.detail());
            lastLogged = result.detail();
            if (result.established()) {
                policy.reset();
            }
            long delay = result.kind() == BridgeLink.Kind.REJECTED
                    ? policy.nextDelayMsForRejection()
                    : policy.nextDelayMs();
            if (!sleep(delay)) {
                break;
            }
        }
        running.set(false);
        lastStatus = "stopped";
    }

    private void logOnce(String previous, String message) {
        if (!message.equals(previous)) {
            logger.info(message);
        }
    }

    private boolean sleep(long millis) {
        long deadline = System.currentTimeMillis() + millis;
        while (running.get()) {
            long left = deadline - System.currentTimeMillis();
            if (left <= 0) {
                return true;
            }
            try {
                Thread.sleep(Math.min(left, 200));
            } catch (InterruptedException err) {
                Thread.currentThread().interrupt();
                return false;
            }
        }
        return false;
    }
}
