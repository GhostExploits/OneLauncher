package org.spiderclient.spiderbridge.bridge;

import com.google.gson.JsonObject;

import java.io.EOFException;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.nio.charset.StandardCharsets;

/** Newline-delimited JSON framing with a hard size cap in both directions. */
public final class SocketLines {
    private SocketLines() {
    }

    public static void write(OutputStream out, JsonObject message) throws IOException {
        byte[] payload = message.toString().getBytes(StandardCharsets.UTF_8);
        if (payload.length == 0 || payload.length > Protocol.MAX_MESSAGE_BYTES) {
            throw new IOException("outgoing bridge message exceeded the size limit");
        }
        out.write(payload);
        out.write('\n');
        out.flush();
    }

    /**
     * Read one line. Returns {@code null} at end of stream. A line longer than
     * the cap is rejected rather than buffered.
     */
    public static String read(InputStream in) throws IOException {
        byte[] buffer = new byte[512];
        int length = 0;
        while (true) {
            int value = in.read();
            if (value == -1) {
                if (length == 0) {
                    return null;
                }
                throw new EOFException("bridge message ended without a newline");
            }
            if (value == '\n') {
                if (length == 0) {
                    throw new IOException("empty bridge message");
                }
                return new String(buffer, 0, length, StandardCharsets.UTF_8);
            }
            if (value == '\r') {
                continue;
            }
            if (length == buffer.length) {
                if (length >= Protocol.MAX_MESSAGE_BYTES) {
                    throw new IOException("incoming bridge message exceeded the size limit");
                }
                buffer = java.util.Arrays.copyOf(buffer, Math.min(buffer.length * 2, Protocol.MAX_MESSAGE_BYTES));
            }
            buffer[length++] = (byte) value;
        }
    }
}
