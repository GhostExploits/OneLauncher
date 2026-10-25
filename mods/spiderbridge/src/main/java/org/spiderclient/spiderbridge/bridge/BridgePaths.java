package org.spiderclient.spiderbridge.bridge;

import java.nio.file.Path;
import java.util.Locale;
import java.util.Map;

/**
 * Locates the agent's endpoint file.
 *
 * <p>Resolution order: the {@code spiderbridge.endpoint} system property, then
 * the {@code SPIDERBRIDGE_ENDPOINT} environment variable, then the platform
 * default used by the Rust agent's {@code directories} crate. The overrides
 * exist so a test or a non-standard agent build can point the mod elsewhere
 * without patching the JAR.
 */
public final class BridgePaths {
    public static final String PROPERTY = "spiderbridge.endpoint";
    public static final String ENV = "SPIDERBRIDGE_ENDPOINT";

    private BridgePaths() {
    }

    public static Path resolve(String property, Map<String, String> environment, String osName,
                               String localAppData, String home) {
        if (property != null && !property.isBlank()) {
            return Path.of(property);
        }
        String fromEnv = environment.get(ENV);
        if (fromEnv != null && !fromEnv.isBlank()) {
            return Path.of(fromEnv);
        }
        String os = osName == null ? "" : osName.toLowerCase(Locale.ROOT);
        if (os.contains("win")) {
            String base = localAppData != null && !localAppData.isBlank()
                    ? localAppData
                    : System.getProperty("user.home", ".");
            return Path.of(base, "SpiderClient", "SpiderClient", "data", "bridge.json");
        }
        if (os.contains("mac")) {
            return Path.of(home, "Library", "Application Support", "org.SpiderClient.SpiderClient", "bridge.json");
        }
        // Linux/BSD: directories-rs lowercases the application name for
        // ProjectDirs::from("org", "SpiderClient", "SpiderClient").
        String xdg = environment.get("XDG_DATA_HOME");
        Path base = xdg != null && !xdg.isBlank()
                ? Path.of(xdg)
                : Path.of(home, ".local", "share");
        return base.resolve("spiderclient").resolve("bridge.json");
    }

    public static Path resolveDefault() {
        return resolve(
                System.getProperty(PROPERTY),
                System.getenv(),
                System.getProperty("os.name", ""),
                System.getenv("LOCALAPPDATA"),
                System.getProperty("user.home", "."));
    }
}
