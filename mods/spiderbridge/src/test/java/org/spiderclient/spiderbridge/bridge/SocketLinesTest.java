package org.spiderclient.spiderbridge.bridge;

import com.google.gson.JsonObject;
import org.junit.jupiter.api.Test;

import java.io.ByteArrayInputStream;
import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.nio.charset.StandardCharsets;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertThrows;

class SocketLinesTest {
    @Test
    void linesRoundTripWithCrLfTolerance() throws IOException {
        ByteArrayOutputStream out = new ByteArrayOutputStream();
        JsonObject object = new JsonObject();
        object.addProperty("type", "bye");
        SocketLines.write(out, object);
        assertEquals("{\"type\":\"bye\"}\n", out.toString(StandardCharsets.UTF_8));

        assertEquals("{\"type\":\"bye\"}",
                SocketLines.read(new ByteArrayInputStream(out.toByteArray())));
        assertEquals("{\"a\":1}",
                SocketLines.read(new ByteArrayInputStream("{\"a\":1}\r\n".getBytes(StandardCharsets.UTF_8))));
    }

    @Test
    void anOversizedIncomingLineIsRejectedWithoutBufferingItAll() {
        byte[] huge = new byte[Protocol.MAX_MESSAGE_BYTES + 10];
        java.util.Arrays.fill(huge, (byte) 'a');
        assertThrows(IOException.class, () -> SocketLines.read(new ByteArrayInputStream(huge)));
    }

    @Test
    void oversizedOutgoingMessagesAndEmptyLinesAreRejected() {
        JsonObject object = new JsonObject();
        object.addProperty("type", "x".repeat(Protocol.MAX_MESSAGE_BYTES));
        assertThrows(IOException.class, () -> SocketLines.write(new ByteArrayOutputStream(), object));
        assertThrows(IOException.class, () -> SocketLines.read(new ByteArrayInputStream("\n".getBytes())));
    }

    @Test
    void endOfStreamIsDistinctFromATruncatedMessage() throws IOException {
        assertNull(SocketLines.read(new ByteArrayInputStream(new byte[0])));
        assertThrows(IOException.class,
                () -> SocketLines.read(new ByteArrayInputStream("{\"type\":\"bye\"}".getBytes())));
    }

    @Test
    void aMessageExactlyAtTheLimitIsAccepted() throws IOException {
        String prefix = "{\"type\":\"";
        String suffix = "\"}";
        int fill = Protocol.MAX_MESSAGE_BYTES - prefix.length() - suffix.length();
        String line = prefix + "a".repeat(fill) + suffix;
        assertEquals(Protocol.MAX_MESSAGE_BYTES, line.length());
        assertEquals(line, SocketLines.read(new ByteArrayInputStream(
                (line + "\n").getBytes(StandardCharsets.UTF_8))));
    }
}
