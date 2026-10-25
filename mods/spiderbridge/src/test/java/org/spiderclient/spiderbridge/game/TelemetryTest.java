package org.spiderclient.spiderbridge.game;

import org.junit.jupiter.api.Test;

import java.time.Instant;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

class TelemetryTest {
    static Telemetry inWorld() {
        return new Telemetry("1.21.1", "fabric", "0.20.1", "0.1.0",
                Telemetry.GameState.IN_WORLD,
                new Telemetry.Player("Steve", 20.0f, 20),
                new Telemetry.World("minecraft:overworld"),
                new Telemetry.Position(12.5, 64.0, -30.25, 90.0f, 0.0f),
                new Telemetry.Baritone(false, "unavailable"),
                new Telemetry.Performance(144, 6.9f),
                Instant.parse("2026-10-25T10:00:00Z"), 42);
    }

    @Test
    void coordinatesAreAbsentRatherThanZeroOnTheMenu() {
        Telemetry menu = new Telemetry("1.21.1", "fabric", "0.20.1", "0.1.0",
                Telemetry.GameState.MENU, null, null, null,
                new Telemetry.Baritone(false, "unavailable"), null, Instant.now(), 5);
        assertNull(menu.position());
        assertTrue(menu.describeCoordinates().contains("not in a world"));
    }

    @Test
    void aWorldWithoutAPlayerStillHasNoPosition() {
        Telemetry loading = new Telemetry("1.21.1", "fabric", "0.20.1", "0.1.0",
                Telemetry.GameState.NO_PLAYER, null, new Telemetry.World("minecraft:the_nether"), null,
                new Telemetry.Baritone(false, "unavailable"), null, Instant.now(), 5);
        assertNull(loading.position());
        assertTrue(loading.describeCoordinates().contains("world loading"));
    }

    @Test
    void aPositionWithoutAWorldIsRejected() {
        assertThrows(IllegalArgumentException.class, () -> new Telemetry(
                "1.21.1", "fabric", "0.20.1", "0.1.0", Telemetry.GameState.IN_WORLD,
                null, null, new Telemetry.Position(0, 64, 0, 0, 0),
                new Telemetry.Baritone(false, "unavailable"), null, Instant.now(), 1));
        assertThrows(IllegalArgumentException.class, () -> new Telemetry(
                "1.21.1", "fabric", "0.20.1", "0.1.0", Telemetry.GameState.MENU,
                null, new Telemetry.World("minecraft:overworld"), new Telemetry.Position(0, 64, 0, 0, 0),
                new Telemetry.Baritone(false, "unavailable"), null, Instant.now(), 1));
    }

    @Test
    void nonFiniteAndOutOfRangeValuesAreRejected() {
        assertThrows(IllegalArgumentException.class,
                () -> new Telemetry.Position(Double.NaN, 64, 0, 0, 0));
        assertThrows(IllegalArgumentException.class,
                () -> new Telemetry.Position(0, 100_000, 0, 0, 0));
        assertThrows(IllegalArgumentException.class,
                () -> new Telemetry.Position(31_000_000, 64, 0, 0, 0));
        assertThrows(IllegalArgumentException.class,
                () -> new Telemetry.Performance(-1, 1.0f));
        assertThrows(IllegalArgumentException.class,
                () -> new Telemetry.Performance(60, Float.NaN));
        assertEquals(90.0f, inWorld().position().yaw());
    }

    @Test
    void uptimeAndBaritoneAreCarriedTruthfully() {
        Telemetry sample = inWorld();
        assertEquals(42, sample.uptimeSeconds());
        assertFalse(sample.baritone().detected());
        assertEquals("unavailable", sample.baritone().integration());
    }
}
