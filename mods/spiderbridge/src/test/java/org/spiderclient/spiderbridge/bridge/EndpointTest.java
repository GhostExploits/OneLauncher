package org.spiderclient.spiderbridge.bridge;

import org.junit.jupiter.api.Test;

import java.nio.file.Path;
import java.util.Map;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

class EndpointTest {
    private static final String SECRET = "0123456789abcdef".repeat(4);

    static String validJson() {
        return """
                {
                  "schema": 1,
                  "protocol": 1,
                  "port": 51234,
                  "secret": "%s",
                  "agent_version": "0.3.0",
                  "agent_pid": 4242,
                  "updated_at": "2026-10-25T12:00:00Z"
                }
                """.formatted(SECRET);
    }

    @Test
    void aValidEndpointIsParsedAndTheSecretIsRedacted() {
        Endpoint endpoint = EndpointFile.parse(validJson());
        assertEquals(1, endpoint.protocol());
        assertEquals(51234, endpoint.port());
        assertEquals(SECRET, endpoint.secret());
        assertTrue(endpoint.describe().contains("127.0.0.1:51234"));
        assertFalse(endpoint.toString().contains(SECRET));
        assertTrue(endpoint.toString().contains("redacted"));
    }

    @Test
    void malformedOrUnsafeEndpointsAreRejected() {
        assertThrows(IllegalArgumentException.class, () -> EndpointFile.parse("not json"));
        assertThrows(IllegalArgumentException.class,
                () -> EndpointFile.parse(validJson().replace("\"schema\": 1", "\"schema\": 2")));
        assertThrows(IllegalArgumentException.class,
                () -> EndpointFile.parse(validJson().replace("\"port\": 51234", "\"port\": 0")));
        assertThrows(IllegalArgumentException.class,
                () -> EndpointFile.parse(validJson().replace(SECRET, SECRET.substring(0, 10))));
        assertThrows(IllegalArgumentException.class,
                () -> EndpointFile.parse(validJson().replace(SECRET, "z".repeat(64))));
        assertThrows(IllegalArgumentException.class,
                () -> EndpointFile.parse(validJson().replace("\"agent_pid\": 4242", "\"agent_pid\": 0")));
        assertThrows(IllegalArgumentException.class,
                () -> EndpointFile.parse("{}"));
    }

    @Test
    void pathsFollowPropertyThenEnvironmentThenPlatform() {
        Map<String, String> env = Map.of(
                "SPIDERBRIDGE_ENDPOINT", "D:/from-env/bridge.json",
                "LOCALAPPDATA", "C:/Users/me/AppData/Local",
                "XDG_DATA_HOME", "/home/me/.local/share");

        assertEquals(Path.of("E:/explicit/bridge.json"),
                BridgePaths.resolve("E:/explicit/bridge.json", env, "Windows 11",
                        "C:/Users/me/AppData/Local", "C:/Users/me"));
        assertEquals(Path.of("D:/from-env/bridge.json"),
                BridgePaths.resolve(null, env, "Windows 11", "C:/Users/me/AppData/Local", "C:/Users/me"));
        assertEquals(Path.of("C:/Users/me/AppData/Local", "SpiderClient", "SpiderClient", "data", "bridge.json"),
                BridgePaths.resolve("", Map.of(), "Windows 11", "C:/Users/me/AppData/Local", "C:/Users/me"));
        assertEquals(Path.of("/home/me/Library/Application Support/org.SpiderClient.SpiderClient/bridge.json"),
                BridgePaths.resolve(null, Map.of(), "Mac OS X", null, "/home/me"));
        assertEquals(Path.of("/home/me/.local/share/spiderclient/bridge.json"),
                BridgePaths.resolve(null, Map.of(), "Linux", null, "/home/me"));
        assertEquals(Path.of("/custom/xdg/spiderclient/bridge.json"),
                BridgePaths.resolve(null, Map.of("XDG_DATA_HOME", "/custom/xdg"), "Linux", null, "/home/me"));
    }

    @Test
    void reconnectBackoffIsBoundedAndResettable() {
        ReconnectPolicy policy = new ReconnectPolicy();
        assertEquals(1_000, policy.nextDelayMs());
        assertEquals(2_000, policy.nextDelayMs());
        assertEquals(4_000, policy.nextDelayMs());
        for (int i = 0; i < 10; i++) {
            policy.nextDelayMs();
        }
        assertEquals(30_000, policy.nextDelayMs());
        policy.reset();
        assertEquals(1_000, policy.nextDelayMs());
        assertEquals(10_000, policy.nextDelayMsForRejection());
        assertEquals(1_000, policy.nextDelayMs());
    }
}
