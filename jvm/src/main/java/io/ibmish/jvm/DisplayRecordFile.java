package io.ibmish.jvm;

import com.smeup.dbnative.file.DBFile;
import com.smeup.dbnative.file.Record;
import com.smeup.dbnative.file.RecordField;
import com.smeup.dbnative.file.Result;
import com.smeup.dbnative.log.Logger;
import com.smeup.dbnative.model.FileMetadata;
import io.ibmish.bridge.BridgeClient;
import io.ibmish.bridge.ReadResult;

import java.math.BigDecimal;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

/**
 * One display record format seen as a file: WRITE puts the record on the
 * screen (shown with the next EXFMT), and for subfile records WRITE, CHAIN,
 * READ (= READC, see {@link Workstation}) and UPDATE work on the subfile kept
 * by ibmish core.
 */
final class DisplayRecordFile implements DBFile {
    private final Workstation.Format format;
    private final BridgeClient client;
    private String name;
    private FileMetadata metadata;
    private Logger logger;
    private boolean lastEof;
    private boolean lastFound;

    DisplayRecordFile(Workstation.Format format, FileMetadata metadata, BridgeClient client) {
        this.format = format;
        this.name = format.name;
        this.metadata = metadata;
        this.client = client;
    }

    @Override public String getName() { return name; }
    @Override public void setName(String name) { this.name = name; }
    @Override public FileMetadata getFileMetadata() { return metadata; }
    @Override public void setFileMetadata(FileMetadata m) { this.metadata = m; }
    @Override public Logger getLogger() { return logger; }
    @Override public void setLogger(Logger logger) { this.logger = logger; }
    @Override public boolean eof() { return lastEof; }
    @Override public boolean equal() { return lastFound; }

    private UnsupportedOperationException notHere(String op) {
        return new UnsupportedOperationException(op + " is not valid for display record format " + format.name
                + (format.isSubfile() ? "" : " (it is not a subfile record)"));
    }

    private void requireSubfile(String op) {
        if (!format.isSubfile()) throw notHere(op);
    }

    private static Map<String, String> values(Record r) {
        Map<String, String> m = new LinkedHashMap<>();
        for (Map.Entry<String, String> e : r.entrySet()) {
            m.put(e.getKey().toUpperCase(), e.getValue() == null ? "" : e.getValue());
        }
        return m;
    }

    private Result result(ReadResult r) {
        lastFound = r.found;
        Record rec = new Record();
        if (r.found && r.record != null) {
            for (Map.Entry<String, String> e : r.record.entrySet()) rec.add(new RecordField(e.getKey(), e.getValue()));
        }
        Result res = new Result();
        res.setRecord(rec);
        if (r.rrn != null) res.setRrn(r.rrn);
        return res;
    }

    @Override
    public Result write(Record record) {
        if (format.isSubfile()) {
            long rrn = Workstation.numberVariable(format.rrnField);
            client.subfileWrite(format.file, format.name, rrn, values(record), Workstation.indicators());
        } else {
            client.displayWrite(format.file, format.name, values(record), Workstation.indicators());
        }
        return new Result();
    }

    @Override
    public Result update(Record record) {
        requireSubfile("UPDATE");
        client.subfileUpdate(format.file, format.name, values(record), Workstation.indicators());
        return new Result();
    }

    @Override
    public Result chain(String key) {
        requireSubfile("CHAIN");
        lastEof = false;
        long rrn = new BigDecimal(key.trim()).longValueExact();
        return result(client.subfileChain(format.file, format.name, rrn));
    }

    @Override
    public Result chain(List<String> keys) {
        if (keys.size() != 1) throw new IllegalArgumentException("CHAIN to a subfile takes one key, the relative record number");
        return chain(keys.get(0));
    }

    /** READC (the preprocessor turns READC into READ for subfile formats). */
    @Override
    public Result read() {
        requireSubfile("READC");
        ReadResult r = client.subfileReadChanged(format.file, format.name);
        lastEof = !r.found;
        if (r.found && r.rrn != null) Workstation.setNumberVariable(format.rrnField, r.rrn);
        return result(r);
    }

    @Override public Result readPrevious() { throw notHere("READP"); }
    @Override public Result readEqual() { throw notHere("READE"); }
    @Override public Result readEqual(String key) { throw notHere("READE"); }
    @Override public Result readEqual(List<String> keys) { throw notHere("READE"); }
    @Override public Result readPreviousEqual() { throw notHere("READPE"); }
    @Override public Result readPreviousEqual(String key) { throw notHere("READPE"); }
    @Override public Result readPreviousEqual(List<String> keys) { throw notHere("READPE"); }
    @Override public boolean setll(String key) { throw notHere("SETLL"); }
    @Override public boolean setll(List<String> keys) { throw notHere("SETLL"); }
    @Override public boolean setgt(String key) { throw notHere("SETGT"); }
    @Override public boolean setgt(List<String> keys) { throw notHere("SETGT"); }
    @Override public Result delete(Record record) { throw notHere("DELETE"); }

    @Override
    public void close() {
        // nothing to release: the subfile lives as long as the program call
    }
}
