package org.spiderclient.spiderbridge.game;

import java.util.ArrayDeque;
import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.ExecutionException;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.TimeoutException;

/**
 * A bounded hand-off between the bridge thread and the Minecraft client thread.
 *
 * <p>The bridge thread must never call into the game. It submits an action here
 * and waits for the client tick to drain and execute it. Each action has a
 * single owner: once the client thread starts it, a timeout can no longer
 * cancel it, so the caller must report the outcome as unknown rather than
 * pretending it was rejected.
 *
 * <p>This class has no Minecraft types so its ordering and cancellation rules
 * are unit-tested without launching the game.
 */
public final class ActionQueue {
    public enum State {
        PENDING,
        RUNNING,
        DONE,
        CANCELLED
    }

    /** What the client did with one action. {@code uncertain} means "may still run". */
    public record Outcome(boolean accepted, boolean uncertain, String detail) {
        public Outcome {
            if (accepted && uncertain) {
                throw new IllegalArgumentException("an accepted action cannot be uncertain");
            }
            if (detail != null && detail.length() > 400) {
                detail = detail.substring(0, 400);
            }
        }

        public static Outcome submitted(String detail) {
            return new Outcome(true, false, detail);
        }

        public static Outcome rejected(String detail) {
            return new Outcome(false, false, detail);
        }

        public static Outcome unknown(String detail) {
            return new Outcome(false, true, detail);
        }
    }

    /** One queued action and its single result. */
    public static final class Pending {
        private final long id;
        private final String kind;
        private final String value;
        private final CompletableFuture<Outcome> result = new CompletableFuture<>();
        private State state = State.PENDING;

        private Pending(long id, String kind, String value) {
            this.id = id;
            this.kind = kind;
            this.value = value;
        }

        public long id() {
            return id;
        }

        public String kind() {
            return kind;
        }

        public String value() {
            return value;
        }

        public synchronized State state() {
            return state;
        }

        /** Client thread: claim this action exactly once. */
        synchronized boolean begin() {
            if (state != State.PENDING) {
                return false;
            }
            state = State.RUNNING;
            return true;
        }

        void complete(Outcome outcome) {
            synchronized (this) {
                state = State.DONE;
            }
            result.complete(outcome);
        }

        /** Returns true only when the action had not started and will never run. */
        boolean cancel(String reason) {
            synchronized (this) {
                if (state != State.PENDING) {
                    return false;
                }
                state = State.CANCELLED;
            }
            result.complete(Outcome.rejected(reason));
            return true;
        }

        public CompletableFuture<Outcome> result() {
            return result;
        }
    }

    private final int capacity;
    private final ArrayDeque<Pending> queue = new ArrayDeque<>();
    private boolean stopped;
    private long nextId = 1;

    /** Client thread: the only code allowed to claim and complete an action. */
    public interface Executor {
        Outcome execute(Pending pending);
    }

    public ActionQueue(int capacity) {
        this.capacity = Math.max(1, capacity);
    }

    /**
     * Client thread only: run everything currently queued, then return. An
     * action added after this returns waits for the next tick.
     */
    public void drain(Executor executor) {
        Pending pending;
        while ((pending = poll()) != null) {
            if (!pending.begin()) {
                continue;
            }
            Outcome outcome;
            try {
                outcome = executor.execute(pending);
            } catch (Throwable err) {
                // The action may already have reached the server before the
                // failure, so this is unknown, never a rejection.
                outcome = Outcome.unknown(
                        "the game threw while running the action: " + err.getClass().getSimpleName());
            }
            pending.complete(outcome);
        }
    }

    public synchronized Pending submit(String kind, String value) {
        if (stopped) {
            return rejectedNow(kind, value, "the bridge is stopping; nothing was submitted");
        }
        if (queue.size() >= capacity) {
            return rejectedNow(kind, value,
                    "too many actions are already waiting for the game; nothing was submitted");
        }
        Pending pending = new Pending(nextId++, kind, value);
        queue.addLast(pending);
        return pending;
    }

    /** An action that is refused before it is queued. */
    public static Pending rejectedNow(String kind, String value, String reason) {
        Pending pending = new Pending(0, kind, value);
        pending.cancel(reason);
        return pending;
    }

    /** Client thread: take the next action, or {@code null}. */
    public synchronized Pending poll() {
        return queue.pollFirst();
    }

    public synchronized List<Pending> pending() {
        return new ArrayList<>(queue);
    }

    public synchronized int size() {
        return queue.size();
    }

    /** Bridge thread timeout: cancel only if the client has not started it. */
    public synchronized boolean cancel(Pending pending, String reason) {
        if (!queue.remove(pending)) {
            return false;
        }
        return pending.cancel(reason);
    }

    /** Stop accepting work and refuse everything still queued. */
    public synchronized void stop(String reason) {
        stopped = true;
        Pending pending;
        while ((pending = queue.pollFirst()) != null) {
            pending.cancel(reason);
        }
    }

    public synchronized boolean isStopped() {
        return stopped;
    }

    /**
     * Wait for the client thread. On timeout, only a pending action can be
     * denied; a running one is reported as uncertain.
     */
    public Outcome await(Pending pending, long timeoutMs) {
        try {
            return pending.result().get(timeoutMs, TimeUnit.MILLISECONDS);
        } catch (TimeoutException timeout) {
            if (cancel(pending, "the game did not process the action in time; nothing was submitted")) {
                return pending.result().join();
            }
            try {
                return pending.result().get(1, TimeUnit.SECONDS);
            } catch (TimeoutException | InterruptedException | ExecutionException stillWaiting) {
                if (stillWaiting instanceof InterruptedException) {
                    Thread.currentThread().interrupt();
                }
                return Outcome.unknown(
                        "the game started processing this action but did not confirm it; the outcome is unknown");
            }
        } catch (InterruptedException interrupted) {
            Thread.currentThread().interrupt();
            return Outcome.unknown("interrupted while waiting for the game; the outcome is unknown");
        } catch (ExecutionException failure) {
            return Outcome.unknown("the action failed unexpectedly; the outcome is unknown");
        }
    }
}
