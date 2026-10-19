package io.ibmish.bridge;

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
 * Client for the ibmish core bridge: one JSON object per line over a loopback
 * TCP socket. One client is one job's database session. Thread-safe (calls are
 * serialized), but a job normally uses it from one thread.
 *
 * See docs/bridge-protocol.md.
 */
public final class BridgeClient implements AutoCloseable {
    public static final int PROTOCOL_VERSION = 6;

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
            throw new BridgeException("ibmish core speaks bridge protocol " + v + ", this client speaks " + PROTOCOL_VERSION);
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
            if (line == null) throw new BridgeException("ibmish core closed the bridge connection");
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

    /** Open a file found through the library list for input; returns its handle. */
    public int open(String file) {
        return open(file, false);
    }

    /**
     * Open a file found through the library list; returns its handle. A file
     * opened for {@code update} (RPG file type U) locks each record it reads
     * until UPDATE, DELETE, the next read, {@link #unlock} or close; reading a
     * record another job holds waits, then fails with CPF5027.
     */
    public int open(String file, boolean update) {
        Map<String, Object> m = req("open");
        m.put("file", file);
        if (update) m.put("update", true);
        return ((Number) call(m).get("handle")).intValue();
    }

    public void closeFile(int handle) {
        call(req("close", handle));
    }

    /** UNLOCK: release the record this open file holds locked. */
    public void unlock(int handle) {
        call(req("unlock", handle));
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

    private static Map<String, Object> readReq(String op, int handle, boolean lock) {
        Map<String, Object> m = req(op, handle);
        if (!lock) m.put("lock", false);
        return m;
    }

    public ReadResult chain(int handle, List<String> keys) {
        return chain(handle, keys, true);
    }

    /** CHAIN; {@code lock=false} is the N extender (read without locking). */
    public ReadResult chain(int handle, List<String> keys, boolean lock) {
        Map<String, Object> m = readReq("chain", handle, lock);
        m.put("keys", keys);
        return toRead(call(m));
    }

    public ReadResult read(int handle) {
        return read(handle, true);
    }

    public ReadResult read(int handle, boolean lock) {
        return toRead(call(readReq("read", handle, lock)));
    }

    public ReadResult readPrevious(int handle) {
        return readPrevious(handle, true);
    }

    public ReadResult readPrevious(int handle, boolean lock) {
        return toRead(call(readReq("readp", handle, lock)));
    }

    /** READE; null keys = use the key of the last SETLL/SETGT/CHAIN. */
    public ReadResult readEqual(int handle, List<String> keys) {
        return readEqual(handle, keys, true);
    }

    public ReadResult readEqual(int handle, List<String> keys, boolean lock) {
        Map<String, Object> m = readReq("reade", handle, lock);
        if (keys != null) m.put("keys", keys);
        return toRead(call(m));
    }

    public ReadResult readPreviousEqual(int handle, List<String> keys) {
        return readPreviousEqual(handle, keys, true);
    }

    public ReadResult readPreviousEqual(int handle, List<String> keys, boolean lock) {
        Map<String, Object> m = readReq("readpe", handle, lock);
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

    /** A display file's DDS source: library, name, source, formats. */
    public Map<String, Object> displayFile(String file) {
        Map<String, Object> m = req("dspf");
        m.put("file", file);
        return call(m);
    }

    /**
     * EXFMT: show a record format on the job's display and wait for input.
     *
     * @param fields     current values of the record's fields
     * @param indicators indicators 01-99 as 99 characters '0'/'1'
     * @return aid (e.g. "ENTER", "F3"), fields (new values of input fields),
     *         indicators (response indicators to set, "03" -> true/false)
     */
    public Map<String, Object> exfmt(String file, String format, Map<String, String> fields, String indicators) {
        Map<String, Object> m = req("exfmt");
        m.put("file", file);
        m.put("format", format);
        m.put("fields", fields);
        m.put("indicators", indicators);
        return call(m);
    }

    /** WRITE of a display record format (not a subfile record): shown with the next EXFMT. */
    public void displayWrite(String file, String format, Map<String, String> fields, String indicators) {
        Map<String, Object> m = req("dspwrite");
        m.put("file", file);
        m.put("format", format);
        m.put("fields", fields);
        m.put("indicators", indicators);
        call(m);
    }

    /** WRITE of a subfile record at relative record number {@code rrn}. */
    public void subfileWrite(String file, String format, long rrn, Map<String, String> fields, String indicators) {
        Map<String, Object> m = req("sflwrite");
        m.put("file", file);
        m.put("format", format);
        m.put("rrn", rrn);
        m.put("fields", fields);
        m.put("indicators", indicators);
        call(m);
    }

    /** CHAIN to a subfile record by relative record number. */
    public ReadResult subfileChain(String file, String format, long rrn) {
        Map<String, Object> m = req("sflchain");
        m.put("file", file);
        m.put("format", format);
        m.put("rrn", rrn);
        return toRead(call(m));
    }

    /** READC: the next subfile record the operator changed (eof when none is left). */
    public ReadResult subfileReadChanged(String file, String format) {
        Map<String, Object> m = req("sflreadc");
        m.put("file", file);
        m.put("format", format);
        return toRead(call(m));
    }

    /** UPDATE the subfile record last read with READC or CHAIN. */
    public void subfileUpdate(String file, String format, Map<String, String> fields, String indicators) {
        Map<String, Object> m = req("sflupdate");
        m.put("file", file);
        m.put("format", format);
        m.put("fields", fields);
        m.put("indicators", indicators);
        call(m);
    }

    /**
     * IN: read a data area found through the library list. With {@code lock}
     * (IN *LOCK) this session holds the data area until {@link #dataAreaOut}
     * or {@link #dataAreaUnlock}, or until the connection closes.
     *
     * @return library, name, type (*CHAR, *DEC, *LGL), length, decimals, value
     */
    public Map<String, Object> dataAreaIn(String name, boolean lock) {
        Map<String, Object> m = req("dtaara_in");
        m.put("name", name);
        m.put("lock", lock);
        return call(m);
    }

    /** OUT: write a data area's whole value (decimal text for *DEC); releases this session's lock. */
    public void dataAreaOut(String name, String value) {
        Map<String, Object> m = req("dtaara_out");
        m.put("name", name);
        m.put("value", value);
        call(m);
    }

    /** UNLOCK: release a lock taken with IN *LOCK, without writing. */
    public void dataAreaUnlock(String name) {
        Map<String, Object> m = req("dtaara_unlock");
        m.put("name", name);
        call(m);
    }

    /**
     * The source of an RPG program object (CRTBNDRPG) in {@code library}, or
     * through the library list when it is null or empty.
     *
     * @return the source, or null when there is no such RPG program
     */
    public String programSource(String name, String library) {
        Map<String, Object> m = req("pgmsrc");
        m.put("name", name);
        if (library != null && !library.isEmpty()) m.put("library", library);
        Map<String, Object> r = call(m);
        return Boolean.TRUE.equals(r.get("found")) ? (String) r.get("source") : null;
    }

    /**
     * A source member (/COPY): {@code file} defaults to QRPGLESRC,
     * {@code library} to the library list.
     *
     * @return the source, or null when there is no such member
     */
    public String sourceMember(String library, String file, String member) {
        Map<String, Object> m = req("srcmbr");
        if (library != null && !library.isEmpty()) m.put("library", library);
        if (file != null && !file.isEmpty()) m.put("file", file);
        m.put("member", member);
        Map<String, Object> r = call(m);
        return Boolean.TRUE.equals(r.get("found")) ? (String) r.get("source") : null;
    }

    /** Start a spooled file (printer file output) for this job; returns its id. */
    public long spoolOpen(String file, String userData) {
        Map<String, Object> m = req("splopen");
        m.put("file", file);
        m.put("user_data", userData);
        return ((Number) call(m).get("spl")).longValue();
    }

    /** Add lines to a spooled file: each is {page, line, text}. */
    public void spoolWrite(long spl, List<Object[]> lines) {
        Map<String, Object> m = req("splwrite");
        m.put("spl", spl);
        List<Object> ls = new java.util.ArrayList<>();
        for (Object[] l : lines) ls.add(java.util.Arrays.asList(l));
        m.put("lines", ls);
        call(m);
    }

    /** The spooled file is complete. */
    public void spoolClose(long spl) {
        Map<String, Object> m = req("splclose");
        m.put("spl", spl);
        call(m);
    }

    @Override
    public void close() throws IOException {
        socket.close();
    }
}
