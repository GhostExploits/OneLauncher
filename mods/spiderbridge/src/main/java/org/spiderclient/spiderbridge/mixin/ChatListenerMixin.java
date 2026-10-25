package org.spiderclient.spiderbridge.mixin;

import net.minecraft.client.multiplayer.chat.ChatListener;
import net.minecraft.network.chat.ChatType;
import net.minecraft.network.chat.Component;
import net.minecraft.network.chat.PlayerChatMessage;
import com.mojang.authlib.GameProfile;
import org.spiderclient.spiderbridge.SpiderBridgeClient;
import org.spiderclient.spiderbridge.game.ChatBuffer;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

/**
 * Observes chat the client already received, so the bounded in-memory buffer
 * contains real messages instead of lines scraped from {@code latest.log}.
 *
 * <p>Injection is at the head of each public handler and every call is guarded:
 * collection is a no-op unless the agent explicitly enabled it in its welcome
 * message, and a failure here must never disturb the game.
 */
@Mixin(ChatListener.class)
public abstract class ChatListenerMixin {
    @Inject(method = "handlePlayerChatMessage", at = @At("HEAD"))
    private void spiderbridge$playerChat(PlayerChatMessage message, GameProfile profile,
                                         ChatType.Bound bound, CallbackInfo callback) {
        try {
            if (message == null) {
                return;
            }
            String text = message.signedContent();
            if (text == null || text.isBlank()) {
                text = message.decoratedContent().getString();
            }
            SpiderBridgeClient.recordChat(ChatBuffer.KIND_CHAT,
                    profile == null ? null : profile.getName(), text);
        } catch (Throwable ignored) {
            // Chat observation is best effort and must never break message handling.
        }
    }

    @Inject(method = "handleDisguisedChatMessage", at = @At("HEAD"))
    private void spiderbridge$disguisedChat(Component content, ChatType.Bound bound,
                                            CallbackInfo callback) {
        try {
            if (content == null) {
                return;
            }
            String sender = bound == null ? null : bound.name().getString();
            SpiderBridgeClient.recordChat(ChatBuffer.KIND_CHAT, sender, content.getString());
        } catch (Throwable ignored) {
            // See above.
        }
    }

    @Inject(method = "handleSystemMessage", at = @At("HEAD"))
    private void spiderbridge$systemChat(Component message, boolean overlay,
                                         CallbackInfo callback) {
        try {
            if (message == null) {
                return;
            }
            SpiderBridgeClient.recordChat(
                    overlay ? ChatBuffer.KIND_ACTION_BAR : ChatBuffer.KIND_SYSTEM,
                    null,
                    message.getString());
        } catch (Throwable ignored) {
            // See above.
        }
    }
}
