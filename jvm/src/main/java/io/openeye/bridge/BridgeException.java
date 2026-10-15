package io.openeye.bridge;

/** An error reported by OpenEye core, e.g. "CPF5026: duplicate key on a UNIQUE file". */
public class BridgeException extends RuntimeException {
    public BridgeException(String message) {
        super(message);
    }

    public BridgeException(String message, Throwable cause) {
        super(message, cause);
    }

    /** The message identifier (e.g. CPF5026) when the error carries one, else null. */
    public String messageId() {
        String m = getMessage();
        if (m != null && m.length() >= 8 && m.charAt(7) == ':') return m.substring(0, 7);
        return null;
    }
}
