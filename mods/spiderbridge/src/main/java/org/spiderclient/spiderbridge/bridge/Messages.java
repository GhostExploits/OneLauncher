package org.spiderclient.spiderbridge.bridge;

import com.google.gson.JsonElement;
import com.google.gson.JsonObject;
import com.google.gson.JsonParser;
import org.spiderclient.spiderbridge.game.Telemetry;

import java.time.Instant;
import java.time.format.DateTimeParseException;

/** Encodes and decodes the narrow, newline-delimited JSON protocol. */
public final class Messages {
    public static final int SCHEMA = 1;
    private static final int MAX_PAYLOAD_CHARS = 8 * 1024;

    private Messages() {
    }

    public record Hello(int protocol, String secret, long pid, String gameDir, String bridgeVersion,
                        String mcVersion) {
        public Hello {
            if (protocol < 1 || secret == null || secret.length() != Protocol.SECRET_HEX_CHARS
                    || pid <= 0 || gameDir == null || gameDir.isBlank() || gameDir.length() > 4096
                    || bridgeVersion == null || bridgeVersion.isBlank()
                    || mcVersion == null || mcVersion.isBlank()) {
                throw new IllegalArgumentException("hello fields are invalid");
            }
        }
    }

    public sealed interface ServerMessage permits Welcome, Request, Error {
    }

    /**
     * The agent's acceptance of this connection.
     *
     * <p>Chat collection is opt-in: the agent sends the owner's decision, the
     * retention window, and the maximum message count. The mod must not buffer
     * anything unless {@code chatCollection} is true.
     */
    public record Welcome(int protocol, String sessionId, boolean chatCollection,
                          int chatRetentionSeconds, int chatMaxMessages) implements ServerMessage {
        public Welcome {
            if (sessionId == null || sessionId.isBlank() || sessionId.length() > 128) {
                throw new IllegalArgumentException("welcome session id is invalid");
            }
            if (chatRetentionSeconds < Protocol.MIN_CHAT_RETENTION_SECONDS
                    || chatRetentionSeconds > Protocol.MAX_CHAT_RETENTION_SECONDS) {
                throw new IllegalArgumentException("welcome chat retention is outside the accepted range");
            }
            if (chatMaxMessages < Protocol.MIN_CHAT_MESSAGES
                    || chatMaxMessages > Protocol.MAX_CHAT_MESSAGES_SETTING) {
                throw new IllegalArgumentException("welcome chat limit is outside the accepted range");
            }
        }
    }

    public record Request(String id, String kind, JsonObject payload) implements ServerMessage {
        public Request {
            if (payload == null) {
                payload = new JsonObject();
            }
            if (payload.toString().length() > MAX_PAYLOAD_CHARS) {
                throw new IllegalArgumentException("request payload is too large");
            }
        }
    }

    public record Error(String code, String message) implements ServerMessage {
    }

    public static JsonObject hello(Hello hello) {
        JsonObject root = new JsonObject();
        root.addProperty("type", "hello");
        root.addProperty("protocol", hello.protocol());
        root.addProperty("secret", hello.secret());
        root.addProperty("pid", hello.pid());
        root.addProperty("game_dir", hello.gameDir());
        root.addProperty("bridge_version", hello.bridgeVersion());
        root.addProperty("mc_version", hello.mcVersion());
        return root;
    }

    public static JsonObject telemetry(Telemetry telemetry) {
        JsonObject root = new JsonObject();
        root.addProperty("type", "telemetry");
        root.add("telemetry", telemetryJson(telemetry));
        return root;
    }

    public static JsonObject reply(String id, boolean ok, String message, Telemetry telemetry) {
        JsonObject root = new JsonObject();
        root.addProperty("type", "reply");
        root.addProperty("id", id);
        root.addProperty("ok", ok);
        if (message != null) {
            root.addProperty("message", bound(message));
        }
        if (telemetry != null) {
            root.add("telemetry", telemetryJson(telemetry));
        }
        return root;
    }

    /** A reply whose kind-specific body goes in the agent's {@code data} field. */
    public static JsonObject replyData(String id, boolean ok, String message, JsonObject data) {
        JsonObject root = reply(id, ok, message, null);
        if (data != null) {
            root.add("data", data);
        }
        return root;
    }

    /**
     * Report whether the client accepted an action. {@code uncertain} means the
     * request may still run: the agent must not call that a rejection.
     */
    public static JsonObject actionReply(String id, boolean accepted, boolean uncertain, String detail) {
        JsonObject action = new JsonObject();
        action.addProperty("accepted", accepted);
        action.addProperty("uncertain", uncertain);
        if (detail != null && !detail.isBlank()) {
            action.addProperty("detail", bound(detail));
        }
        JsonObject data = new JsonObject();
        data.add("action", action);
        return replyData(id, true, null, data);
    }

    /** The exact payload the agent deserializes into {@code BridgeTelemetry}. */
    public static JsonObject telemetryJson(Telemetry telemetry) {
        JsonObject root = new JsonObject();
        root.addProperty("schema", SCHEMA);

        JsonObject game = new JsonObject();
        game.addProperty("mc_version", telemetry.mcVersion());
        game.addProperty("loader", telemetry.loader());
        game.addProperty("loader_version", telemetry.loaderVersion());
        game.addProperty("bridge_version", telemetry.bridgeVersion());
        root.add("game", game);

        root.addProperty("state", telemetry.state().wire());

        if (telemetry.player() != null) {
            JsonObject player = new JsonObject();
            player.addProperty("name", telemetry.player().name());
            if (telemetry.player().health() != null) {
                player.addProperty("health", telemetry.player().health());
            }
            if (telemetry.player().foodLevel() != null) {
                player.addProperty("food_level", telemetry.player().foodLevel());
            }
            root.add("player", player);
        }
        if (telemetry.world() != null) {
            JsonObject world = new JsonObject();
            world.addProperty("dimension", telemetry.world().dimension());
            root.add("world", world);
        }
        if (telemetry.position() != null) {
            JsonObject position = new JsonObject();
            position.addProperty("x", telemetry.position().x());
            position.addProperty("y", telemetry.position().y());
            position.addProperty("z", telemetry.position().z());
            position.addProperty("yaw", telemetry.position().yaw());
            position.addProperty("pitch", telemetry.position().pitch());
            root.add("position", position);
        }
        JsonObject baritone = new JsonObject();
        baritone.addProperty("detected", telemetry.baritone().detected());
        baritone.addProperty("integration", telemetry.baritone().integration());
        root.add("baritone", baritone);

        if (telemetry.performance() != null) {
            JsonObject performance = new JsonObject();
            if (telemetry.performance().fps() != null) {
                performance.addProperty("fps", telemetry.performance().fps());
            }
            if (telemetry.performance().frameTimeMs() != null) {
                performance.addProperty("frame_time_ms", telemetry.performance().frameTimeMs());
            }
            root.add("performance", performance);
        }

        root.addProperty("captured_at", telemetry.capturedAt().toString());
        root.addProperty("uptime_seconds", telemetry.uptimeSeconds());
        return root;
    }

    public static JsonObject bye() {
        JsonObject root = new JsonObject();
        root.addProperty("type", "bye");
        return root;
    }

    public static ServerMessage parseServer(String line) {
        JsonElement parsed = JsonParser.parseString(line);
        if (!parsed.isJsonObject()) {
            throw new IllegalArgumentException("server message is not a JSON object");
        }
        JsonObject object = parsed.getAsJsonObject();
        String type = string(object, "type");
        return switch (type) {
            case "welcome" -> new Welcome(
                    integer(object, "protocol"),
                    string(object, "session_id"),
                    bool(object, "chat_collection"),
                    integer(object, "chat_retention_seconds"),
                    integer(object, "chat_max_messages"));
            case "request" -> new Request(string(object, "id"), string(object, "kind"),
                    objectPayload(object));
            case "error" -> new Error(string(object, "code"), string(object, "message"));
            default -> throw new IllegalArgumentException("unknown server message type '" + type + "'");
        };
    }

    /** A bounded amount of text from a request payload; never used as a secret. */
    public static String payloadString(JsonObject payload, String name, int maxChars) {
        JsonElement value = payload.get(name);
        if (value == null || !value.isJsonPrimitive() || !value.getAsJsonPrimitive().isString()) {
            throw new IllegalArgumentException("payload field '" + name + "' is missing or not a string");
        }
        String text = value.getAsString();
        if (text.isEmpty() || text.length() > maxChars
                || text.chars().anyMatch(Character::isISOControl)) {
            throw new IllegalArgumentException("payload field '" + name + "' is invalid");
        }
        return text;
    }

    public static int payloadInt(JsonObject payload, String name, int fallback,
                                 int min, int max) {
        JsonElement value = payload.get(name);
        if (value == null || !value.isJsonPrimitive() || !value.getAsJsonPrimitive().isNumber()) {
            return fallback;
        }
        int parsed = value.getAsInt();
        return Math.max(min, Math.min(max, parsed));
    }

    public static boolean payloadBoolean(JsonObject payload, String name, boolean fallback) {
        JsonElement value = payload.get(name);
        if (value == null || !value.isJsonPrimitive() || !value.getAsJsonPrimitive().isBoolean()) {
            return fallback;
        }
        return value.getAsBoolean();
    }

    private static JsonObject objectPayload(JsonObject message) {
        JsonElement value = message.get("payload");
        if (value == null || value.isJsonNull()) {
            return new JsonObject();
        }
        if (!value.isJsonObject()) {
            throw new IllegalArgumentException("field 'payload' is not an object");
        }
        return value.getAsJsonObject();
    }

    private static String string(JsonObject object, String name) {
        JsonElement value = object.get(name);
        if (value == null || !value.isJsonPrimitive() || !value.getAsJsonPrimitive().isString()) {
            throw new IllegalArgumentException("field '" + name + "' is missing or not a string");
        }
        String text = value.getAsString();
        if (text.isEmpty() || text.length() > 4096 || text.chars().anyMatch(Character::isISOControl)) {
            throw new IllegalArgumentException("field '" + name + "' is invalid");
        }
        return text;
    }

    private static int integer(JsonObject object, String name) {
        JsonElement value = object.get(name);
        if (value == null || !value.isJsonPrimitive() || !value.getAsJsonPrimitive().isNumber()) {
            throw new IllegalArgumentException("field '" + name + "' is missing or not a number");
        }
        return value.getAsInt();
    }

    private static boolean bool(JsonObject object, String name) {
        JsonElement value = object.get(name);
        if (value == null || !value.isJsonPrimitive() || !value.getAsJsonPrimitive().isBoolean()) {
            throw new IllegalArgumentException("field '" + name + "' is missing or not a boolean");
        }
        return value.getAsBoolean();
    }

    private static String bound(String text) {
        String clean = text.replaceAll("[\\p{Cntrl}]", " ");
        return clean.length() <= 512 ? clean : clean.substring(0, 512);
    }

    public static Instant parseTimestamp(String value) {
        try {
            return Instant.parse(value);
        } catch (DateTimeParseException err) {
            throw new IllegalArgumentException("invalid timestamp", err);
        }
    }
}
