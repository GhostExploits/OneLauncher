package org.spiderclient.spiderbridge.game;

import org.junit.jupiter.api.Test;

import java.util.ArrayList;
import java.util.List;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

/**
 * Ordering and cancellation rules, tested without Minecraft. The distinction
 * that matters: a queued action can be denied, a started action can only be
 * reported as unknown.
 */
class ActionQueueTest {
    @Test
    void actionsRunInOrderOnTheDrainingThread() {
        ActionQueue queue = new ActionQueue(4);
        queue.submit("chat_send", "one");
        queue.submit("command", "two");

        List<String> seen = new ArrayList<>();
        queue.drain(pending -> {
            seen.add(pending.kind() + ":" + pending.value());
            return ActionQueue.Outcome.submitted("accepted " + pending.value());
        });

        assertEquals(List.of("chat_send:one", "command:two"), seen);
        assertEquals(0, queue.size());
    }

    @Test
    void theQueueIsBounded() {
        ActionQueue queue = new ActionQueue(1);
        ActionQueue.Pending first = queue.submit("chat_send", "one");
        ActionQueue.Pending second = queue.submit("chat_send", "two");

        assertEquals(1, queue.size());
        ActionQueue.Outcome outcome = second.result().join();
        assertFalse(outcome.accepted());
        assertFalse(outcome.uncertain());
        assertTrue(outcome.detail().contains("nothing was submitted"));
        assertFalse(first.result().isDone());
    }

    @Test
    void aStoppedQueueRefusesNewWorkAndCancelsWhatIsWaiting() {
        ActionQueue queue = new ActionQueue(4);
        ActionQueue.Pending waiting = queue.submit("chat_send", "one");
        queue.stop("the bridge is stopping");

        ActionQueue.Pending late = queue.submit("chat_send", "two");
        assertTrue(late.result().join().detail().contains("stopping"));
        ActionQueue.Outcome cancelled = waiting.result().join();
        assertFalse(cancelled.accepted());
        assertFalse(cancelled.uncertain());
        assertEquals(0, queue.size());
    }

    @Test
    void aQueuedActionThatTimesOutIsCancelledBeforeItCanRun() {
        ActionQueue queue = new ActionQueue(4);
        ActionQueue.Pending pending = queue.submit("chat_send", "one");

        ActionQueue.Outcome outcome = queue.await(pending, 50);
        assertFalse(outcome.accepted());
        assertFalse(outcome.uncertain(), "an action that never started is a rejection");
        assertEquals(ActionQueue.State.CANCELLED, pending.state());
        assertEquals(0, queue.size());
    }

    @Test
    void aStartedActionThatTimesOutIsUnknownNotRejected() {
        ActionQueue queue = new ActionQueue(4);
        ActionQueue.Pending pending = queue.submit("command", "list");
        ActionQueue.Pending claimed = queue.poll();
        assertEquals(pending, claimed);
        assertTrue(claimed.begin());

        ActionQueue.Outcome outcome = queue.await(pending, 50);
        assertFalse(outcome.accepted());
        assertTrue(outcome.uncertain(), "a started action may still have reached the server");
        assertEquals(ActionQueue.State.RUNNING, pending.state());
    }

    @Test
    void anExecutorFailureIsReportedAsUnknown() {
        ActionQueue queue = new ActionQueue(4);
        ActionQueue.Pending pending = queue.submit("command", "list");

        queue.drain(ignored -> {
            throw new IllegalStateException("boom");
        });

        // The line may have left our hands before the failure, so this must not
        // be reported as a rejection.
        ActionQueue.Outcome outcome = pending.result().join();
        assertFalse(outcome.accepted());
        assertTrue(outcome.uncertain());
        assertTrue(outcome.detail().contains("IllegalStateException"));
        assertEquals(0, queue.size());
    }

    @Test
    void anOutcomeCannotBeBothAcceptedAndUncertain() {
        org.junit.jupiter.api.Assertions.assertThrows(IllegalArgumentException.class,
                () -> new ActionQueue.Outcome(true, true, "contradiction"));
    }
}
