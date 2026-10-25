package org.spiderclient.spiderbridge.game;

import net.minecraft.client.Minecraft;
import net.minecraft.client.multiplayer.ClientPacketListener;
import org.spiderclient.spiderbridge.bridge.BridgeLink;
import org.spiderclient.spiderbridge.bridge.Protocol;

import com.google.gson.JsonObject;

/**
 * Executes queued chat and command actions on the Minecraft client thread.
 *
 * <p>Only vanilla APIs are used: {@code ClientPacketListener.sendChat} and
 * {@code ClientPacketListener.sendCommand}. Both hand a line to the server and
 * return. Neither exposes a request id or a correlated result, so success here
 * means "the client accepted and submitted the line", never "the server ran
 * it". That distinction is carried all the way back to Discord.
 *
 * <p>Every method except {@link #drain()} may be called from the bridge thread.
 * {@link #drain()} must only be called from the client thread.
 */
public final class GameActions implements BridgeLink.ChatSource {
    private final ActionQueue queue;
    private final ChatBuffer chat;

    public GameActions(ActionQueue queue, ChatBuffer chat) {
        this.queue = queue;
        this.chat = chat;
    }

    public ActionQueue queue() {
        return queue;
    }

    public ChatBuffer chat() {
        return chat;
    }

    /** Client thread: run at most the actions currently queued, then stop. */
    public void drain() {
        queue.drain(GameActions::execute);
    }

    private static ActionQueue.Outcome execute(ActionQueue.Pending pending) {
        String kind = pending.kind();
        String value = pending.value();
        if ("command".equals(kind)) {
            if (value.startsWith("/")) {
                value = value.substring(1);
            }
            if (value.isBlank()) {
                return ActionQueue.Outcome.rejected("the command was empty; nothing was submitted");
            }
            if (value.length() > Protocol.MAX_COMMAND_CHARS) {
                return ActionQueue.Outcome.rejected(
                        "the command was longer than the accepted limit; nothing was submitted");
            }
        } else if ("chat_send".equals(kind)) {
            if (value.isBlank()) {
                return ActionQueue.Outcome.rejected("the chat line was empty; nothing was submitted");
            }
            if (value.length() > Protocol.MAX_CHAT_MESSAGE_CHARS) {
                return ActionQueue.Outcome.rejected(
                        "the chat line was longer than the accepted limit; nothing was submitted");
            }
        } else {
            return ActionQueue.Outcome.rejected("unsupported action '" + kind + "'");
        }

        Minecraft minecraft = Minecraft.getInstance();
        if (minecraft == null) {
            return ActionQueue.Outcome.rejected("the Minecraft client is not available");
        }
        if (minecraft.player == null) {
            return ActionQueue.Outcome.rejected("no player is in a world; nothing was submitted");
        }
        ClientPacketListener connection = minecraft.getConnection();
        if (connection == null) {
            return ActionQueue.Outcome.rejected(
                    "the client is not connected to a server; nothing was submitted");
        }

        if ("chat_send".equals(kind)) {
            connection.sendChat(value);
            return ActionQueue.Outcome.submitted(
                    "ClientPacketListener.sendChat accepted the line; delivery is not verified");
        }
        connection.sendCommand(value);
        return ActionQueue.Outcome.submitted(
                "ClientPacketListener.sendCommand accepted the command; server execution is not verified");
    }

    /** Called from the chat mixin on the client thread. Never throws. */
    public boolean recordChat(String kind, String sender, String text) {
        return chat.add(kind, sender, text);
    }

    @Override
    public void configure(boolean enabled, int retentionSeconds, int maxMessages) {
        chat.configure(enabled, retentionSeconds, maxMessages);
    }

    @Override
    public void disable() {
        chat.disable();
    }

    @Override
    public JsonObject snapshot(String sessionId, int limit, boolean includeSystem,
                               boolean includeActionBar) {
        return chat.snapshotJson(sessionId, limit, includeSystem, includeActionBar);
    }
}
