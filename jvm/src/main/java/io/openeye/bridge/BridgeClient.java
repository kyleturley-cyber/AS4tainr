package io.openeye.bridge;

import java.io.BufferedReader;
import java.io.IOException;
import java.io.InputStreamReader;
import java.io.OutputStreamWriter;
import java.io.Writer;
import java.net.Socket;
import java.nio.charset.StandardCharsets;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

/**
 * Client for the OpenEye core bridge: one JSON object per line over a loopback
 * TCP socket. One client is one job's database session. Thread-safe (calls are
 * serialized), but a job normally uses it from one thread.
 *
 * See docs/bridge-protocol.md.
 */
public final class BridgeClient implements AutoCloseable {
    public static final int PROTOCOL_VERSION = 1;

    private final Socket socket;
    private final BufferedReader in;
    private final Writer out;
    private long nextId = 1;

    public BridgeClient(String host, int port) throws IOException {
        socket = new Socket(host, port);
        socket.setTcpNoDelay(true);
        in = new BufferedReader(new InputStreamReader(socket.getInputStream(), StandardCharsets.UTF_8));
        out = new OutputStreamWriter(socket.getOutputStream(), StandardCharsets.UTF_8);
        Map<String, Object> hello = call(req("hello"));
        long v = ((Number) hello.get("version")).longValue();
        if (v != PROTOCOL_VERSION) {
            throw new BridgeException("OpenEye core speaks bridge protocol " + v + ", this client speaks " + PROTOCOL_VERSION);
        }
    }

    /** Parse "host:port". */
    public static BridgeClient connect(String hostPort) throws IOException {
        int c = hostPort.lastIndexOf(':');
        if (c < 0) throw new IllegalArgumentException("expected HOST:PORT, got " + hostPort);
        return new BridgeClient(hostPort.substring(0, c), Integer.parseInt(hostPort.substring(c + 1)));
    }

    private static Map<String, Object> req(String op) {
        Map<String, Object> m = new LinkedHashMap<>();
        m.put("op", op);
        return m;
    }

    private static Map<String, Object> req(String op, int handle) {
        Map<String, Object> m = req(op);
        m.put("handle", handle);
        return m;
    }

    @SuppressWarnings("unchecked")
    private synchronized Map<String, Object> call(Map<String, Object> request) {
        long id = nextId++;
        Map<String, Object> withId = new LinkedHashMap<>();
        withId.put("id", id);
        withId.putAll(request);
        try {
            out.write(Json.write(withId));
            out.write('\n');
            out.flush();
            String line = in.readLine();
            if (line == null) throw new BridgeException("OpenEye core closed the bridge connection");
            Map<String, Object> resp = (Map<String, Object>) Json.parse(line);
            if (!Boolean.TRUE.equals(resp.get("ok"))) {
                throw new BridgeException(String.valueOf(resp.get("error")));
            }
            return resp;
        } catch (IOException e) {
            throw new BridgeException("bridge I/O error: " + e.getMessage(), e);
        }
    }

    @SuppressWarnings("unchecked")
    private static ReadResult toRead(Map<String, Object> r) {
        Object rec = r.get("record");
        Object rrn = r.get("rrn");
        return new ReadResult(
                Boolean.TRUE.equals(r.get("found")),
                Boolean.TRUE.equals(r.get("eof")),
                (Map<String, String>) rec,
                rrn == null ? null : ((Number) rrn).longValue());
    }

    /** File description: name, library, format, keys, fields (name, type, length, decimals, numeric). */
    public Map<String, Object> metadata(String file) {
        Map<String, Object> m = req("metadata");
        m.put("file", file);
        return call(m);
    }

    /** Open a file found through the library list; returns its handle. */
    public int open(String file) {
        Map<String, Object> m = req("open");
        m.put("file", file);
        return ((Number) call(m).get("handle")).intValue();
    }

    public void closeFile(int handle) {
        call(req("close", handle));
    }

    /** SETLL; an empty key list means *LOVAL. Returns %EQUAL. */
    public boolean setll(int handle, List<String> keys) {
        Map<String, Object> m = req("setll", handle);
        m.put("keys", keys);
        return Boolean.TRUE.equals(call(m).get("equal"));
    }

    /** SETGT; an empty key list means *HIVAL. */
    public void setgt(int handle, List<String> keys) {
        Map<String, Object> m = req("setgt", handle);
        m.put("keys", keys);
        call(m);
    }

    public ReadResult chain(int handle, List<String> keys) {
        Map<String, Object> m = req("chain", handle);
        m.put("keys", keys);
        return toRead(call(m));
    }

    public ReadResult read(int handle) {
        return toRead(call(req("read", handle)));
    }

    public ReadResult readPrevious(int handle) {
        return toRead(call(req("readp", handle)));
    }

    /** READE; null keys = use the key of the last SETLL/SETGT/CHAIN. */
    public ReadResult readEqual(int handle, List<String> keys) {
        Map<String, Object> m = req("reade", handle);
        if (keys != null) m.put("keys", keys);
        return toRead(call(m));
    }

    public ReadResult readPreviousEqual(int handle, List<String> keys) {
        Map<String, Object> m = req("readpe", handle);
        if (keys != null) m.put("keys", keys);
        return toRead(call(m));
    }

    /** WRITE; returns the new record's RRN. */
    public long write(int handle, Map<String, String> record) {
        Map<String, Object> m = req("write", handle);
        m.put("record", record);
        return ((Number) call(m).get("rrn")).longValue();
    }

    /** UPDATE the last record read. */
    public void update(int handle, Map<String, String> record) {
        Map<String, Object> m = req("update", handle);
        m.put("record", record);
        call(m);
    }

    /** DELETE the last record read. */
    public void delete(int handle) {
        call(req("delete", handle));
    }

    @Override
    public void close() throws IOException {
        socket.close();
    }
}
