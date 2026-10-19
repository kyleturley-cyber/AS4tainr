package io.as4tainr.jvm;

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
import com.smeup.rpgparser.interpreter.FeatureFlag;
import com.smeup.rpgparser.interpreter.FileMetadata;
import com.smeup.rpgparser.interpreter.NumberType;
import com.smeup.rpgparser.interpreter.NumberValue;
import com.smeup.rpgparser.interpreter.OnExfmtResponse;
import com.smeup.rpgparser.interpreter.RuntimeInterpreterSnapshot;
import com.smeup.rpgparser.interpreter.StringType;
import com.smeup.rpgparser.interpreter.StringValue;
import com.smeup.rpgparser.interpreter.Type;
import com.smeup.rpgparser.interpreter.Value;
import com.smeup.rpgparser.interpreter.RpgProgram;
import com.smeup.rpgparser.parsing.ast.Api;
import com.smeup.rpgparser.parsing.ast.ApiDescriptor;
import com.smeup.rpgparser.parsing.ast.ApiId;
import com.smeup.rpgparser.parsing.ast.SourceProgram;
import com.smeup.rpgparser.parsing.facade.Copy;
import com.smeup.rpgparser.parsing.facade.CopyId;
import com.smeup.rpgparser.parsing.parsetreetoast.RpgType;
import com.smeup.rpgparser.rpginterop.DirRpgProgramFinder;
import com.smeup.rpgparser.rpginterop.RpgProgramFinder;
import io.as4tainr.bridge.BridgeClient;

import java.io.BufferedReader;
import java.io.ByteArrayInputStream;
import java.io.File;
import java.io.IOException;
import java.io.StringReader;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.math.BigDecimal;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.HashMap;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

/**
 * The as4tainr RPG runner. as4tainr core launches it as:
 *
 * <pre>java -jar as4tainr-jvm.jar --bridge HOST:PORT PGM [parm...]</pre>
 *
 * Programs are RPG program objects (CRTBNDRPG), whose source comes from
 * as4tainr core through the bridge: the program called in AS4TAINR_PGM_LIB (when
 * set), programs it calls through the library list. Otherwise they are
 * looked up as PGM.rpgle in the directories listed in AS4TAINR_PGM_PATH
 * (path-separator separated; default ./qrpglesrc). /COPY members come from
 * source files the same way. Database
 * files and display files both come from as4tainr core through the bridge: file
 * I/O goes to PostgreSQL, and EXFMT shows the record on the job's 5250 display.
 */
public final class Runner {
    private Runner() {}

    public static void main(String[] args) {
        if (args.length < 3 || !args[0].equals("--bridge")) {
            System.err.println("usage: as4tainr-jvm --bridge HOST:PORT PGM [parm...]");
            System.exit(2);
        }
        String hostPort = args[1];
        String program = args[2].toUpperCase();
        List<String> parms = Arrays.asList(args).subList(3, args.length);

        BridgeClient bridge;
        try {
            bridge = BridgeClient.connect(hostPort);
        } catch (Exception e) {
            System.err.println("cannot reach as4tainr core at " + hostPort + ": " + e.getMessage());
            System.exit(1);
            return;
        }
        Displays displays = new Displays(bridge);
        Displays.current = displays;

        // Database files go to As4tainrDBMManager. Display record formats used as
        // files (see Workstation) get one entry each, added as programs load.
        List<ConnectionConfig> connections = new ArrayList<>();
        connections.add(new ConnectionConfig(
                "*", As4tainrDBMManager.URL_PREFIX + hostPort, "", "", null, As4tainrDBMManager.class.getName()));

        List<RpgProgramFinder> finders = new ArrayList<>();
        finders.add(new BridgeFinder(bridge, program, System.getenv().getOrDefault("AS4TAINR_PGM_LIB", ""),
                displays, connections, hostPort));
        String path = System.getenv().getOrDefault("AS4TAINR_PGM_PATH", "qrpglesrc");
        for (String dir : path.split(File.pathSeparator)) {
            if (new File(dir).isDirectory()) {
                finders.add(new PreprocessingFinder(new File(dir), displays, connections, hostPort));
            }
        }

        Configuration configuration = new Configuration();
        configuration.setReloadConfig(new ReloadConfig(
                new DBNativeAccessConfig(connections),
                dbFile -> Workstation.format(dbFile) != null
                        ? displays.formatMetadata(dbFile)
                        : fileMetadata(bridge.metadata(dbFile))));
        configuration.setDspfConfig(new DspfConfig(displays::metadata, displays::dspf));
        configuration.getJarikoCallback().setOnExfmt(displays::exfmt);
        // Jariko's CHAIN cache answers a repeated CHAIN from memory. On IBM i
        // every CHAIN reads the file: it sees other jobs' changes and makes
        // the record found the one UPDATE and DELETE work on. With the cache,
        // CHAIN A, CHAIN B, CHAIN A, UPDATE updates B. So it is off.
        configuration.getJarikoCallback().setFeatureFlagIsOn(
                flag -> flag != FeatureFlag.ChainCacheFlag && flag.getOn());
        DataAreas dataAreas = new DataAreas(bridge);
        configuration.getJarikoCallback().setReadDataArea(dataAreas::in);
        configuration.getJarikoCallback().setWriteDataArea((name, value) -> {
            if (name.equalsIgnoreCase(RecordLocks.SIGNAL_AREA)) {
                String v = value.trim();
                if (v.startsWith("X ")) {
                    // EXCEPT, put in by Printers.preprocess.
                    Printers.except(v.substring(2).trim(), Workstation.interpreter(), bridge);
                } else if (!RecordLocks.signal(v)) {
                    // Record lock signals put in by RecordLocks.preprocess.
                    dataAreas.unlock(v.split("\\s+")[1]);
                }
            } else {
                dataAreas.out(name);
            }
            return kotlin.Unit.INSTANCE;
        });

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
                // Spooled files the program printed are complete.
                Printers.closeAll(bridge);
            } catch (Exception e) {
                System.err.println("as4tainr: closing spooled files: " + e.getMessage());
            }
            try {
                bridge.close();
            } catch (Exception ignored) {
                // ending anyway
            }
        }
    }

    /** Map an as4tainr database file description to Jariko's typed FileMetadata. */
    @SuppressWarnings("unchecked")
    static FileMetadata fileMetadata(Map<String, Object> md) {
        List<DbField> fields = new ArrayList<>();
        for (Map<String, Object> f : (List<Map<String, Object>>) md.get("fields")) {
            int length = ((Number) f.get("length")).intValue();
            int decimals = ((Number) f.get("decimals")).intValue();
            Type type;
            switch (String.valueOf(f.get("type"))) {
                case "P": type = new NumberType(length - decimals, decimals, RpgType.PACKED); break;
                // RPG defines numeric input fields of externally described
                // files as packed in the program, whatever their type in the
                // file (ILE RPG reference, externally described files), so
                // one name can be zoned in one file and packed in another.
                case "S": type = new NumberType(length - decimals, decimals, RpgType.PACKED); break;
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

    /**
     * Data areas (*DTAARA DEFINE, IN, OUT) through the bridge. Jariko hands
     * over only the data area's name and expects text back; as4tainr sets the
     * program's field itself instead, so a numeric field gets the *DEC value
     * as a number (not as a byte image), and OUT sends a number as decimal
     * text.
     */
    static final class DataAreas {
        private final BridgeClient bridge;

        DataAreas(BridgeClient bridge) {
            this.bridge = bridge;
        }

        private static com.smeup.rpgparser.interpreter.InterpreterCore core() {
            return MainExecutionContext.INSTANCE.getProgramStack().peek().getIntepreterCore();
        }

        /** The program fields DEFINEd for this data area. */
        private static List<com.smeup.rpgparser.interpreter.AbstractDataDefinition> fields(String name) {
            com.smeup.rpgparser.interpreter.InterpreterCore core = core();
            List<com.smeup.rpgparser.interpreter.AbstractDataDefinition> out = new ArrayList<>();
            for (Map.Entry<String, String> e : core.getStatus().getDataAreas().entrySet()) {
                if (e.getValue().equalsIgnoreCase(name)) {
                    com.smeup.rpgparser.interpreter.AbstractDataDefinition d =
                            core.getGlobalSymbolTable().dataDefinitionByName(e.getKey());
                    if (d != null) out.add(d);
                }
            }
            if (out.isEmpty()) throw new IllegalStateException("no field is defined for data area " + name);
            return out;
        }

        String in(String name, Boolean lock) {
            Map<String, Object> d = bridge.dataAreaIn(name, Boolean.TRUE.equals(lock));
            String value = (String) d.get("value");
            com.smeup.rpgparser.interpreter.InterpreterCore core = core();
            for (com.smeup.rpgparser.interpreter.AbstractDataDefinition def : fields(name)) {
                Value v = def.getType() instanceof NumberType
                        ? new DecimalValue(new BigDecimal("*DEC".equals(d.get("type")) ? value : value.trim()))
                        : new StringValue(value, false);
                core.assign(def, v);
            }
            return null; // already assigned
        }

        /** UNLOCK of a data area field (releases IN *LOCK). */
        void unlock(String field) {
            String external = core().getStatus().getDataAreas().get(field.toUpperCase());
            if (external == null) {
                for (Map.Entry<String, String> e : core().getStatus().getDataAreas().entrySet()) {
                    if (e.getKey().equalsIgnoreCase(field)) external = e.getValue();
                }
            }
            if (external == null) throw new IllegalStateException("UNLOCK " + field + ": not an open file or a data area");
            bridge.dataAreaUnlock(external);
        }

        void out(String name) {
            com.smeup.rpgparser.interpreter.InterpreterCore core = core();
            com.smeup.rpgparser.interpreter.AbstractDataDefinition def = fields(name).get(0);
            Value v = core.get(def);
            String text = v instanceof NumberValue
                    ? ((NumberValue) v).getBigDecimal().toPlainString()
                    : v.asString().getValue();
            bridge.dataAreaOut(name, text);
        }
    }

    /** Display files: their definitions for Jariko, and EXFMT through the bridge. */
    static final class Displays {
        /** The running program's displays (one runner process runs one call). */
        static Displays current;

        private final BridgeClient bridge;
        private final Map<String, DSPF> parsed = new HashMap<>();
        private final Map<String, String> fileOfFormat = new HashMap<>();

        Displays(BridgeClient bridge) {
            this.bridge = bridge;
        }

        BridgeClient bridge() {
            return bridge;
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
                    fields.putIfAbsent(f.getName(), new DbField(f.getName(), type(f)));
                }
            }
            return new FileMetadata(file.toUpperCase(), "", "", new ArrayList<>(fields.values()), List.of());
        }

        /** Jariko's type for a display field. */
        private static Type type(MutableField f) {
            int length = f.getLength() == null ? 10 : f.getLength();
            if (!f.isNumeric()) return new StringType(length, false);
            int decimals = f.getPrecision() == null ? 0 : f.getPrecision();
            // Packed, like most database files: RPG programs often share a
            // field name between a display file and a database file, and
            // Jariko rejects one name with two numeric types. Display I/O
            // always goes through decimal text, so the choice is safe.
            return new NumberType(length - decimals, decimals, RpgType.PACKED);
        }

        DSPFRecord record(String format) {
            Workstation.Format f = Workstation.format(format);
            if (f == null) throw new IllegalArgumentException(format + " is not a display record format");
            for (DSPFRecord r : dspf(f.file).getRecords()) {
                if (r.getName().equalsIgnoreCase(format)) return r;
            }
            throw new IllegalArgumentException("record format " + format + " not found in display file " + f.file);
        }

        /** One record format as a file (for WRITE/UPDATE/CHAIN/READC). */
        FileMetadata formatMetadata(String format) {
            String name = format.toUpperCase();
            List<DbField> fields = new ArrayList<>();
            for (MutableField f : record(name).getMutables()) {
                fields.add(new DbField(f.getName(), type(f)));
            }
            return new FileMetadata(name, name, name, fields, List.of());
        }

        /** The same, in reload's terms. */
        static com.smeup.dbnative.model.FileMetadata reloadMetadata(String format) {
            String name = format.toUpperCase();
            List<com.smeup.dbnative.model.Field> fields = new ArrayList<>();
            for (MutableField f : current.record(name).getMutables()) {
                fields.add(new com.smeup.dbnative.model.Field(f.getName(), "", f.isNumeric()));
            }
            return new com.smeup.dbnative.model.FileMetadata(name, name, fields, List.of());
        }

        List<String> formats(String file) {
            List<String> out = new ArrayList<>();
            for (DSPFRecord r : dspf(file).getRecords()) out.add(r.getName().toUpperCase());
            return out;
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
            Map<String, Object> resp = bridge.exfmt(file, format, values, Workstation.indicators());

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
            // Function key indicators *INKA-*INKY (F1-F24; there is no
            // *INKO): on for the key pressed, off for the others, at each
            // input. Jariko numbers them 105-128.
            String aid = String.valueOf(resp.get("aid"));
            for (int n = 1; n <= 24; n++) {
                indicators.put(104 + n, new BooleanValue(aid.equals("F" + n)));
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

    /**
     * An RPG program from its source: the source rewrites (display files,
     * printer files, record locks), then Jariko's parser.
     */
    static RpgProgram load(String name, String source, Displays displays, List<ConnectionConfig> connections,
            String hostPort) {
        String rewritten = RecordLocks.preprocess(Printers.preprocess(
                Workstation.preprocess(SourceForms.normalize(source,
                        path -> displays.bridge().sourceMember(null, null, path)), displays::formats), name));
        synchronized (connections) {
            for (Workstation.Format fmt : Workstation.formats()) {
                boolean known = connections.stream().anyMatch(c -> c.getFileName().equalsIgnoreCase(fmt.name));
                if (!known) {
                    // Before the "*" entry, so the format is not looked up as a database file.
                    connections.add(0, new ConnectionConfig(fmt.name, DisplayDBMManager.URL_PREFIX + hostPort,
                            "", "", null, DisplayDBMManager.class.getName()));
                }
            }
        }
        return RpgProgram.Companion.fromInputStream(
                new ByteArrayInputStream(rewritten.getBytes(StandardCharsets.UTF_8)),
                name.toUpperCase(), SourceProgram.RPGLE);
    }

    /**
     * Programs and /COPY members kept by as4tainr core (CRTBNDRPG, source
     * files), read through the bridge.
     */
    static final class BridgeFinder implements RpgProgramFinder {
        private final BridgeClient bridge;
        private final String first;
        private final String firstLibrary;
        private final Displays displays;
        private final List<ConnectionConfig> connections;
        private final String hostPort;

        BridgeFinder(BridgeClient bridge, String first, String firstLibrary, Displays displays,
                List<ConnectionConfig> connections, String hostPort) {
            this.bridge = bridge;
            this.first = first;
            this.firstLibrary = firstLibrary;
            this.displays = displays;
            this.connections = connections;
            this.hostPort = hostPort;
        }

        @Override
        public RpgProgram findRpgProgram(String nameOrSource) {
            String name = nameOrSource.trim().toUpperCase();
            if (name.isEmpty() || name.contains("\n")) return null;
            // The program called: the library core found it in. Programs it
            // calls: the library list.
            String library = name.equals(first) ? firstLibrary : "";
            String source = bridge.programSource(name, library);
            return source == null ? null : load(name, source, displays, connections, hostPort);
        }

        @Override
        public Copy findCopy(CopyId copyId) {
            String source = bridge.sourceMember(copyId.getLibrary(), copyId.getFile(), copyId.getMember());
            return source == null ? null : new Copy(source);
        }

        @Override
        public ApiDescriptor findApiDescriptor(ApiId apiId) {
            return null;
        }

        @Override
        public Api findApi(ApiId apiId) {
            return null;
        }
    }

    /**
     * Finds PGM.rpgle in a directory, like Jariko's DirRpgProgramFinder, and
     * runs the source through {@link Workstation#preprocess} first.
     */
    static final class PreprocessingFinder implements RpgProgramFinder {
        private final File dir;
        private final DirRpgProgramFinder plain;
        private final Displays displays;
        private final List<ConnectionConfig> connections;
        private final String hostPort;

        PreprocessingFinder(File dir, Displays displays, List<ConnectionConfig> connections, String hostPort) {
            this.dir = dir;
            this.plain = new DirRpgProgramFinder(dir);
            this.displays = displays;
            this.connections = connections;
            this.hostPort = hostPort;
        }

        @Override
        public RpgProgram findRpgProgram(String nameOrSource) {
            File f = new File(dir, nameOrSource + ".rpgle");
            if (!f.isFile()) f = new File(dir, nameOrSource.toUpperCase() + ".rpgle");
            if (!f.isFile()) f = new File(dir, nameOrSource.toLowerCase() + ".rpgle");
            if (!f.isFile()) return null;
            String source;
            try {
                source = new String(Files.readAllBytes(f.toPath()), StandardCharsets.UTF_8);
            } catch (IOException e) {
                throw new IllegalStateException("cannot read " + f + ": " + e.getMessage(), e);
            }
            return load(nameOrSource, source, displays, connections, hostPort);
        }

        @Override
        public Copy findCopy(CopyId copyId) {
            return plain.findCopy(copyId);
        }

        @Override
        public ApiDescriptor findApiDescriptor(ApiId apiId) {
            return plain.findApiDescriptor(apiId);
        }

        @Override
        public Api findApi(ApiId apiId) {
            return plain.findApi(apiId);
        }
    }
}
