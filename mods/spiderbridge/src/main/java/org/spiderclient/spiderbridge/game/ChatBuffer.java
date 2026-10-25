package org.spiderclient.spiderbridge.game;

import com.google.gson.JsonArray;
import com.google.gson.JsonObject;
import org.spiderclient.spiderbridge.bridge.Protocol;

import java.time.Instant;
import java.util.ArrayDeque;
import java.util.ArrayList;
import java.util.List;

/**
 * A small, bounded, in-memory window over chat the client actually received.
 *
 * <p>This is deliberately not a log scraper. Entries come from the
 * {@code ChatListener} mixin on the client thread, live only in memory, and are
 * discarded when the owner disables collection, when the agent disconnects, or
 * when the retention window or count limit is exceeded. Nothing is written to
 * disk.
 *
 * <p>All methods are synchronized and short. The mixin adds at most one entry
 * per received message; the bridge thread copies a filtered snapshot.
 */
public final class ChatBuffer {
    public static final String KIND_CHAT = "chat";
    public static final String KIND_SYSTEM = "system";
    public static final String KIND_ACTION_BAR = "action_bar";

    public record Entry(long seq, Instant capturedAt, String kind, String sender, String text) {
        public Entry {
            if (seq <= 0 || capturedAt == null || kind == null || text == null || text.isEmpty()) {
                throw new IllegalArgumentException("chat entry is incomplete");
            }
        }
    }

    public record Snapshot(boolean enabled, Instant collectedSince, long dropped, List<Entry> messages) {
        public Snapshot {
            messages = List.copyOf(messages);
        }
    }

    private boolean enabled;
    private int retentionSeconds = 300;
    private int maxMessages = 200;
    private final ArrayDeque<Entry> entries = new ArrayDeque<>();
    private long nextSeq = 1;
    private long dropped;

    /**
     * Apply the agent's welcome settings. Disabling collection clears the
     * buffer immediately: a later re-enable must not resurrect old messages.
     */
    public synchronized void configure(boolean enabled, int retentionSeconds, int maxMessages) {
        this.retentionSeconds = Math.max(Protocol.MIN_CHAT_RETENTION_SECONDS,
                Math.min(Protocol.MAX_CHAT_RETENTION_SECONDS, retentionSeconds));
        this.maxMessages = Math.max(Protocol.MIN_CHAT_MESSAGES,
                Math.min(Protocol.MAX_CHAT_MESSAGES_SETTING, maxMessages));
        if (!enabled) {
            this.enabled = false;
            clearLocked();
            return;
        }
        this.enabled = true;
        pruneLocked(Instant.now());
    }

    public synchronized void disable() {
        enabled = false;
        clearLocked();
    }

    public synchronized boolean isEnabled() {
        return enabled;
    }

    public synchronized int maxMessages() {
        return maxMessages;
    }

    /**
     * Add one received message. Returns false when collection is disabled or the
     * text is unusable. Never throws: a mixin must not break the game.
     */
    public synchronized boolean add(String kind, String sender, String text) {
        if (!enabled) {
            return false;
        }
        String cleanKind = switch (kind) {
            case KIND_CHAT, KIND_SYSTEM, KIND_ACTION_BAR -> kind;
            default -> null;
        };
        String cleanText = clean(text, Protocol.MAX_CHAT_TEXT_CHARS);
        if (cleanKind == null || cleanText == null) {
            return false;
        }
        String cleanSender = clean(sender, Protocol.MAX_CHAT_SENDER_CHARS);
        Instant now = Instant.now();
        pruneLocked(now);
        entries.addLast(new Entry(nextSeq++, now, cleanKind, cleanSender, cleanText));
        while (entries.size() > maxMessages) {
            entries.removeFirst();
            dropped++;
        }
        return true;
    }

    /** A filtered copy, oldest first, capped to the protocol's message limit. */
    public synchronized Snapshot snapshot(int limit, boolean includeSystem, boolean includeActionBar) {
        int bounded = Math.max(1, Math.min(Protocol.MAX_CHAT_MESSAGES, limit));
        if (!enabled) {
            return new Snapshot(false, null, dropped, List.of());
        }
        pruneLocked(Instant.now());
        List<Entry> selected = new ArrayList<>();
        for (Entry entry : entries) {
            if (KIND_SYSTEM.equals(entry.kind()) && !includeSystem) {
                continue;
            }
            if (KIND_ACTION_BAR.equals(entry.kind()) && !includeActionBar) {
                continue;
            }
            selected.add(entry);
        }
        if (selected.size() > bounded) {
            selected = selected.subList(selected.size() - bounded, selected.size());
        }
        Instant since = selected.isEmpty() ? null : selected.get(0).capturedAt();
        return new Snapshot(true, since, dropped, selected);
    }

    /** JSON shape consumed by the agent's {@code ChatHistory}. */
    public JsonObject snapshotJson(String sessionId, int limit, boolean includeSystem,
                                   boolean includeActionBar) {
        Snapshot snapshot = snapshot(limit, includeSystem, includeActionBar);
        JsonObject root = new JsonObject();
        root.addProperty("session_id", sessionId);
        root.addProperty("collection_enabled", snapshot.enabled());
        if (snapshot.collectedSince() != null) {
            root.addProperty("collected_since", snapshot.collectedSince().toString());
        }
        root.addProperty("dropped", snapshot.dropped());
        JsonArray messages = new JsonArray();
        for (Entry entry : snapshot.messages()) {
            JsonObject message = new JsonObject();
            message.addProperty("seq", entry.seq());
            message.addProperty("captured_at", entry.capturedAt().toString());
            message.addProperty("kind", entry.kind());
            if (entry.sender() != null) {
                message.addProperty("sender", entry.sender());
            }
            message.addProperty("text", entry.text());
            messages.add(message);
        }
        root.add("messages", messages);
        return root;
    }

    private void pruneLocked(Instant now) {
        Instant cutoff = now.minusSeconds(retentionSeconds);
        while (!entries.isEmpty() && entries.peekFirst().capturedAt().isBefore(cutoff)) {
            entries.removeFirst();
            dropped++;
        }
        while (entries.size() > maxMessages) {
            entries.removeFirst();
            dropped++;
        }
    }

    private void clearLocked() {
        entries.clear();
    }

    private static String clean(String value, int maxChars) {
        if (value == null) {
            return null;
        }
        StringBuilder builder = new StringBuilder(Math.min(value.length(), maxChars));
        for (int i = 0; i < value.length() && builder.length() < maxChars; i++) {
            char ch = value.charAt(i);
            builder.append(Character.isISOControl(ch) ? ' ' : ch);
        }
        String cleaned = builder.toString().trim();
        return cleaned.isEmpty() ? null : cleaned;
    }
}
