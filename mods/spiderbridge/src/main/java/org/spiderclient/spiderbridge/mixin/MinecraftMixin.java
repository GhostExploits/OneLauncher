package org.spiderclient.spiderbridge.mixin;

import net.minecraft.client.Minecraft;
import org.spiderclient.spiderbridge.SpiderBridgeClient;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

/**
 * Two hooks, both on documented Minecraft lifecycle methods and both
 * allocation-light:
 *
 * <ul>
 *   <li>{@code tick} tail: drain queued chat/command actions, then sample at
 *       most once per second. All network work stays on the bridge thread.</li>
 *   <li>{@code close} head: ask the bridge to send {@code bye} and stop.</li>
 * </ul>
 *
 * <p>The tick counter lives in the entry point, so this mixin has no state and
 * cannot keep the client alive.
 */
@Mixin(Minecraft.class)
public abstract class MinecraftMixin {
    @Inject(method = "tick", at = @At("TAIL"))
    private void spiderbridge$afterTick(CallbackInfo callback) {
        SpiderBridgeClient.onClientTick();
    }

    @Inject(method = "close", at = @At("HEAD"))
    private void spiderbridge$beforeClose(CallbackInfo callback) {
        SpiderBridgeClient.onGameClose();
    }
}
