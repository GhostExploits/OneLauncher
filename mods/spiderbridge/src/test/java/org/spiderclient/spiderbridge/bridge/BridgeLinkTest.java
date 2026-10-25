package org.spiderclient.spiderbridge.bridge;

import com.google.gson.JsonObject;
import com.google.gson.JsonParser;
import org.junit.jupiter.api.Test;
import org.spiderclient.spiderbridge.game.ActionQueue;
import org.spiderclient.spiderbridge.game.ChatBuffer;
import org.spiderclient.spiderbridge.game.Telemetry;

import java.io.BufferedInputStream;
import java.io.BufferedOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.InetAddress;
import java.net.ServerSocket;
import java.net.Socket;
import java.time.Instant;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.concurrent.atomic.AtomicReference;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

/**
 * Exercises the real socket transport against a fake agent on loopback. This
 * proves the wire format and threading, not the Minecraft integration: no game
 * is started here, so action submission ends at the queue boundary.
 */
class BridgeLinkTest {
    private static final String SECRET = "cd".repeat(32);

    private static Telemetry sample() {
        return new Telemetry("1.21.1", "fabric", "0.20.1", "0.1.0",
                Telemetry.GameState.IN_WORLD,
                new Telemetry.Player("Steve", 19.0f, 18),
                new Telemetry.World("minecraft:overworld"),
                new Telemetry.Position(100.25, 70.0, -200.5, 45.0f, -10.0f),
                new Telemetry.Baritone(false, "unavailable"),
                new Telemetry.Performance(165, 6.06f),
                Instant.now(), 120);
    }

    private static Endpoint endpoint(int port) {
        return new Endpoint(1, Protocol.VERSION, port, SECRET, "test-agent", 999);
    }

    private static Messages.Hello hello() {
        return new Messages.Hello(Protocol.VERSION, SECRET, 4242, "C:/games/pack", "0.1.0", "1.21.1");
    }

    private static JsonObject readJson(InputStream in) throws IOException {
        String line = SocketLines.read(in);
        assertNotNull(line, "expected a message before the connection closed");
        return JsonParser.parseString(line).getAsJsonObject();
    }

    private static BridgeLink link(BridgeLink.SnapshotSource source, TestChat chat,
                                   ActionQueue queue) {
        return new BridgeLink(source, chat, queue, null);
    }

    @Test
    void handshakeTelemetryAndAStatusReplyUseTheDocumentedPayloads() throws Exception {
        try (ServerSocket server = new ServerSocket(0, 8, InetAddress.getLoopbackAddress())) {
            server.setSoTimeout(5_000);
            AtomicBoolean running = new AtomicBoolean(true);
            AtomicReference<BridgeLink.Result> result = new AtomicReference<>();
            BridgeLink link = link(BridgeLinkTest::sample, new TestChat(), new ActionQueue(8));

            Thread client = new Thread(() -> result.set(link.runOnce(endpoint(server.getLocalPort()), hello(), running)));
            client.start();

            try (Socket agent = server.accept()) {
                agent.setSoTimeout(5_000);
                InputStream in = new BufferedInputStream(agent.getInputStream());
                OutputStream out = new BufferedOutputStream(agent.getOutputStream());

                JsonObject handshake = readJson(in);
                assertEquals("hello", handshake.get("type").getAsString());
                assertEquals(Protocol.VERSION, handshake.get("protocol").getAsInt());
                assertEquals(SECRET, handshake.get("secret").getAsString());
                assertEquals(4242, handshake.get("pid").getAsLong());
                assertEquals("C:/games/pack", handshake.get("game_dir").getAsString());

                sendWelcome(out, true);

                JsonObject request = new JsonObject();
                request.addProperty("type", "request");
                request.addProperty("id", "req-1");
                request.addProperty("kind", "status");
                SocketLines.write(out, request);

                boolean sawTelemetry = false;
                JsonObject reply = null;
                for (int i = 0; i < 8 && reply == null; i++) {
                    JsonObject message = readJson(in);
                    if ("telemetry".equals(message.get("type").getAsString())) {
                        sawTelemetry = true;
                    } else if ("reply".equals(message.get("type").getAsString())) {
                        reply = message;
                    }
                }
                assertTrue(sawTelemetry, "the mod must stream telemetry after welcome");
                assertNotNull(reply, "the mod must answer a status request");
                assertTrue(reply.get("ok").getAsBoolean());
                assertEquals("req-1", reply.get("id").getAsString());
                assertEquals(100.25,
                        reply.getAsJsonObject("telemetry").getAsJsonObject("position").get("x").getAsDouble());

                running.set(false);
                client.join(4_000);
                assertNotNull(result.get());
                assertEquals(BridgeLink.Kind.CLOSED, result.get().kind());
            }
        }
    }

    @Test
    void aStatusRequestIsAnsweredWithTheLatestSampleWithoutBlockingTheGame() throws Exception {
        TestChat chat = new TestChat();
        try (ServerSocket server = new ServerSocket(0, 8, InetAddress.getLoopbackAddress())) {
            AtomicBoolean running = new AtomicBoolean(true);
            AtomicReference<BridgeLink.Result> result = new AtomicReference<>();
            BridgeLink link = link(BridgeLinkTest::sample, chat, new ActionQueue(8));
            Thread client = new Thread(() -> result.set(link.runOnce(endpoint(server.getLocalPort()), hello(), running)));
            client.start();

            try (Socket agent = server.accept()) {
                agent.setSoTimeout(5_000);
                InputStream in = new BufferedInputStream(agent.getInputStream());
                OutputStream out = new BufferedOutputStream(agent.getOutputStream());
                readJson(in);
                sendWelcome(out, true);
                assertTrue(chat.awaitConfigured());
                JsonObject request = new JsonObject();
                request.addProperty("type", "request");
                request.addProperty("id", "req-2");
                request.addProperty("kind", "status");
                SocketLines.write(out, request);

                JsonObject reply = awaitType(in, "reply");
                assertTrue(reply.get("ok").getAsBoolean());
                assertEquals("req-2", reply.get("id").getAsString());
                JsonObject telemetry = reply.getAsJsonObject("telemetry");
                assertEquals(100.25, telemetry.getAsJsonObject("position").get("x").getAsDouble());
                assertEquals("minecraft:overworld",
                        telemetry.getAsJsonObject("world").get("dimension").getAsString());
            }
            running.set(false);
            client.join(4_000);
        }
    }

    @Test
    void aRequestBeforeTheFirstSampleReturnsAnExplicitError() throws Exception {
        TestChat chat = new TestChat();
        try (ServerSocket server = new ServerSocket(0, 8, InetAddress.getLoopbackAddress())) {
            AtomicBoolean running = new AtomicBoolean(true);
            AtomicReference<BridgeLink.Result> result = new AtomicReference<>();
            BridgeLink link = link(() -> null, chat, new ActionQueue(8));
            Thread client = new Thread(() -> result.set(link.runOnce(endpoint(server.getLocalPort()), hello(), running)));
            client.start();

            try (Socket agent = server.accept()) {
                agent.setSoTimeout(5_000);
                InputStream in = new BufferedInputStream(agent.getInputStream());
                OutputStream out = new BufferedOutputStream(agent.getOutputStream());
                readJson(in);
                sendWelcome(out, true);
                assertTrue(chat.awaitConfigured());
                JsonObject request = new JsonObject();
                request.addProperty("type", "request");
                request.addProperty("id", "req-3");
                request.addProperty("kind", "status");
                SocketLines.write(out, request);

                JsonObject reply = awaitType(in, "reply");
                assertFalse(reply.get("ok").getAsBoolean());
                assertFalse(reply.has("telemetry"));
                assertTrue(reply.get("message").getAsString().contains("sample"));
            }
            running.set(false);
            client.join(4_000);
        }
    }

    @Test
    void anUnknownRequestKindIsRefusedRatherThanExecuted() throws Exception {
        TestChat chat = new TestChat();
        try (ServerSocket server = new ServerSocket(0, 8, InetAddress.getLoopbackAddress())) {
            AtomicBoolean running = new AtomicBoolean(true);
            AtomicReference<BridgeLink.Result> result = new AtomicReference<>();
            BridgeLink link = link(BridgeLinkTest::sample, chat, new ActionQueue(8));
            Thread client = new Thread(() -> result.set(link.runOnce(endpoint(server.getLocalPort()), hello(), running)));
            client.start();

            try (Socket agent = server.accept()) {
                agent.setSoTimeout(5_000);
                InputStream in = new BufferedInputStream(agent.getInputStream());
                OutputStream out = new BufferedOutputStream(agent.getOutputStream());
                readJson(in);
                sendWelcome(out, true);
                assertTrue(chat.awaitConfigured());
                JsonObject request = new JsonObject();
                request.addProperty("type", "request");
                request.addProperty("id", "req-4");
                request.addProperty("kind", "run_command");
                SocketLines.write(out, request);

                JsonObject reply = awaitType(in, "reply");
                assertFalse(reply.get("ok").getAsBoolean());
                assertTrue(reply.get("message").getAsString().contains("run_command"));
            }
            running.set(false);
            client.join(4_000);
        }
    }

    @Test
    void chatHistoryComesFromTheBoundedBufferWithTheSessionId() throws Exception {
        TestChat chat = new TestChat();
        try (ServerSocket server = new ServerSocket(0, 8, InetAddress.getLoopbackAddress())) {
            AtomicBoolean running = new AtomicBoolean(true);
            AtomicReference<BridgeLink.Result> result = new AtomicReference<>();
            BridgeLink link = link(BridgeLinkTest::sample, chat, new ActionQueue(8));
            Thread client = new Thread(() -> result.set(link.runOnce(endpoint(server.getLocalPort()), hello(), running)));
            client.start();

            try (Socket agent = server.accept()) {
                agent.setSoTimeout(5_000);
                InputStream in = new BufferedInputStream(agent.getInputStream());
                OutputStream out = new BufferedOutputStream(agent.getOutputStream());
                readJson(in);
                sendWelcome(out, true);
                assertTrue(chat.awaitConfigured());

                assertTrue(chat.buffer.add(ChatBuffer.KIND_CHAT, "Alex", "hello world"));
                assertTrue(chat.buffer.add(ChatBuffer.KIND_SYSTEM, null, "joined the game"));

                JsonObject payload = new JsonObject();
                payload.addProperty("limit", 10);
                payload.addProperty("include_system", false);
                payload.addProperty("include_action_bar", false);
                JsonObject request = new JsonObject();
                request.addProperty("type", "request");
                request.addProperty("id", "req-chat");
                request.addProperty("kind", "chat");
                request.add("payload", payload);
                SocketLines.write(out, request);

                JsonObject reply = awaitType(in, "reply");
                assertTrue(reply.get("ok").getAsBoolean());
                JsonObject history = reply.getAsJsonObject("data").getAsJsonObject("chat");
                assertEquals("sess-abc", history.get("session_id").getAsString());
                assertTrue(history.get("collection_enabled").getAsBoolean());
                assertEquals(1, history.getAsJsonArray("messages").size(),
                        "system messages were excluded by the request");
                JsonObject message = history.getAsJsonArray("messages").get(0).getAsJsonObject();
                assertEquals("chat", message.get("kind").getAsString());
                assertEquals("Alex", message.get("sender").getAsString());
                assertEquals("hello world", message.get("text").getAsString());
            }
            running.set(false);
            client.join(4_000);
        }
    }

    @Test
    void chatSendIsQueuedForTheClientThreadInsteadOfBeingEchoed() throws Exception {
        TestChat chat = new TestChat();
        ActionQueue queue = new ActionQueue(8);
        AtomicBoolean consumerRunning = new AtomicBoolean(true);
        Thread consumer = new Thread(() -> {
            while (consumerRunning.get()) {
                queue.drain(pending -> ActionQueue.Outcome.submitted(
                        "ClientPacketListener.sendChat accepted the line; delivery is not verified"));
                try {
                    Thread.sleep(10);
                } catch (InterruptedException interrupted) {
                    return;
                }
            }
        }, "test-client-thread");
        consumer.setDaemon(true);
        consumer.start();

        try (ServerSocket server = new ServerSocket(0, 8, InetAddress.getLoopbackAddress())) {
            AtomicBoolean running = new AtomicBoolean(true);
            AtomicReference<BridgeLink.Result> result = new AtomicReference<>();
            BridgeLink link = link(BridgeLinkTest::sample, chat, queue);
            Thread client = new Thread(() -> result.set(link.runOnce(endpoint(server.getLocalPort()), hello(), running)));
            client.start();

            try (Socket agent = server.accept()) {
                agent.setSoTimeout(5_000);
                InputStream in = new BufferedInputStream(agent.getInputStream());
                OutputStream out = new BufferedOutputStream(agent.getOutputStream());
                readJson(in);
                sendWelcome(out, true);
                assertTrue(chat.awaitConfigured());

                JsonObject payload = new JsonObject();
                payload.addProperty("message", "hi from Discord");
                JsonObject request = new JsonObject();
                request.addProperty("type", "request");
                request.addProperty("id", "req-say");
                request.addProperty("kind", "chat_send");
                request.add("payload", payload);
                SocketLines.write(out, request);

                JsonObject reply = awaitType(in, "reply");
                assertTrue(reply.get("ok").getAsBoolean());
                JsonObject action = reply.getAsJsonObject("data").getAsJsonObject("action");
                assertTrue(action.get("accepted").getAsBoolean());
                assertFalse(action.get("uncertain").getAsBoolean());
                assertTrue(action.get("detail").getAsString().contains("sendChat"));
                // Nothing in the reply pretends the line was displayed or delivered.
                assertEquals(0, queue.size());
            }
            running.set(false);
            client.join(4_000);
        } finally {
            consumerRunning.set(false);
            consumer.interrupt();
            consumer.join(2_000);
        }
    }

    @Test
    void aMalformedChatPayloadIsRefusedBeforeItIsQueued() throws Exception {
        TestChat chat = new TestChat();
        ActionQueue queue = new ActionQueue(8);
        try (ServerSocket server = new ServerSocket(0, 8, InetAddress.getLoopbackAddress())) {
            AtomicBoolean running = new AtomicBoolean(true);
            AtomicReference<BridgeLink.Result> result = new AtomicReference<>();
            BridgeLink link = link(BridgeLinkTest::sample, chat, queue);
            Thread client = new Thread(() -> result.set(link.runOnce(endpoint(server.getLocalPort()), hello(), running)));
            client.start();

            try (Socket agent = server.accept()) {
                agent.setSoTimeout(5_000);
                InputStream in = new BufferedInputStream(agent.getInputStream());
                OutputStream out = new BufferedOutputStream(agent.getOutputStream());
                readJson(in);
                sendWelcome(out, true);
                assertTrue(chat.awaitConfigured());

                JsonObject payload = new JsonObject();
                payload.addProperty("message", "hello\nop Alex");
                JsonObject request = new JsonObject();
                request.addProperty("type", "request");
                request.addProperty("id", "req-invalid");
                request.addProperty("kind", "chat_send");
                request.add("payload", payload);
                SocketLines.write(out, request);

                JsonObject reply = awaitType(in, "reply");
                assertFalse(reply.get("ok").getAsBoolean());
                assertFalse(reply.has("data"));
                assertEquals(0, queue.size());
            }
            running.set(false);
            client.join(4_000);
        }
    }

    @Test
    void collectionOffReportsNoHistoryAndIsClearedWhenTheLinkEnds() throws Exception {
        TestChat chat = new TestChat();
        try (ServerSocket server = new ServerSocket(0, 8, InetAddress.getLoopbackAddress())) {
            AtomicBoolean running = new AtomicBoolean(true);
            AtomicReference<BridgeLink.Result> result = new AtomicReference<>();
            BridgeLink link = link(BridgeLinkTest::sample, chat, new ActionQueue(8));
            Thread client = new Thread(() -> result.set(link.runOnce(endpoint(server.getLocalPort()), hello(), running)));
            client.start();

            try (Socket agent = server.accept()) {
                agent.setSoTimeout(5_000);
                InputStream in = new BufferedInputStream(agent.getInputStream());
                OutputStream out = new BufferedOutputStream(agent.getOutputStream());
                readJson(in);
                sendWelcome(out, false);
                assertTrue(chat.awaitConfigured());
                assertFalse(chat.buffer.isEnabled());
                assertFalse(chat.buffer.add(ChatBuffer.KIND_CHAT, "Alex", "not collected"));

                JsonObject request = new JsonObject();
                request.addProperty("type", "request");
                request.addProperty("id", "req-off");
                request.addProperty("kind", "chat");
                SocketLines.write(out, request);

                JsonObject reply = awaitType(in, "reply");
                assertTrue(reply.get("ok").getAsBoolean());
                JsonObject history = reply.getAsJsonObject("data").getAsJsonObject("chat");
                assertFalse(history.get("collection_enabled").getAsBoolean());
                assertEquals(0, history.getAsJsonArray("messages").size());
            }
            client.join(4_000);
            assertFalse(chat.buffer.isEnabled(), "history must not outlive the bridge connection");
        }
    }

    @Test
    void anAuthenticationErrorIsReportedAsARejection() throws Exception {
        TestChat chat = new TestChat();
        try (ServerSocket server = new ServerSocket(0, 8, InetAddress.getLoopbackAddress())) {
            AtomicBoolean running = new AtomicBoolean(true);
            AtomicReference<BridgeLink.Result> result = new AtomicReference<>();
            BridgeLink link = link(BridgeLinkTest::sample, chat, new ActionQueue(8));
            Thread client = new Thread(() -> result.set(link.runOnce(endpoint(server.getLocalPort()), hello(), running)));
            client.start();

            try (Socket agent = server.accept()) {
                agent.setSoTimeout(5_000);
                InputStream in = new BufferedInputStream(agent.getInputStream());
                OutputStream out = new BufferedOutputStream(agent.getOutputStream());
                readJson(in);
                JsonObject error = new JsonObject();
                error.addProperty("type", "error");
                error.addProperty("code", "unauthorized");
                error.addProperty("message", "the bridge credential was rejected");
                SocketLines.write(out, error);
            }
            client.join(4_000);
            assertNotNull(result.get());
            assertEquals(BridgeLink.Kind.REJECTED, result.get().kind());
            assertFalse(result.get().established());
            assertTrue(result.get().detail().contains("unauthorized"));
        }
    }

    @Test
    void aProtocolMismatchIsRejectedBeforeTelemetryIsSent() throws Exception {
        TestChat chat = new TestChat();
        try (ServerSocket server = new ServerSocket(0, 8, InetAddress.getLoopbackAddress())) {
            AtomicBoolean running = new AtomicBoolean(true);
            AtomicReference<BridgeLink.Result> result = new AtomicReference<>();
            BridgeLink link = link(BridgeLinkTest::sample, chat, new ActionQueue(8));
            Thread client = new Thread(() -> result.set(link.runOnce(endpoint(server.getLocalPort()), hello(), running)));
            client.start();

            try (Socket agent = server.accept()) {
                agent.setSoTimeout(5_000);
                InputStream in = new BufferedInputStream(agent.getInputStream());
                OutputStream out = new BufferedOutputStream(agent.getOutputStream());
                readJson(in);
                JsonObject welcome = new JsonObject();
                welcome.addProperty("type", "welcome");
                welcome.addProperty("protocol", Protocol.VERSION + 1);
                welcome.addProperty("session_id", "sess-abc");
                welcome.addProperty("chat_collection", false);
                welcome.addProperty("chat_retention_seconds", 300);
                welcome.addProperty("chat_max_messages", 200);
                SocketLines.write(out, welcome);
            }
            client.join(4_000);
            assertNotNull(result.get());
            assertEquals(BridgeLink.Kind.REJECTED, result.get().kind());
        }
    }

    @Test
    void anAgentThatDisappearsMidSessionRequestsAReconnect() throws Exception {
        TestChat chat = new TestChat();
        try (ServerSocket server = new ServerSocket(0, 8, InetAddress.getLoopbackAddress())) {
            AtomicBoolean running = new AtomicBoolean(true);
            AtomicReference<BridgeLink.Result> result = new AtomicReference<>();
            BridgeLink link = link(BridgeLinkTest::sample, chat, new ActionQueue(8));
            Thread client = new Thread(() -> result.set(link.runOnce(endpoint(server.getLocalPort()), hello(), running)));
            client.start();

            try (Socket agent = server.accept()) {
                agent.setSoTimeout(5_000);
                InputStream in = new BufferedInputStream(agent.getInputStream());
                OutputStream out = new BufferedOutputStream(agent.getOutputStream());
                readJson(in);
                sendWelcome(out, true);
                readJson(in); // the first telemetry message
            }
            client.join(4_000);
            assertNotNull(result.get());
            assertEquals(BridgeLink.Kind.RECONNECT, result.get().kind());
            assertTrue(result.get().established());
        }
    }

    private static void sendWelcome(OutputStream out, boolean chatCollection) throws IOException {
        JsonObject welcome = new JsonObject();
        welcome.addProperty("type", "welcome");
        welcome.addProperty("protocol", Protocol.VERSION);
        welcome.addProperty("session_id", "sess-abc");
        welcome.addProperty("chat_collection", chatCollection);
        welcome.addProperty("chat_retention_seconds", 300);
        welcome.addProperty("chat_max_messages", 200);
        SocketLines.write(out, welcome);
    }

    private static JsonObject awaitType(InputStream in, String type) throws IOException {
        for (int i = 0; i < 8; i++) {
            JsonObject message = readJson(in);
            if (type.equals(message.get("type").getAsString())) {
                return message;
            }
        }
        throw new AssertionError("did not receive a '" + type + "' message");
    }

    /** A real buffer behind the bridge's narrow chat interface. */
    private static final class TestChat implements BridgeLink.ChatSource {
        private final ChatBuffer buffer = new ChatBuffer();
        private final CountDownLatch configured = new CountDownLatch(1);

        @Override
        public void configure(boolean enabled, int retentionSeconds, int maxMessages) {
            buffer.configure(enabled, retentionSeconds, maxMessages);
            configured.countDown();
        }

        @Override
        public void disable() {
            buffer.disable();
        }

        @Override
        public JsonObject snapshot(String sessionId, int limit, boolean includeSystem,
                                   boolean includeActionBar) {
            return buffer.snapshotJson(sessionId, limit, includeSystem, includeActionBar);
        }

        boolean awaitConfigured() throws InterruptedException {
            return configured.await(4, TimeUnit.SECONDS);
        }
    }
}
