package org.spiderclient.spiderbridge.bridge;

import com.google.gson.JsonObject;
import org.spiderclient.spiderbridge.game.ActionQueue;
import org.spiderclient.spiderbridge.game.Telemetry;

import java.io.BufferedInputStream;
import java.io.BufferedOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.InetSocketAddress;
import java.net.Socket;
import java.net.SocketTimeoutException;
import java.util.concurrent.atomic.AtomicBoolean;

/**
 * One connection to the local agent.
 *
 * <p>This class contains no Minecraft types and never touches the render
 * thread: it runs on the bridge's own daemon thread. {@link SnapshotSource}
 * exposes the last immutable sample captured by the tick hook, so a request is
 * answered without blocking the game.
 */
public final class BridgeLink {
    public interface SnapshotSource {
        /** The latest sample, or {@code null} before the first tick produced one. */
        Telemetry latest();
    }

    /**
     * The client-side chat buffer, seen from the bridge thread. The welcome
     * message decides whether collection is enabled; every reply is served from
     * the same bounded buffer.
     */
    public interface ChatSource {
        void configure(boolean enabled, int retentionSeconds, int maxMessages);

        void disable();

        JsonObject snapshot(String sessionId, int limit, boolean includeSystem,
                            boolean includeActionBar);
    }

    public interface Logger {
        void info(String message);

        void debug(String message);
    }

    public enum Kind {
        /** The mod asked to stop; the agent saw a clean bye. */
        CLOSED,
        /** Transient problem; reconnect. */
        RECONNECT,
        /** The secret or protocol was refused; retry slowly. */
        REJECTED
    }

    public record Result(Kind kind, String detail, boolean established) {
    }

    private static final Logger SILENT = new Logger() {
        @Override
        public void info(String message) {
        }

        @Override
        public void debug(String message) {
        }
    };

    private final SnapshotSource source;
    private final ChatSource chat;
    private final ActionQueue actions;
    private final Logger logger;

    public BridgeLink(SnapshotSource source, ChatSource chat, ActionQueue actions, Logger logger) {
        this.source = source;
        this.chat = chat;
        this.actions = actions;
        this.logger = logger == null ? SILENT : logger;
    }

    public Result runOnce(Endpoint endpoint, Messages.Hello hello, AtomicBoolean running) {
        if (endpoint.protocol() != Protocol.VERSION) {
            return new Result(Kind.REJECTED, "bridge protocol " + endpoint.protocol()
                    + " does not match this mod's protocol " + Protocol.VERSION, false);
        }
        try (Socket socket = new Socket()) {
            socket.connect(new InetSocketAddress("127.0.0.1", endpoint.port()), Protocol.CONNECT_TIMEOUT_MS);
            socket.setTcpNoDelay(true);
            socket.setSoTimeout(Protocol.HANDSHAKE_TIMEOUT_MS);
            OutputStream out = new BufferedOutputStream(socket.getOutputStream());
            InputStream in = new BufferedInputStream(socket.getInputStream());

            SocketLines.write(out, Messages.hello(hello));
            String line = SocketLines.read(in);
            if (line == null) {
                return new Result(Kind.RECONNECT, "the agent closed the connection during the handshake", false);
            }
            Messages.ServerMessage first = Messages.parseServer(line);
            if (first instanceof Messages.Error error) {
                logger.info("agent refused the bridge connection: " + error.code() + " - " + error.message());
                return new Result(Kind.REJECTED, error.code() + ": " + error.message(), false);
            }
            if (!(first instanceof Messages.Welcome welcome)) {
                return new Result(Kind.REJECTED, "the agent did not send a welcome message", false);
            }
            if (welcome.protocol() != Protocol.VERSION) {
                return new Result(Kind.REJECTED, "the agent speaks bridge protocol " + welcome.protocol()
                        + " but this mod speaks " + Protocol.VERSION, false);
            }
            logger.info("connected to session " + welcome.sessionId() + " for " + endpoint.describe());

            // Collection is disabled unless the owner opted in, and it is
            // cleared as soon as this connection ends: history never outlives
            // the session that produced it.
            chat.configure(welcome.chatCollection(), welcome.chatRetentionSeconds(),
                    welcome.chatMaxMessages());

            try {
                socket.setSoTimeout(Protocol.READ_TIMEOUT_MS);
                Telemetry lastSent = null;
                long lastSentAt = 0;
                long windowStart = System.currentTimeMillis();
                int windowCount = 0;

                while (running.get()) {
                    long now = System.currentTimeMillis();
                    Telemetry latest = source.latest();
                    if (latest != null
                            && (latest != lastSent || now - lastSentAt >= Protocol.HEARTBEAT_INTERVAL_MS)
                            && now - lastSentAt >= Protocol.MIN_MESSAGE_INTERVAL_MS) {
                        SocketLines.write(out, Messages.telemetry(latest));
                        lastSent = latest;
                        lastSentAt = now;
                    }

                    String incoming;
                    try {
                        incoming = SocketLines.read(in);
                    } catch (SocketTimeoutException timeout) {
                        continue;
                    }
                    if (incoming == null) {
                        return new Result(Kind.RECONNECT, "the agent closed the connection", true);
                    }
                    now = System.currentTimeMillis();
                    if (now - windowStart >= 1_000) {
                        windowStart = now;
                        windowCount = 0;
                    } else if (++windowCount > 40) {
                        return new Result(Kind.RECONNECT, "the agent sent more than 40 messages in one second", true);
                    }

                    Messages.ServerMessage message = Messages.parseServer(incoming);
                    if (message instanceof Messages.Request request) {
                        SocketLines.write(out, handleRequest(request, welcome));
                    } else if (message instanceof Messages.Welcome) {
                        return new Result(Kind.REJECTED, "the agent sent a second welcome message", true);
                    } else if (message instanceof Messages.Error error) {
                        return new Result(Kind.REJECTED, error.code() + ": " + error.message(), true);
                    }
                }
            } finally {
                chat.disable();
            }

            try {
                SocketLines.write(out, Messages.bye());
            } catch (IOException ignored) {
                // The agent may already have closed the socket during shutdown.
            }
            return new Result(Kind.CLOSED, "stopped", true);
        } catch (IOException | RuntimeException err) {
            if (!running.get()) {
                return new Result(Kind.CLOSED, "stopped", true);
            }
            return new Result(Kind.RECONNECT, "bridge connection failed: " + safeMessage(err), false);
        }
    }

    private JsonObject handleRequest(Messages.Request request, Messages.Welcome welcome) {
        return switch (request.kind()) {
            case "status" -> {
                Telemetry sample = source.latest();
                yield sample == null
                        ? Messages.reply(request.id(), false,
                        "the game has not produced a telemetry sample yet", null)
                        : Messages.reply(request.id(), true, null, sample);
            }
            case "chat" -> {
                try {
                    JsonObject payload = request.payload();
                    int limit = Messages.payloadInt(payload, "limit", 50, 1,
                            Protocol.MAX_CHAT_MESSAGES);
                    boolean includeSystem = Messages.payloadBoolean(payload, "include_system", true);
                    boolean includeActionBar = Messages.payloadBoolean(payload, "include_action_bar",
                            false);
                    JsonObject data = new JsonObject();
                    data.add("chat", chat.snapshot(welcome.sessionId(), limit, includeSystem,
                            includeActionBar));
                    yield Messages.replyData(request.id(), true, null, data);
                } catch (RuntimeException err) {
                    yield Messages.reply(request.id(), false,
                            "the chat buffer is unavailable: " + safeMessage(err), null);
                }
            }
            case "chat_send" -> action(request, "chat_send", "message",
                    Protocol.MAX_CHAT_MESSAGE_CHARS);
            case "command" -> action(request, "command", "command", Protocol.MAX_COMMAND_CHARS);
            default -> Messages.reply(request.id(), false,
                    "this mod does not support the request kind '" + request.kind() + "'", null);
        };
    }

    /**
     * Hand one action to the client thread. A timeout is reported as uncertain,
     * never as a rejection, because the client may already have run it.
     */
    private JsonObject action(Messages.Request request, String kind, String field, int maxChars) {
        String value;
        try {
            value = Messages.payloadString(request.payload(), field, maxChars);
        } catch (RuntimeException err) {
            return Messages.reply(request.id(), false, safeMessage(err), null);
        }
        ActionQueue.Pending pending = actions.submit(kind, value);
        ActionQueue.Outcome outcome = actions.await(pending, Protocol.ACTION_TIMEOUT_MS);
        return Messages.actionReply(request.id(), outcome.accepted(), outcome.uncertain(),
                outcome.detail());
    }

    /** Exception text is bounded and has no secret: this class never holds one in a throwable. */
    private static String safeMessage(Throwable err) {
        String text = err.getMessage();
        if (text == null || text.isBlank()) {
            text = err.getClass().getSimpleName();
        }
        text = text.replaceAll("[\\r\\n\\t]", " ");
        return text.length() <= 200 ? text : text.substring(0, 200);
    }
}
