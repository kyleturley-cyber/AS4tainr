package io.openeye.bridge;

import java.math.BigDecimal;
import java.util.Arrays;
import java.util.Collections;
import java.util.List;
import java.util.Map;

/**
 * A stand-in runner for `openeye call`, with no dependencies beyond the JDK.
 *
 * It is launched exactly like the Jariko runner (`--bridge HOST:PORT PGM args...`)
 * and replays, in Java, the file operations of the sample RPG programs in
 * samples/qrpglesrc. Its output must match samples/expected/*.txt, the same
 * golden files the real Jariko runner is checked against. That way the Rust
 * core, the bridge protocol and the Java client are tested end to end even
 * where Jariko cannot be built.
 *
 * `SELFTEST` runs protocol checks and exits non-zero on the first failure.
 */
public final class BridgeConformance {

    public static void main(String[] args) throws Exception {
        if (args.length < 3 || !args[0].equals("--bridge")) {
            System.err.println("usage: BridgeConformance --bridge HOST:PORT PGM [args...]");
            System.exit(2);
        }
        String pgm = args[2].toUpperCase();
        List<String> parms = Arrays.asList(args).subList(3, args.length);
        try (BridgeClient c = BridgeClient.connect(args[1])) {
            switch (pgm) {
                case "CUSTLIST": custlist(c, parms); break;
                case "ORDTOTAL": ordtotal(c, parms); break;
                case "SELFTEST": selftest(c); break;
                default:
                    System.err.println("CPF4101: program " + pgm + " not found");
                    System.exit(1);
            }
        }
    }

    private static String trimEnd(String s) {
        int e = s.length();
        while (e > 0 && s.charAt(e - 1) == ' ') e--;
        return s.substring(0, e);
    }

    private static void dsply(String msg) {
        System.out.println(msg);
    }

    /** Mirrors samples/qrpglesrc/CUSTLIST.rpgle. */
    static void custlist(BridgeClient c, List<String> parms) {
        int f = c.open("CUSTMAST");
        ReadResult r = c.read(f);
        while (!r.eof) {
            dsply(new BigDecimal(r.record.get("CUSNO")).toPlainString() + " " + r.record.get("CUSNAM").trim());
            r = c.read(f);
        }
        r = c.chain(f, Collections.singletonList(parms.isEmpty() ? "0" : parms.get(0)));
        dsply(r.found
                ? "Found " + r.record.get("CUSNAM").trim() + " in " + r.record.get("CUSCTY").trim()
                : "Customer not found");
        c.closeFile(f);
    }

    /** Mirrors samples/qrpglesrc/ORDTOTAL.rpgle. */
    static void ordtotal(BridgeClient c, List<String> parms) {
        String ord = parms.isEmpty() ? "0" : parms.get(0);
        List<String> key = Collections.singletonList(ord);
        int f = c.open("ORDLIN");
        c.setll(f, key);
        BigDecimal total = new BigDecimal("0.00");
        int lines = 0;
        ReadResult r = c.readEqual(f, key);
        while (!r.eof) {
            total = total.add(new BigDecimal(r.record.get("QTY")).multiply(new BigDecimal(r.record.get("PRICE"))))
                    .setScale(2);
            lines++;
            r = c.readEqual(f, key);
        }
        dsply("Order " + new BigDecimal(ord).toPlainString() + ": " + lines + " lines, total " + total.toPlainString());
        c.closeFile(f);
    }

    private static void check(boolean cond, String what) {
        if (!cond) {
            System.err.println("FAIL: " + what);
            System.exit(1);
        }
        System.out.println("ok   " + what);
    }

    static void selftest(BridgeClient c) {
        Map<String, Object> md = c.metadata("CUSTMAST");
        check("CUSTREC".equals(md.get("format")), "metadata reports the record format");
        check(Arrays.asList("CUSNO").equals(md.get("keys")), "metadata reports the key fields");

        int f = c.open("CUSTMAST");
        ReadResult r = c.chain(f, Collections.singletonList("1"));
        check(r.found && !r.eof, "CHAIN finds customer 1 and leaves %EOF off");
        check(r.record.get("CUSNAM").length() == 30, "character fields keep their fixed length");
        check("1250.50".equals(r.record.get("CUSBAL")), "packed fields keep their scale");
        check(r.rrn != null, "reads return the RRN");

        check(!c.chain(f, Collections.singletonList("999")).found, "CHAIN on a missing key sets %FOUND off");

        try {
            c.update(f, Collections.singletonMap("CUSBAL", "1.00"));
            check(false, "UPDATE without a current record is rejected");
        } catch (BridgeException e) {
            check("CPF5013".equals(e.messageId()), "UPDATE without a current record fails with CPF5013");
        }

        try {
            c.write(f, Collections.singletonMap("CUSNO", "1"));
            check(false, "duplicate key is rejected");
        } catch (BridgeException e) {
            check("CPF5026".equals(e.messageId()), "WRITE of a duplicate key fails with CPF5026");
        }

        int o = c.open("ORDLIN");
        check(c.setll(o, Collections.singletonList("1002")), "SETLL sets %EQUAL when the key exists");
        int n = 0;
        while (!c.readEqual(o, null).eof) n++;
        check(n == 3, "READE without a key reads the 3 lines of order 1002");
        c.closeFile(o);
        c.closeFile(f);
        System.out.println("SELFTEST passed");
    }
}
