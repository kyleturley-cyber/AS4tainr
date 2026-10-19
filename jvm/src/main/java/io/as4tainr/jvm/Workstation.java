package io.as4tainr.jvm;

import com.smeup.rpgparser.execution.MainExecutionContext;
import com.smeup.rpgparser.interpreter.AbstractDataDefinition;
import com.smeup.rpgparser.interpreter.BooleanValue;
import com.smeup.rpgparser.interpreter.DecimalValue;
import com.smeup.rpgparser.interpreter.InterpreterCore;
import com.smeup.rpgparser.interpreter.NumberValue;
import com.smeup.rpgparser.interpreter.Value;

import java.math.BigDecimal;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.regex.Matcher;
import java.util.regex.Pattern;

/**
 * Display file record formats used with WRITE, UPDATE, CHAIN and READC.
 *
 * <p>Jariko v2.2.0 runs EXFMT through a callback, but has no WORKSTN support
 * for the other operations: WRITE and UPDATE look for a database file, and
 * READC does nothing. So before a program is parsed, {@link #preprocess}
 * declares each record format of its WORKSTN files as an extra DISK file
 * (served by {@link DisplayDBMManager}, which sends the operations to as4tainr
 * core), takes the SFILE keywords off the F specs, and turns READC of a
 * subfile into READ of that "file". Core keeps the subfile and the records on
 * the screen; see docs/display-files.md.
 */
public final class Workstation {
    private Workstation() {}

    /** A record format of a display file, as the program uses it. */
    public static final class Format {
        public final String file;
        public final String name;
        /** Program variable named in SFILE(format:rrn), or null for other formats. */
        public final String rrnField;

        Format(String file, String name, String rrnField) {
            this.file = file;
            this.name = name;
            this.rrnField = rrnField;
        }

        public boolean isSubfile() {
            return rrnField != null;
        }
    }

    private static final Map<String, Format> FORMATS = new LinkedHashMap<>();

    public static synchronized Format format(String name) {
        return FORMATS.get(name.toUpperCase(Locale.ROOT));
    }

    public static synchronized List<Format> formats() {
        return new ArrayList<>(FORMATS.values());
    }

    private static final Pattern SFILE = Pattern.compile("(?i)SFILE\\(\\s*([A-Z0-9_#@$]+)\\s*:\\s*([A-Z0-9_#@$]+)\\s*\\)");
    private static final Pattern FREE_READC = Pattern.compile("(?i)\\bREADC(\\s+)([A-Z0-9_#@$]+)");

    private static String cols(String line, int from, int to) {
        if (line.length() < from) return "";
        return line.substring(from - 1, Math.min(to, line.length()));
    }

    private static String pad(String s, int n) {
        StringBuilder b = new StringBuilder(s);
        while (b.length() < n) b.append(' ');
        return b.toString();
    }

    /** The record format names of a display file. */
    public interface FormatSource {
        List<String> formats(String displayFile);
    }

    /**
     * Rewrite a fixed-form program for the display files it uses (see the
     * class comment). Free-form READC is rewritten too; free-form DCL-F is
     * not supported yet.
     */
    public static synchronized String preprocess(String source, FormatSource displays) {
        String[] lines = source.split("\r?\n", -1);
        List<String> out = new ArrayList<>();
        Map<String, String> sfile = new LinkedHashMap<>(); // format -> rrn field
        List<String> workstnFiles = new ArrayList<>();
        int lastF = -1;
        boolean inWorkstn = false;

        for (String line : lines) {
            String type = cols(line, 6, 6).toUpperCase(Locale.ROOT);
            boolean comment = cols(line, 7, 7).equals("*");
            if (type.equals("F") && !comment) {
                String name = cols(line, 7, 16).trim();
                if (!name.isEmpty()) {
                    inWorkstn = cols(line, 36, 42).trim().equalsIgnoreCase("WORKSTN");
                    if (inWorkstn) workstnFiles.add(name.toUpperCase(Locale.ROOT));
                }
                if (inWorkstn) {
                    Matcher m = SFILE.matcher(line);
                    StringBuffer b = new StringBuffer();
                    while (m.find()) {
                        sfile.put(m.group(1).toUpperCase(Locale.ROOT), m.group(2).toUpperCase(Locale.ROOT));
                        m.appendReplacement(b, " ".repeat(m.group().length()));
                    }
                    m.appendTail(b);
                    line = b.toString();
                    if (name.isEmpty() && cols(line, 7, 80).trim().isEmpty()) {
                        // Nothing left on the continuation line: keep it as a comment.
                        line = pad(cols(line, 1, 5), 5) + "F* (SFILE: see as4tainr Workstation)";
                    }
                }
                out.add(line);
                lastF = out.size() - 1;
                continue;
            }
            out.add(line);
        }

        // Every record format of every WORKSTN file becomes a DISK file.
        List<String> extra = new ArrayList<>();
        for (String file : workstnFiles) {
            for (String fmt : displays.formats(file)) {
                String f = fmt.toUpperCase(Locale.ROOT);
                FORMATS.put(f, new Format(file, f, sfile.get(f)));
                extra.add("     F" + pad(f, 10) + "UF A E           K DISK");
            }
        }
        for (String f : sfile.keySet()) {
            Format known = FORMATS.get(f);
            if (known == null || !known.isSubfile()) {
                throw new IllegalArgumentException("SFILE(" + f + ":...): " + f + " is not a record format of a WORKSTN file");
            }
        }
        if (lastF >= 0) out.addAll(lastF + 1, extra);

        // READC of a subfile -> READ of its DISK file.
        for (int i = 0; i < out.size(); i++) {
            String line = out.get(i);
            if (cols(line, 6, 6).equalsIgnoreCase("C") && !cols(line, 7, 7).equals("*")) {
                String op = cols(line, 26, 35).trim().toUpperCase(Locale.ROOT);
                String f2 = cols(line, 36, 49).trim().toUpperCase(Locale.ROOT);
                if (op.equals("READC") && sfile.containsKey(f2)) {
                    out.set(i, line.substring(0, 25) + pad("READ", 10) + line.substring(35));
                }
            } else if (!cols(line, 6, 6).trim().isEmpty() || line.trim().isEmpty()) {
                // other fixed-form specs and blank lines: nothing to do
            } else {
                Matcher m = FREE_READC.matcher(line);
                StringBuffer b = new StringBuffer();
                while (m.find()) {
                    String rep = sfile.containsKey(m.group(2).toUpperCase(Locale.ROOT))
                            ? "READ " + m.group(1) + m.group(2) : m.group();
                    m.appendReplacement(b, Matcher.quoteReplacement(rep));
                }
                m.appendTail(b);
                out.set(i, b.toString());
            }
        }
        return String.join("\n", out);
    }

    // ------------------------------------------------ the running program

    static InterpreterCore interpreter() {
        return MainExecutionContext.INSTANCE.getProgramStack().peek().getIntepreterCore();
    }

    /** *IN01-*IN99 as 99 characters '0'/'1'. */
    public static String indicators() {
        Map<Integer, BooleanValue> ind = interpreter().getIndicators();
        StringBuilder b = new StringBuilder(99);
        for (int i = 1; i <= 99; i++) {
            BooleanValue v = ind.get(i);
            b.append(v != null && v.getValue() ? '1' : '0');
        }
        return b.toString();
    }

    /** The value of a numeric program variable (the SFILE relative record number). */
    public static long numberVariable(String name) {
        Value v = interpreter().get(name);
        if (v instanceof NumberValue) return ((NumberValue) v).getBigDecimal().longValueExact();
        return new BigDecimal(v.asString().getValue().trim()).longValueExact();
    }

    public static void setNumberVariable(String name, long value) {
        InterpreterCore core = interpreter();
        AbstractDataDefinition def = core.dataDefinitionByName(name);
        if (def == null) throw new IllegalStateException("variable " + name + " is not defined");
        core.assign(def, new DecimalValue(BigDecimal.valueOf(value)));
    }
}
