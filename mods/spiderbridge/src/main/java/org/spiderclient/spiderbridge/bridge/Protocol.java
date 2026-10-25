package org.spiderclient.spiderbridge.bridge;

/**
 * Wire constants shared by the mod and the Rust agent.
 *
 * <p>Keep these in sync with {@code packages/spider_remote/src/bridge.rs}. They
 * are duplicated rather than generated so the mod has no build dependency on
 * the Rust workspace; the integration tests and the handshake check both sides.
 */
public final class Protocol {
    private Protocol() {
    }

    /**
     * Bumped only for breaking wire changes.
     *
     * <p>Version 2 adds payload-bearing requests, in-memory chat history, chat
     * submission, and policy-checked command submission. A version 1 peer is
     * refused explicitly rather than silently served a different contract.
     */
    public static final int VERSION = 2;
    public static final int ENDPOINT_SCHEMA = 1;
    /** One newline-delimited UTF-8 JSON message. */
    public static final int MAX_MESSAGE_BYTES = 64 * 1024;
    /** The agent's secret is 32 random bytes, hex encoded. */
    public static final int SECRET_HEX_CHARS = 64;
    /** Telemetry older than this is reported as stale by the agent. */
    public static final int STALE_AFTER_SECONDS = 10;
    public static final int CONNECT_TIMEOUT_MS = 3_000;
    public static final int HANDSHAKE_TIMEOUT_MS = 5_000;
    public static final int READ_TIMEOUT_MS = 500;
    /** Telemetry cadence. The sampler runs once per second. */
    public static final int SAMPLE_INTERVAL_TICKS = 20;
    /** Even when nothing changed, send this often so the agent sees a heartbeat. */
    public static final long HEARTBEAT_INTERVAL_MS = 5_000;
    /** The agent closes a connection above 40 messages/second. Stay well under. */
    public static final int MIN_MESSAGE_INTERVAL_MS = 250;

    /** Vanilla caps a chat message at 256 characters. */
    public static final int MAX_CHAT_MESSAGE_CHARS = 256;
    /** One command line, without a leading slash. */
    public static final int MAX_COMMAND_CHARS = 256;
    public static final int MAX_CHAT_MESSAGES = 200;
    public static final int MAX_CHAT_TEXT_CHARS = 512;
    public static final int MAX_CHAT_SENDER_CHARS = 64;
    public static final int MIN_CHAT_RETENTION_SECONDS = 30;
    public static final int MAX_CHAT_RETENTION_SECONDS = 3_600;
    public static final int MIN_CHAT_MESSAGES = 20;
    public static final int MAX_CHAT_MESSAGES_SETTING = 500;
    /** Actions waiting for the client thread. More than this is a bug, not a queue. */
    public static final int MAX_PENDING_ACTIONS = 8;
    /** How long the bridge thread waits for the client thread to process one action. */
    public static final long ACTION_TIMEOUT_MS = 4_000;

    public static final String MOD_ID = "spiderbridge";
    public static final String MOD_VERSION = "0.1.0";
    public static final String LOADER = "fabric";
}
