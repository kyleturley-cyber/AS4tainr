package io.ibmish.jvm;

import com.smeup.dbnative.ConnectionConfig;
import com.smeup.dbnative.DBNativeAccessConfig;
import com.smeup.dspfparser.domain.DisplayFileParser;
import com.smeup.dspfparser.linesclassifier.DSPF;
import com.smeup.dspfparser.linesclassifier.DSPFRecord;
import com.smeup.dspfparser.linesclassifier.MutableField;
import com.smeup.rpgparser.execution.Configuration;
import com.smeup.rpgparser.execution.DspfConfig;
import com.smeup.rpgparser.execution.MainExecutionContext;
import com.smeup.rpgparser.execution.ReloadConfig;
import com.smeup.rpgparser.execution.RunnerKt;
import com.smeup.rpgparser.interpreter.BooleanValue;
import com.smeup.rpgparser.interpreter.DbField;
import com.smeup.rpgparser.interpreter.DecimalValue;
import com.smeup.rpgparser.interpreter.FileMetadata;
import com.smeup.rpgparser.interpreter.NumberType;
import com.smeup.rpgparser.interpreter.NumberValue;
import com.smeup.rpgparser.interpreter.OnExfmtResponse;
import com.smeup.rpgparser.interpreter.RuntimeInterpreterSnapshot;
import com.smeup.rpgparser.interpreter.StringType;
import com.smeup.rpgparser.interpreter.StringValue;
import com.smeup.rpgparser.interpreter.Type;
import com.smeup.rpgparser.interpreter.Value;
import com.smeup.rpgparser.parsing.parsetreetoast.RpgType;
import com.smeup.rpgparser.rpginterop.DirRpgProgramFinder;
import com.smeup.rpgparser.rpginterop.RpgProgramFinder;
import io.ibmish.bridge.BridgeClient;

import java.io.BufferedReader;
import java.io.File;
import java.io.StringReader;
import java.math.BigDecimal;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.HashMap;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

/**
 * The ibmish RPG runner. ibmish core launches it as:
 *
 * <pre>java -jar ibmish-jvm.jar --bridge HOST:PORT PGM [parm...]</pre>
 *
 * Programs are looked up as PGM.rpgle in the directories listed in
 * IBMISH_PGM_PATH (path-separator separated; default ./qrpglesrc). Database
 * files and display files both come from ibmish core through the bridge: file
 * I/O goes to PostgreSQL, and EXFMT shows the record on the job's 5250 display.
 */
public final class Runner {
    private Runner() {}

    public static void main(String[] args) {
        if (args.length < 3 || !args[0].equals("--bridge")) {
            System.err.println("usage: ibmish-jvm --bridge HOST:PORT PGM [parm...]");
            System.exit(2);
        }
        String hostPort = args[1];
        String program = args[2].toUpperCase();
        List<String> parms = Arrays.asList(args).subList(3, args.length);

        List<RpgProgramFinder> finders = new ArrayList<>();
        String path = System.getenv().getOrDefault("IBMISH_PGM_PATH", "qrpglesrc");
        for (String dir : path.split(File.pathSeparator)) {
            if (new File(dir).isDirectory()) finders.add(new DirRpgProgramFinder(new File(dir)));
        }
        if (finders.isEmpty()) {
            System.err.println("CPF4101: no program directory found (set IBMISH_PGM_PATH)");
            System.exit(1);
        }

        BridgeClient bridge;
        try {
            bridge = BridgeClient.connect(hostPort);
        } catch (Exception e) {
            System.err.println("cannot reach ibmish core at " + hostPort + ": " + e.getMessage());
            System.exit(1);
            return;
        }

        Configuration configuration = new Configuration();
        configuration.setReloadConfig(new ReloadConfig(
                new DBNativeAccessConfig(List.of(new ConnectionConfig(
                        "*", IbmishDBMManager.URL_PREFIX + hostPort, "", "", null, IbmishDBMManager.class.getName()))),
                dbFile -> fileMetadata(bridge.metadata(dbFile))));
        Displays displays = new Displays(bridge);
        configuration.setDspfConfig(new DspfConfig(displays::metadata, displays::dspf));
        configuration.getJarikoCallback().setOnExfmt(displays::exfmt);

        try {
            RunnerKt.executePgmWithStringArgs(program, parms, null, finders, configuration);
        } catch (Throwable e) {
            e.printStackTrace();
            Throwable root = e;
            while (root.getCause() != null) root = root.getCause();
            String why = root.getMessage() != null ? root.getMessage() : root.getClass().getSimpleName();
            System.err.println("RNQ0202: program " + program + " ended with an error: " + why);
            System.exit(1);
        } finally {
            try {
                bridge.close();
            } catch (Exception ignored) {
                // ending anyway
            }
        }
    }

    /** Map an ibmish database file description to Jariko's typed FileMetadata. */
    @SuppressWarnings("unchecked")
    static FileMetadata fileMetadata(Map<String, Object> md) {
        List<DbField> fields = new ArrayList<>();
        for (Map<String, Object> f : (List<Map<String, Object>>) md.get("fields")) {
            int length = ((Number) f.get("length")).intValue();
            int decimals = ((Number) f.get("decimals")).intValue();
            Type type;
            switch (String.valueOf(f.get("type"))) {
                case "P": type = new NumberType(length - decimals, decimals, RpgType.PACKED); break;
                case "S": type = new NumberType(length - decimals, decimals, RpgType.ZONED); break;
                default: type = new StringType(length, false);
            }
            fields.add(new DbField((String) f.get("name"), type));
        }
        return new FileMetadata(
                (String) md.get("name"),
                md.get("library") + "/" + md.get("name"),
                (String) md.get("format"),
                fields,
                (List<String>) md.get("keys"));
    }

    /** Display files: their definitions for Jariko, and EXFMT through the bridge. */
    static final class Displays {
        private final BridgeClient bridge;
        private final Map<String, DSPF> parsed = new HashMap<>();
        private final Map<String, String> fileOfFormat = new HashMap<>();

        Displays(BridgeClient bridge) {
            this.bridge = bridge;
        }

        DSPF dspf(String file) {
            return parsed.computeIfAbsent(file.toUpperCase(), name -> {
                String source = (String) bridge.displayFile(name).get("source");
                DSPF d = DisplayFileParser.Companion.parse(new BufferedReader(new StringReader(source)));
                for (DSPFRecord r : d.getRecords()) fileOfFormat.put(r.getName().toUpperCase(), name);
                return d;
            });
        }

        /** Field definitions of every record format, for the program's variables. */
        FileMetadata metadata(String file) {
            Map<String, DbField> fields = new LinkedHashMap<>();
            for (DSPFRecord r : dspf(file).getRecords()) {
                for (MutableField f : r.getMutables()) {
                    int length = f.getLength() == null ? 10 : f.getLength();
                    Type type;
                    if (f.isNumeric()) {
                        int decimals = f.getPrecision() == null ? 0 : f.getPrecision();
                        // Packed, like most database files: RPG programs often share a
                        // field name between a display file and a database file, and
                        // Jariko rejects one name with two numeric types. Display I/O
                        // always goes through decimal text, so the choice is safe.
                        type = new NumberType(length - decimals, decimals, RpgType.PACKED);
                    } else {
                        type = new StringType(length, false);
                    }
                    fields.putIfAbsent(f.getName(), new DbField(f.getName(), type));
                }
            }
            return new FileMetadata(file.toUpperCase(), "", "", new ArrayList<>(fields.values()), List.of());
        }

        @SuppressWarnings("unchecked")
        OnExfmtResponse exfmt(DSPFRecord record, RuntimeInterpreterSnapshot snapshot) {
            String format = record.getName().toUpperCase();
            String file = fileOfFormat.get(format);
            if (file == null) throw new IllegalStateException("record format " + format + " is not in a known display file");

            Map<String, String> values = new LinkedHashMap<>();
            for (MutableField f : record.getMutables()) values.put(f.getName(), text(f.getValue()));

            Map<Integer, BooleanValue> indicators = MainExecutionContext.INSTANCE.getProgramStack().peek()
                    .getIntepreterCore().getIndicators();
            StringBuilder ind = new StringBuilder(99);
            for (int i = 1; i <= 99; i++) {
                BooleanValue b = indicators.get(i);
                ind.append(b != null && b.getValue() ? '1' : '0');
            }

            Map<String, Object> resp = bridge.exfmt(file, format, values, ind.toString());

            Map<String, Value> newValues = new HashMap<>();
            Map<String, Object> fields = (Map<String, Object>) resp.get("fields");
            for (MutableField f : record.getMutables()) {
                Object v = fields.get(f.getName());
                if (v == null) continue;
                newValues.put(f.getName(), f.isNumeric()
                        ? new DecimalValue(new BigDecimal((String) v))
                        : new StringValue((String) v, false));
            }
            Map<String, Object> resInd = (Map<String, Object>) resp.get("indicators");
            for (Map.Entry<String, Object> e : resInd.entrySet()) {
                indicators.put(Integer.parseInt(e.getKey()), new BooleanValue(Boolean.TRUE.equals(e.getValue())));
            }
            return new OnExfmtResponse(snapshot, newValues);
        }

        private static String text(Object v) {
            if (v == null) return "";
            if (v instanceof NumberValue) return ((NumberValue) v).getBigDecimal().toPlainString();
            if (v instanceof Value) return ((Value) v).asString().getValue();
            return String.valueOf(v);
        }
    }
}
