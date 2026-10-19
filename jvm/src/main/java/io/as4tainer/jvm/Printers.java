package io.as4tainer.jvm;

import com.smeup.rpgparser.interpreter.AbstractDataDefinition;
import com.smeup.rpgparser.interpreter.BooleanValue;
import com.smeup.rpgparser.interpreter.DecimalValue;
import com.smeup.rpgparser.interpreter.InterpreterCore;
import com.smeup.rpgparser.interpreter.NumberType;
import com.smeup.rpgparser.interpreter.NumberValue;
import com.smeup.rpgparser.interpreter.StringValue;
import com.smeup.rpgparser.interpreter.Type;
import com.smeup.rpgparser.interpreter.Value;
import com.smeup.rpgparser.parsing.ast.IndicatorsKt;
import io.as4tainer.bridge.BridgeClient;

import java.math.BigDecimal;
import java.math.RoundingMode;
import java.time.LocalDate;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;

/**
 * Program-described printer files: O specs and EXCEPT.
 *
 * <p>Jariko v2.2.0 parses output specifications but does nothing with them,
 * and EXCEPT is not implemented. So before parsing, {@link #preprocess}
 * reads the PRINTER F specs and the O specs of those files, and replaces
 * each EXCEPT with a signal (see {@link RecordLocks}): {@code X name}. On the
 * signal, {@link #except} prints the exception (E) records with that EXCEPT
 * name whose conditioning indicators are on, laying out each line from the
 * program's fields (end positions, edit codes, constants, blank after) and
 * moving down the page (space and skip, before and after). Pages are 66
 * lines with overflow at line 60: reaching it turns on the file's overflow
 * indicator (OFLIND). The lines go to as4tainer core as a spooled file
 * (bridge splopen/splwrite/splclose), closed when the program ends.
 */
public final class Printers {
    private Printers() {}

    static final int PAGE_LENGTH = 66;
    static final int OVERFLOW_LINE = 60;

    /** A conditioning indicator: (negated, name such as 01, OF, LR). */
    static final class Cond {
        final boolean not;
        final String ind;

        Cond(boolean not, String ind) {
            this.not = not;
            this.ind = ind;
        }
    }

    static final class Field {
        List<List<Cond>> cond = new ArrayList<>();
        String name;      // null for a constant
        String constant;  // text of a constant
        char editCode;    // ' ' for none
        boolean blankAfter;
        int end;          // end position (absolute)
    }

    static final class Rec {
        String file;
        char type;
        String exceptName;
        List<List<Cond>> cond = new ArrayList<>(); // OR groups of AND conditions
        int spaceBefore = -1, spaceAfter = -1, skipBefore = -1, skipAfter = -1;
        List<Field> fields = new ArrayList<>();
    }

    static final class Printer {
        final String file;
        final int length;
        final String overflowInd;
        long spl;
        boolean open;
        int page = 1;
        int line = 1;
        boolean printedOnPage;
        final List<Object[]> buffer = new ArrayList<>();

        Printer(String file, int length, String overflowInd) {
            this.file = file;
            this.length = length;
            this.overflowInd = overflowInd;
        }
    }

    private static final Map<String, Printer> PRINTERS = new LinkedHashMap<>();
    private static final List<Rec> RECORDS = new ArrayList<>();
    private static String program = "";

    private static String cols(String line, int from, int to) {
        if (line.length() < from) return "";
        return line.substring(from - 1, Math.min(to, line.length()));
    }

    private static String up(String s) {
        return s.trim().toUpperCase(Locale.ROOT);
    }

    private static boolean isSpec(String line, char type) {
        String t = cols(line, 6, 6);
        return !t.isEmpty() && Character.toUpperCase(t.charAt(0)) == type && !cols(line, 7, 7).equals("*");
    }

    private static int num(String s, int dflt) {
        s = s.trim();
        if (s.isEmpty()) return dflt;
        try {
            return Integer.parseInt(s);
        } catch (NumberFormatException e) {
            return dflt;
        }
    }

    /** Conditioning indicators in positions 21-29: up to three of N + 2 characters. */
    private static List<Cond> conds(String line) {
        List<Cond> out = new ArrayList<>();
        for (int p = 21; p <= 27; p += 3) {
            String ind = up(cols(line, p + 1, p + 2));
            if (ind.isEmpty()) continue;
            out.add(new Cond(cols(line, p, p).equalsIgnoreCase("N"), ind));
        }
        return out;
    }

    /**
     * Read PRINTER F specs and their O specs; replace EXCEPT operations with
     * signals. Programs without printer files come back unchanged.
     */
    public static synchronized String preprocess(String source, String programName) {
        String[] lines = source.split("\r?\n", -1);
        Map<String, Printer> found = new LinkedHashMap<>();
        String lastF = null;
        for (String line : lines) {
            if (!isSpec(line, 'F')) continue;
            String name = up(cols(line, 7, 16));
            if (!name.isEmpty()) {
                lastF = null;
                if (up(cols(line, 36, 42)).equals("PRINTER") && up(cols(line, 22, 22)).equals("F")) {
                    lastF = name;
                    found.put(name, new Printer(name, num(cols(line, 23, 27), 132), null));
                }
            }
            if (lastF != null) {
                String kw = up(cols(line, 44, 80));
                int i = kw.indexOf("OFLIND(");
                if (i >= 0) {
                    String ind = kw.substring(i + 7, kw.indexOf(')', i)).replace("*IN", "").trim();
                    Printer p = found.get(lastF);
                    found.put(lastF, new Printer(p.file, p.length, ind));
                }
            }
        }
        if (found.isEmpty()) return source;
        program = programName.toUpperCase(Locale.ROOT);
        PRINTERS.putAll(found);

        // O specs.
        Rec rec = null;
        String file = null;
        for (String line : lines) {
            if (!isSpec(line, 'O')) continue;
            String name = up(cols(line, 7, 16));
            String andOr = up(cols(line, 16, 18));
            char type = Character.toUpperCase(cols(line, 17, 17).isEmpty() ? ' ' : cols(line, 17, 17).charAt(0));
            if (andOr.equals("AND") || andOr.equals("OR")) {
                if (rec == null) continue;
                if (andOr.equals("OR")) rec.cond.add(conds(line));
                else rec.cond.get(rec.cond.size() - 1).addAll(conds(line));
                continue;
            }
            if ("HDTE".indexOf(type) >= 0 && type != ' ') {
                if (!name.isEmpty()) file = name;
                rec = null;
                if (file == null || !PRINTERS.containsKey(file)) continue;
                rec = new Rec();
                rec.file = file;
                rec.type = type;
                rec.exceptName = up(cols(line, 30, 39));
                rec.cond.add(conds(line));
                rec.spaceBefore = num(cols(line, 40, 42), -1);
                rec.spaceAfter = num(cols(line, 43, 45), -1);
                rec.skipBefore = num(cols(line, 46, 48), -1);
                rec.skipAfter = num(cols(line, 49, 51), -1);
                RECORDS.add(rec);
                continue;
            }
            if (rec == null) continue;
            Field f = new Field();
            f.cond.add(conds(line));
            String fname = up(cols(line, 30, 43));
            String edit = cols(line, 44, 44);
            f.editCode = edit.isBlank() ? ' ' : Character.toUpperCase(edit.charAt(0));
            f.blankAfter = cols(line, 45, 45).equalsIgnoreCase("B");
            String endText = cols(line, 47, 51).trim();
            int prevEnd = rec.fields.isEmpty() ? 0 : rec.fields.get(rec.fields.size() - 1).end;
            f.end = endText.startsWith("+") ? prevEnd + num(endText.substring(1), 0) : num(endText, 0);
            String rest = cols(line, 53, 80).trim();
            if (fname.isEmpty()) {
                if (rest.startsWith("'")) {
                    int close = rest.lastIndexOf('\'');
                    f.constant = rest.substring(1, close > 0 ? close : rest.length()).replace("''", "'");
                } else {
                    continue;
                }
            } else {
                f.name = fname;
            }
            rec.fields.add(f);
        }

        // EXCEPT -> signal.
        List<String> out = new ArrayList<>();
        for (String line : lines) {
            if (isSpec(line, 'C')) {
                String op = up(cols(line, 26, 35));
                String base = op.contains("(") ? op.substring(0, op.indexOf('(')) : op;
                if (base.equals("EXCEPT")) {
                    String name = up(cols(line, 36, 49));
                    String cond = cols(line, 7, 11);
                    out.add(RecordLocks.signalLine(cond, "X " + (name.isEmpty() ? "*NONE" : name)));
                    out.add(RecordLocks.outLine(cond));
                    continue;
                }
            }
            out.add(line);
        }
        return String.join("\n", out);
    }

    // ------------------------------------------------ running

    private static boolean indicator(InterpreterCore core, String ind) {
        if (ind.equals("1P")) return false;
        try {
            int key = IndicatorsKt.toIndicatorKey(ind);
            BooleanValue v = core.getIndicators().get(key);
            return v != null && v.getValue();
        } catch (RuntimeException e) {
            throw new IllegalStateException("indicator *IN" + ind + " is not supported on O specs yet");
        }
    }

    private static void setIndicator(InterpreterCore core, String ind, boolean on) {
        core.getIndicators().put(IndicatorsKt.toIndicatorKey(ind), new BooleanValue(on));
    }

    private static boolean condTrue(InterpreterCore core, List<List<Cond>> groups) {
        boolean any = false;
        for (List<Cond> g : groups) {
            boolean all = true;
            for (Cond c : g) {
                if (indicator(core, c.ind) == c.not) {
                    all = false;
                    break;
                }
            }
            if (all) return true;
            any = true;
        }
        return !any;
    }

    /** EXCEPT name ("*NONE": the E records without an EXCEPT name). */
    public static synchronized void except(String name, InterpreterCore core, BridgeClient bridge) {
        String want = name.equals("*NONE") ? "" : name;
        for (Rec r : RECORDS) {
            if (r.type != 'E' || !r.exceptName.equals(want)) continue;
            if (!condTrue(core, r.cond)) continue;
            print(r, core, bridge);
        }
    }

    private static void newPage(Printer p) {
        p.page++;
        p.line = 1;
        p.printedOnPage = false;
    }

    private static void print(Rec r, InterpreterCore core, BridgeClient bridge) {
        Printer p = PRINTERS.get(r.file);
        if (!p.open) {
            p.spl = bridge.spoolOpen(p.file, program);
            p.open = true;
        }
        // Before: skip, then space.
        if (r.skipBefore > 0) {
            if (r.skipBefore < p.line || (r.skipBefore == p.line && p.printedOnPage)) newPage(p);
            p.line = r.skipBefore;
        }
        if (r.spaceBefore > 0) p.line += r.spaceBefore;
        if (p.line > PAGE_LENGTH) newPage(p);

        char[] buf = new char[p.length];
        java.util.Arrays.fill(buf, ' ');
        for (Field f : r.fields) {
            if (!condTrue(core, f.cond)) continue;
            String text = f.constant != null ? f.constant : value(f, core, p);
            int start = f.end - text.length();
            for (int i = 0; i < text.length(); i++) {
                int at = start + i;
                if (at >= 0 && at < buf.length) buf[at] = text.charAt(i);
            }
            if (f.blankAfter && f.name != null) blank(f.name, core);
        }
        p.buffer.add(new Object[] {p.page, p.line, new String(buf).replaceAll("\\s+$", "")});
        p.printedOnPage = true;
        int printedPage = p.page;
        int printedLine = p.line;

        // After: skip, then space; nothing given means space 1.
        boolean moved = false;
        if (r.skipAfter > 0) {
            if (r.skipAfter <= p.line) newPage(p);
            p.line = r.skipAfter;
            moved = true;
        }
        if (r.spaceAfter >= 0) {
            p.line += r.spaceAfter;
            moved = true;
        }
        if (!moved && r.skipBefore < 0 && r.spaceBefore < 0) p.line += 1;
        // Overflow: the overflow line was printed on, or spaced past.
        boolean overflow = printedLine >= OVERFLOW_LINE
                || (p.page == printedPage && p.line > OVERFLOW_LINE);
        if (p.line > PAGE_LENGTH) newPage(p);
        if (overflow && p.overflowInd != null) setIndicator(core, p.overflowInd, true);
        if (p.buffer.size() >= 50) flush(p, bridge);
    }

    private static void flush(Printer p, BridgeClient bridge) {
        if (p.buffer.isEmpty()) return;
        bridge.spoolWrite(p.spl, new ArrayList<>(p.buffer));
        p.buffer.clear();
    }

    /** The program ends: close every spooled file it opened. */
    public static synchronized void closeAll(BridgeClient bridge) {
        for (Printer p : PRINTERS.values()) {
            if (!p.open) continue;
            flush(p, bridge);
            bridge.spoolClose(p.spl);
            p.open = false;
        }
    }

    private static void blank(String name, InterpreterCore core) {
        AbstractDataDefinition def = core.getGlobalSymbolTable().dataDefinitionByName(name);
        if (def == null) return;
        core.assign(def, def.getType() instanceof NumberType
                ? new DecimalValue(BigDecimal.ZERO)
                : new StringValue("", false));
    }

    /** A field's printed text: edited numbers, character fields as they are. */
    private static String value(Field f, InterpreterCore core, Printer p) {
        LocalDate today = LocalDate.now();
        BigDecimal n;
        int len;
        int dec;
        switch (f.name) {
            case "PAGE": case "PAGE1": case "PAGE2": case "PAGE3":
            case "PAGE4": case "PAGE5": case "PAGE6": case "PAGE7":
                n = BigDecimal.valueOf(p.page);
                len = 4;
                dec = 0;
                break;
            case "UDATE":
                n = new BigDecimal(String.format("%02d%02d%02d",
                        today.getMonthValue(), today.getDayOfMonth(), today.getYear() % 100));
                len = 6;
                dec = 0;
                break;
            case "*DATE":
                n = new BigDecimal(String.format("%02d%02d%04d",
                        today.getMonthValue(), today.getDayOfMonth(), today.getYear()));
                len = 8;
                dec = 0;
                break;
            default:
                AbstractDataDefinition def = core.getGlobalSymbolTable().dataDefinitionByName(f.name);
                if (def == null) throw new IllegalStateException("O spec field " + f.name + " is not defined");
                Type t = def.getType();
                Value v = core.get(def);
                if (!(t instanceof NumberType)) return v.asString().getValue();
                NumberType nt = (NumberType) t;
                dec = nt.getDecimalDigits();
                len = nt.getEntireDigits() + dec;
                n = v instanceof NumberValue ? ((NumberValue) v).getBigDecimal() : new BigDecimal(v.asString().getValue().trim());
        }
        return edit(n, len, dec, f.editCode);
    }

    /**
     * Edit a number as IBM's edit codes do: 1-4 (no sign), J-M (minus sign
     * after), with commas for 1, 2, J, K; zero shown blank for 2, 4, K, M;
     * Z suppresses leading zeros and drops sign and decimal point; Y edits a
     * date (nn/nn/nn). No edit code: all digits, no decimal point, and a
     * minus sign after a negative number.
     */
    static String edit(BigDecimal value, int len, int dec, char code) {
        BigDecimal v = value.setScale(dec, RoundingMode.DOWN);
        boolean neg = v.signum() < 0;
        String digits = v.abs().unscaledValue().toString();
        while (digits.length() < len) digits = "0" + digits;
        if (code == 'Y') {
            // Date edit: nn/nn/nn (nn/nn/nnnn for 8 digits); the leftmost
            // zero is suppressed.
            String d = digits.substring(digits.length() - len);
            String out = len == 6 || len == 8
                    ? d.substring(0, 2) + "/" + d.substring(2, 4) + "/" + d.substring(4)
                    : d;
            return out.startsWith("0") ? " " + out.substring(1) : out;
        }
        String intPart = digits.substring(0, len - dec).replaceFirst("^0+", "");
        String frac = digits.substring(len - dec);
        boolean zero = v.signum() == 0;
        boolean commas, zeroBlank, sign, point;
        switch (code) {
            case '1': commas = true; zeroBlank = false; sign = false; point = true; break;
            case '2': commas = true; zeroBlank = true; sign = false; point = true; break;
            case '3': commas = false; zeroBlank = false; sign = false; point = true; break;
            case '4': commas = false; zeroBlank = true; sign = false; point = true; break;
            case 'J': commas = true; zeroBlank = false; sign = true; point = true; break;
            case 'K': commas = true; zeroBlank = true; sign = true; point = true; break;
            case 'L': commas = false; zeroBlank = false; sign = true; point = true; break;
            case 'M': commas = false; zeroBlank = true; sign = true; point = true; break;
            case 'Z': commas = false; zeroBlank = true; sign = false; point = false; break;
            default:
                return digits.substring(digits.length() - len) + (neg ? "-" : "");
        }
        int width = len + (commas && len - dec > 0 ? (len - dec - 1) / 3 : 0)
                + (point && dec > 0 ? 1 : 0) + (sign ? 1 : 0);
        if (zero && zeroBlank) return " ".repeat(width);
        StringBuilder ip = new StringBuilder();
        for (int i = 0; i < intPart.length(); i++) {
            if (commas && i > 0 && (intPart.length() - i) % 3 == 0) ip.append(',');
            ip.append(intPart.charAt(i));
        }
        String s = ip.toString();
        if (dec > 0) s += (point ? "." : "") + frac;
        else if (s.isEmpty()) s = "0";
        if (s.isEmpty()) s = "0";
        if (sign) s += neg ? "-" : " ";
        while (s.length() < width) s = " " + s;
        return s;
    }
}
