package org.spiderclient.spiderbridge.bridge;

import com.google.gson.JsonElement;
import com.google.gson.JsonObject;
import com.google.gson.JsonParser;

import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Optional;

/** Reads the agent's {@code bridge.json}. */
public final class EndpointFile {
    /** Refuse to read an implausibly large endpoint file. */
    private static final long MAX_BYTES = 64 * 1024;

    private EndpointFile() {
    }

    public static Optional<Endpoint> tryRead(Path path) {
        try {
            if (!Files.isRegularFile(path)) {
                return Optional.empty();
            }
            if (Files.size(path) > MAX_BYTES) {
                throw new IllegalArgumentException("the endpoint file is unexpectedly large");
            }
            String text = Files.readString(path, StandardCharsets.UTF_8);
            return Optional.of(parse(text));
        } catch (IOException | RuntimeException ignored) {
            return Optional.empty();
        }
    }

    /**
     * Parse the agent's JSON. Unknown fields are ignored so the agent can add
     * diagnostics without breaking an older mod.
     */
    public static Endpoint parse(String text) {
        JsonElement parsed;
        try {
            parsed = JsonParser.parseString(text);
        } catch (RuntimeException err) {
            throw new IllegalArgumentException("the endpoint file is not valid JSON", err);
        }
        if (!parsed.isJsonObject()) {
            throw new IllegalArgumentException("the endpoint file is not a JSON object");
        }
        JsonObject object = parsed.getAsJsonObject();
        int schema = requiredInt(object, "schema");
        int protocol = requiredInt(object, "protocol");
        int port = requiredInt(object, "port");
        String secret = requiredString(object, "secret");
        String agentVersion = requiredString(object, "agent_version");
        int agentPid = requiredInt(object, "agent_pid");
        return new Endpoint(schema, protocol, port, secret, agentVersion, agentPid);
    }

    private static int requiredInt(JsonObject object, String name) {
        JsonElement value = object.get(name);
        if (value == null || !value.isJsonPrimitive() || !value.getAsJsonPrimitive().isNumber()) {
            throw new IllegalArgumentException("endpoint field '" + name + "' is missing or not a number");
        }
        return value.getAsInt();
    }

    private static String requiredString(JsonObject object, String name) {
        JsonElement value = object.get(name);
        if (value == null || !value.isJsonPrimitive() || !value.getAsJsonPrimitive().isString()) {
            throw new IllegalArgumentException("endpoint field '" + name + "' is missing or not a string");
        }
        return value.getAsString();
    }
}
