package org.spiderclient.spiderbridge.bridge;

import com.google.gson.JsonObject;
import org.junit.jupiter.api.Test;
import org.spiderclient.spiderbridge.game.Telemetry;

import java.time.Instant;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

class MessagesTest {
    private static final String SECRET = "ab".repeat(32);

    static Telemetry sample() {
        return new Telemetry("1.21.1", "fabric", "0.20.1", "0.1.0",
                Telemetry.GameState.IN_WORLD,
                new Telemetry.Player("Alex", 20.0f, 20),
                new Telemetry.World("minecraft:overworld"),
                new Telemetry.Position(1.5, 64.0, 2.5, 180.0f, 12.0f),
                new Telemetry.Baritone(true, "unavailable"),
                new Telemetry.Performance(120, 8.333f),
                Instant.parse("2026-10-25T12:34:56.789Z"), 77);
    }

    @Test
    void telemetryEncodesEveryLiveFieldAndRoundTripsTimestamps() {
        JsonObject json = Messages.telemetryJson(sample());
        assertEquals(1, json.get("schema").getAsInt());
        assertEquals("in_world", json.get("state").getAsString());
        assertEquals("1.21.1", json.getAsJsonObject("game").get("mc_version").getAsString());
        assertEquals("minecraft:overworld",
                json.getAsJsonObject("world").get("dimension").getAsString());
        assertEquals(1.5, json.getAsJsonObject("position").get("x").getAsDouble());
        assertEquals("Alex", json.getAsJsonObject("player").get("name").getAsString());
        assertEquals(120, json.getAsJsonObject("performance").get("fps").getAsInt());
        assertEquals(Instant.parse("2026-10-25T12:34:56.789Z"),
                Messages.parseTimestamp(json.get("captured_at").getAsString()));
        assertTrue(json.getAsJsonObject("baritone").get("detected").getAsBoolean());
    }

    @Test
    void noWorldOmitsPositionAndDimensionCompletely() {
        Telemetry menu = new Telemetry("1.21.1", "fabric", "0.20.1", "0.1.0",
                Telemetry.GameState.MENU, null, null, null,
                new Telemetry.Baritone(false, "unavailable"), null, Instant.now(), 3);
        JsonObject json = Messages.telemetryJson(menu);
        assertFalse(json.has("position"));
        assertFalse(json.has("world"));
        assertFalse(json.has("player"));
        assertFalse(json.has("performance"));
        assertEquals("menu", json.get("state").getAsString());
    }

    @Test
    void serverMessagesAreParsedStrictly() {
        Messages.ServerMessage welcome = Messages.parseServer(
                "{\"type\":\"welcome\",\"protocol\":2,\"session_id\":\"abc\","
                        + "\"chat_collection\":true,\"chat_retention_seconds\":300,"
                        + "\"chat_max_messages\":200}");
        assertTrue(welcome instanceof Messages.Welcome);
        assertEquals("abc", ((Messages.Welcome) welcome).sessionId());
        assertTrue(((Messages.Welcome) welcome).chatCollection());

        Messages.ServerMessage request = Messages.parseServer(
                "{\"type\":\"request\",\"id\":\"req-1\",\"kind\":\"status\"}");
        assertTrue(request instanceof Messages.Request);
        assertEquals("status", ((Messages.Request) request).kind());

        Messages.ServerMessage error = Messages.parseServer(
                "{\"type\":\"error\",\"code\":\"unauthorized\",\"message\":\"no\"}");
        assertTrue(error instanceof Messages.Error);
        assertEquals("unauthorized", ((Messages.Error) error).code());

        assertThrows(IllegalArgumentException.class,
                () -> Messages.parseServer("{\"type\":\"teleport\"}"));
        assertThrows(IllegalArgumentException.class,
                () -> Messages.parseServer("{\"type\":\"request\",\"id\":\"x\\n\",\"kind\":\"status\"}"));
        assertThrows(IllegalArgumentException.class, () -> Messages.parseServer("[]"));
        // A welcome without the chat decision is refused: the mod must never
        // guess whether the owner opted in.
        assertThrows(IllegalArgumentException.class,
                () -> Messages.parseServer("{\"type\":\"welcome\",\"protocol\":2,\"session_id\":\"abc\"}"));
        assertThrows(IllegalArgumentException.class,
                () -> Messages.parseServer(
                        "{\"type\":\"welcome\",\"protocol\":2,\"session_id\":\"abc\","
                                + "\"chat_collection\":true,\"chat_retention_seconds\":5,"
                                + "\"chat_max_messages\":200}"));
    }

    @Test
    void payloadAccessorsAreBoundedAndRejectControlCharacters() {
        JsonObject payload = new JsonObject();
        payload.addProperty("message", "hello");
        payload.addProperty("limit", 900);
        payload.addProperty("include_system", false);
        assertEquals("hello", Messages.payloadString(payload, "message", 256));
        assertEquals(200, Messages.payloadInt(payload, "limit", 50, 1, 200));
        assertEquals(50, Messages.payloadInt(new JsonObject(), "limit", 50, 1, 200));
        assertFalse(Messages.payloadBoolean(payload, "include_system", true));
        assertTrue(Messages.payloadBoolean(new JsonObject(), "include_system", true));

        assertThrows(IllegalArgumentException.class,
                () -> Messages.payloadString(payload, "missing", 256));
        payload.addProperty("bad", "line\nbreak");
        assertThrows(IllegalArgumentException.class,
                () -> Messages.payloadString(payload, "bad", 256));
        payload.addProperty("long", "x".repeat(300));
        assertThrows(IllegalArgumentException.class,
                () -> Messages.payloadString(payload, "long", 256));
    }

    @Test
    void helloRequiresAWellFormedCredentialAndProcess() {
        Messages.Hello hello = new Messages.Hello(Protocol.VERSION, SECRET, 1234, "C:/game", "0.1.0", "1.21.1");
        JsonObject json = Messages.hello(hello);
        assertEquals(SECRET, json.get("secret").getAsString());
        assertEquals(1234, json.get("pid").getAsLong());
        assertEquals("C:/game", json.get("game_dir").getAsString());

        assertThrows(IllegalArgumentException.class,
                () -> new Messages.Hello(1, "short", 1, "C:/game", "0.1.0", "1.21.1"));
        assertThrows(IllegalArgumentException.class,
                () -> new Messages.Hello(1, SECRET, 0, "C:/game", "0.1.0", "1.21.1"));
        assertThrows(IllegalArgumentException.class,
                () -> new Messages.Hello(1, SECRET, 1, "", "0.1.0", "1.21.1"));
    }

    @Test
    void anErrorReplyNeverIncludesTelemetryOrSecret() {
        JsonObject reply = Messages.reply("req-9", false, "menu\nno world", null);
        assertEquals("reply", reply.get("type").getAsString());
        assertFalse(reply.get("ok").getAsBoolean());
        assertFalse(reply.has("telemetry"));
        assertFalse(reply.get("message").getAsString().contains("\n"));
        assertFalse(reply.toString().contains(SECRET));
    }
}
