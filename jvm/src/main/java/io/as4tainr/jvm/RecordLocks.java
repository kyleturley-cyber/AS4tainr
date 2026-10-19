package io.as4tainr.jvm;

import java.util.ArrayList;
import java.util.HashMap;
import java.util.HashSet;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.Set;
import java.util.regex.Matcher;
import java.util.regex.Pattern;

/**
 * Record locks for RPG programs on Jariko.
 *
 * <p>On IBM i a file opened for update (file type U) locks every record it
 * reads, unless the read has the N extender; the lock lasts until UPDATE,
 * DELETE, the next read of the file, UNLOCK or close. A read of a record
 * another job holds waits, then fails; with an error indicator (positions
 * 73-74) or the E extender the program goes on with the indicator on.
 *
 * <p>Jariko v2.2.0 drops what that needs: the file's type, the N and E
 * extenders, and UNLOCK (a no-op). So, before parsing, {@link #preprocess}
 * reads the F specs, and puts two statements in front of each read with N, E
 * or an error indicator, and of each UNLOCK:
 *
 * <pre>
 *     C                   EVAL      IBM#SG = 'N CUSTMAST'
 *     C                   OUT       IBM#SG
 * </pre>
 *
 * IBM#SG is DEFINEd as the data area IBM#SIGNAL, so OUT reaches the runner's
 * data area callback, which hands the text to {@link #signal}. The next read
 * of that file then knows its flags ({@link #takeFlags}). The statements get
 * the same conditioning indicators as the operation they announce.
 */
public final class RecordLocks {
    private RecordLocks() {}

    /** The data area name the signal statements write to. */
    public static final String SIGNAL_AREA = "IBM#SIGNAL";
    static final String SIGNAL_FIELD = "IBM#SG";

    private static final Set<String> UPDATE = new HashSet<>();
    private static final Map<String, String> PENDING = new HashMap<>();
    private static final Map<String, String> FORMAT_OF = new HashMap<>();
    private static final Map<String, String> FILE_OF_FORMAT = new HashMap<>();
    private static final Map<String, Runnable> UNLOCKERS = new HashMap<>();

    private static String up(String s) {
        return s.trim().toUpperCase(Locale.ROOT);
    }

    /** Is this file opened for update by the programs loaded so far? */
    public static synchronized boolean isUpdate(String file) {
        return UPDATE.contains(up(file));
    }

    /** An open file: its record format name, and how to release its lock (UNLOCK). */
    public static synchronized void register(String file, String format, Runnable unlock) {
        String f = up(file);
        if (format != null && !format.isBlank()) {
            FORMAT_OF.put(f, up(format));
            FILE_OF_FORMAT.put(up(format), f);
        }
        UNLOCKERS.put(f, unlock);
    }

    /** The flags announced for the next read of this file ("N", "E", "NE" or ""). */
    public static synchronized String takeFlags(String file) {
        String f = up(file);
        String flags = PENDING.remove(f);
        String fmt = FORMAT_OF.get(f);
        if (fmt != null) {
            String byFormat = PENDING.remove(fmt);
            if (flags == null) flags = byFormat;
        }
        return flags == null ? "" : flags;
    }

    /**
     * A signal from the program: "flags NAME" before a read, "U NAME" for
     * UNLOCK. NAME is a file or record format. Returns false for an UNLOCK of
     * something that is not an open file (a data area, for the caller).
     */
    public static boolean signal(String text) {
        String[] parts = up(text).split("\\s+");
        if (parts.length != 2) return true;
        String flags = parts[0];
        String name = parts[1];
        Runnable unlock;
        synchronized (RecordLocks.class) {
            String file = FILE_OF_FORMAT.getOrDefault(name, name);
            if (!flags.equals("U")) {
                PENDING.put(file, flags.replace("-", ""));
                return true;
            }
            unlock = UNLOCKERS.get(file);
        }
        if (unlock == null) return false;
        unlock.run();
        return true;
    }

    // ------------------------------------------------ source rewriting

    private static final Set<String> READS = Set.of("CHAIN", "READ", "READE", "READP", "READPE");
    private static final Pattern FREE_DCLF = Pattern.compile(
            "(?i)^\\s*DCL-F\\s+([A-Z0-9_#@$]+)\\b([^;]*)");

    private static String cols(String line, int from, int to) {
        if (line.length() < from) return "";
        return line.substring(from - 1, Math.min(to, line.length()));
    }

    private static String pad(String s, int n) {
        StringBuilder b = new StringBuilder(s);
        while (b.length() < n) b.append(' ');
        return b.toString();
    }

    private static boolean isSpec(String line, char type) {
        return Character.toUpperCase(cols(line, 6, 6).isEmpty() ? ' ' : cols(line, 6, 6).charAt(0)) == type
                && !cols(line, 7, 7).equals("*");
    }

    /** One fixed-form C spec: conditioning (positions 7-11), factor 1, opcode, factor 2. */
    private static String cspec(String cond, String f1, String op, String rest) {
        return "     C" + pad(cond, 5) + pad(f1, 14) + pad(op, 10) + rest;
    }

    /** A signal statement: IBM#SG = 'text', with the given conditioning (positions 7-11). */
    static String signalLine(String cond, String text) {
        return cspec(cond, "", "EVAL", SIGNAL_FIELD + " = '" + text + "'");
    }

    /** The OUT that sends a signal. */
    static String outLine(String cond) {
        return cspec(cond, "", "OUT", SIGNAL_FIELD);
    }

    /** Rewrite a program as the class comment describes. Free-form reads are left as they are. */
    public static synchronized String preprocess(String source) {
        String[] lines = source.split("\r?\n", -1);
        List<String> out = new ArrayList<>();
        boolean signals = false;
        for (String line : lines) {
            // Signals other rewrites put in (Printers: EXCEPT).
            if (isSpec(line, 'C') && up(cols(line, 36, 80)).startsWith(SIGNAL_FIELD)) signals = true;
            if (isSpec(line, 'F')) {
                String name = up(cols(line, 7, 16));
                String device = up(cols(line, 36, 42));
                if (!name.isEmpty() && cols(line, 17, 17).equalsIgnoreCase("U")
                        && device.equals("DISK") && Workstation.format(name) == null) {
                    UPDATE.add(name);
                }
                out.add(line);
                continue;
            }
            Matcher dclf = FREE_DCLF.matcher(line);
            if (cols(line, 6, 6).isBlank() && dclf.find()) {
                String rest = up(dclf.group(2));
                if (rest.contains("*UPDATE") && !rest.contains("WORKSTN")) UPDATE.add(up(dclf.group(1)));
            }
            if (isSpec(line, 'C')) {
                String op = up(cols(line, 26, 35));
                String base = op.contains("(") ? op.substring(0, op.indexOf('(')) : op;
                String ext = op.contains("(") ? op.substring(op.indexOf('(') + 1).replace(")", "") : "";
                String f2 = up(cols(line, 36, 49));
                String flags = null;
                if (READS.contains(base)) {
                    String f = "";
                    if (ext.contains("N")) f += "N";
                    if (ext.contains("E") || !cols(line, 73, 74).isBlank()) f += "E";
                    if (!f.isEmpty()) flags = f;
                } else if (base.equals("UNLOCK") && !f2.isEmpty()) {
                    flags = "U";
                }
                if (flags != null && !f2.isEmpty()) {
                    String cond = cols(line, 7, 11);
                    out.add(signalLine(cond, flags + " " + f2));
                    out.add(outLine(cond));
                    signals = true;
                }
            }
            out.add(line);
        }
        if (!signals) return String.join("\n", out);

        // The signal field and its DEFINE: the D spec before the first D, I
        // or C spec, the DEFINE before the first C spec.
        int firstD = -1;
        int firstC = -1;
        for (int i = 0; i < out.size(); i++) {
            String l = out.get(i);
            if (firstD < 0 && (isSpec(l, 'D') || isSpec(l, 'I') || isSpec(l, 'C'))) firstD = i;
            if (firstC < 0 && isSpec(l, 'C')) firstC = i;
        }
        out.add(firstC, cspec("", "*DTAARA", "DEFINE", pad(SIGNAL_AREA, 14) + SIGNAL_FIELD));
        char[] d = pad("", 39).toCharArray();
        d[5] = 'D';
        SIGNAL_FIELD.getChars(0, SIGNAL_FIELD.length(), d, 6);
        d[23] = 'S';
        d[37] = '3';
        d[38] = '0';
        out.add(firstD, new String(d));
        return String.join("\n", out);
    }
}
