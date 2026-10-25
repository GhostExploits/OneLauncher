package org.spiderclient.spiderbridge.bridge;

/** Bounded exponential backoff for reconnects. */
public final class ReconnectPolicy {
    private static final long FIRST_MS = 1_000;
    private static final long MAX_MS = 30_000;

    private long nextMs = FIRST_MS;

    public long nextDelayMs() {
        long delay = nextMs;
        nextMs = Math.min(nextMs * 2, MAX_MS);
        return delay;
    }

    public void reset() {
        nextMs = FIRST_MS;
    }

    /** Authentication failures retry more slowly but must still recover when the agent restarts. */
    public long nextDelayMsForRejection() {
        reset();
        return MAX_MS / 3;
    }
}
