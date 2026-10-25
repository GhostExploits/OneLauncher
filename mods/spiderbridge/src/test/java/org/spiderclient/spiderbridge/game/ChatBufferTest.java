package org.spiderclient.spiderbridge.game;

import com.google.gson.JsonObject;
import org.junit.jupiter.api.Test;
import org.spiderclient.spiderbridge.bridge.Protocol;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

/**
 * The buffer is plain Java and its bounds are the only thing standing between
 * an opt-in chat preview and unbounded memory. These tests do not start a game.
 */
class ChatBufferTest {
    @Test
    void collectionIsDisabledUntilTheOwnerOptsIn() {
        ChatBuffer buffer = new ChatBuffer();
        assertFalse(buffer.isEnabled());
        assertFalse(buffer.add(ChatBuffer.KIND_CHAT, "Alex", "hello"));

        ChatBuffer.Snapshot snapshot = buffer.snapshot(50, true, true);
        assertFalse(snapshot.enabled());
        assertTrue(snapshot.messages().isEmpty());
    }

    @Test
    void enablingCollectsOnlyTheKindsAndFiltersRequested() {
        ChatBuffer buffer = new ChatBuffer();
        buffer.configure(true, 300, 100);
        assertTrue(buffer.isEnabled());

        assertTrue(buffer.add(ChatBuffer.KIND_CHAT, "Alex", "hello"));
        assertTrue(buffer.add(ChatBuffer.KIND_SYSTEM, null, "Alex joined the game"));
        assertTrue(buffer.add(ChatBuffer.KIND_ACTION_BAR, null, "You cannot rest now"));

        ChatBuffer.Snapshot chatOnly = buffer.snapshot(50, false, false);
        assertEquals(1, chatOnly.messages().size());
        assertEquals("hello", chatOnly.messages().get(0).text());

        ChatBuffer.Snapshot withSystem = buffer.snapshot(50, true, false);
        assertEquals(2, withSystem.messages().size());

        ChatBuffer.Snapshot everything = buffer.snapshot(50, true, true);
        assertEquals(3, everything.messages().size());
        assertEquals(ChatBuffer.KIND_ACTION_BAR, everything.messages().get(2).kind());
    }

    @Test
    void theNewestMessagesSurviveAndDiscardsAreCounted() {
        ChatBuffer buffer = new ChatBuffer();
        buffer.configure(true, 300, Protocol.MIN_CHAT_MESSAGES);
        for (int i = 0; i < Protocol.MIN_CHAT_MESSAGES + 7; i++) {
            assertTrue(buffer.add(ChatBuffer.KIND_CHAT, "Alex", "message " + i));
        }
        ChatBuffer.Snapshot snapshot = buffer.snapshot(Protocol.MAX_CHAT_MESSAGES, false, false);
        assertEquals(Protocol.MIN_CHAT_MESSAGES, snapshot.messages().size());
        assertEquals("message " + (Protocol.MIN_CHAT_MESSAGES + 6),
                snapshot.messages().get(snapshot.messages().size() - 1).text());
        assertTrue(snapshot.dropped() >= 7);
    }

    @Test
    void textAndSenderAreBoundedAndControlCharactersAreNeutralised() {
        ChatBuffer buffer = new ChatBuffer();
        buffer.configure(true, 300, 100);
        assertTrue(buffer.add(ChatBuffer.KIND_CHAT, "Alex\nAdmin",
                "line one\nline two " + "x".repeat(2_000)));

        ChatBuffer.Snapshot snapshot = buffer.snapshot(10, false, false);
        ChatBuffer.Entry entry = snapshot.messages().get(0);
        assertFalse(entry.text().contains("\n"));
        assertFalse(entry.sender().contains("\n"));
        assertTrue(entry.text().length() <= Protocol.MAX_CHAT_TEXT_CHARS);
        assertTrue(entry.sender().length() <= Protocol.MAX_CHAT_SENDER_CHARS);
    }

    @Test
    void unknownKindsAndBlankTextAreRejected() {
        ChatBuffer buffer = new ChatBuffer();
        buffer.configure(true, 300, 100);
        assertFalse(buffer.add("whisper", "Alex", "hi"));
        assertFalse(buffer.add(ChatBuffer.KIND_CHAT, "Alex", "   "));
        assertFalse(buffer.add(ChatBuffer.KIND_CHAT, "Alex", "\n\t"));
    }

    @Test
    void disablingClearsEverythingSoOldHistoryCannotComeBack() {
        ChatBuffer buffer = new ChatBuffer();
        buffer.configure(true, 300, 100);
        assertTrue(buffer.add(ChatBuffer.KIND_CHAT, "Alex", "secret plan"));
        buffer.disable();
        assertFalse(buffer.isEnabled());
        assertTrue(buffer.snapshot(50, true, true).messages().isEmpty());

        buffer.configure(true, 300, 100);
        assertTrue(buffer.snapshot(50, true, true).messages().isEmpty());
    }

    @Test
    void theJsonShapeMatchesTheAgentContract() {
        ChatBuffer buffer = new ChatBuffer();
        buffer.configure(true, 300, 100);
        buffer.add(ChatBuffer.KIND_CHAT, "Alex", "hello");

        JsonObject json = buffer.snapshotJson("sess-1", 10, false, false);
        assertEquals("sess-1", json.get("session_id").getAsString());
        assertTrue(json.get("collection_enabled").getAsBoolean());
        assertEquals(1, json.getAsJsonArray("messages").size());
        JsonObject message = json.getAsJsonArray("messages").get(0).getAsJsonObject();
        assertEquals("chat", message.get("kind").getAsString());
        assertEquals("Alex", message.get("sender").getAsString());
        assertEquals("hello", message.get("text").getAsString());
        assertTrue(message.get("captured_at").getAsString().endsWith("Z"));
        assertTrue(message.get("seq").getAsLong() > 0);
    }
}
