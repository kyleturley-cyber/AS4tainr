package io.as4tainr.bridge;

import java.util.Collections;
import java.util.Map;

/** Outcome of CHAIN, READ, READP, READE or READPE. */
public final class ReadResult {
    /** %FOUND for CHAIN; for reads, whether a record came back. */
    public final boolean found;
    /** %EOF after a read (beginning of file for READP/READPE). CHAIN never sets it. */
    public final boolean eof;
    /** Field values by field name; empty when nothing was read. Character fields keep trailing blanks. */
    public final Map<String, String> record;
    /** Relative record number, or null. */
    public final Long rrn;

    ReadResult(boolean found, boolean eof, Map<String, String> record, Long rrn) {
        this.found = found;
        this.eof = eof;
        this.record = record == null ? Collections.emptyMap() : record;
        this.rrn = rrn;
    }

    @Override
    public String toString() {
        return "ReadResult{found=" + found + ", eof=" + eof + ", rrn=" + rrn + ", record=" + record + "}";
    }
}
