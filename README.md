# ibmish

**An ibm-ish experience for open source devs.**

ibmish is an open, IBM i-compatible runtime that runs in a container.

It runs IBM i business applications (RPG, CL, DDS files) on ordinary Linux and cloud hosts. It does not emulate POWER hardware or boot IBM's operating system. It reimplements the interfaces that programs and people use, the same way Wine runs Windows programs: libraries and objects, database files with record-level access, 5250 green screens, and CL commands. RPG runs on [Jariko](https://github.com/smeup/jariko), an open-source RPG interpreter for the JVM.

> Status: **early prototype**. Sign on with a real 5250 emulator, run CL commands, and `CALL` CL and RPG programs, all against PostgreSQL. That includes **interactive programs that use DDS display files** (`EXFMT`, and subfiles: the scrolling lists behind "Work with..." screens), DDS physical and logical files, SQL, and data areas. The Phase 2 gate is met: a sample **order entry application** (CL driver, RPG with an input subfile, five files, a data area) runs unmodified. See [docs/order-entry.md](docs/order-entry.md), [docs/5250-server.md](docs/5250-server.md) and [docs/display-files.md](docs/display-files.md). Phase 3 has started: jobs, `SBMJOB` with a QBATCH subsystem, job logs and message queues ([docs/work-management.md](docs/work-management.md)). Two users changing the same record are kept apart with IBM i-style record locks ([docs/record-locks.md](docs/record-locks.md)). Printed reports become spooled files (`WRKSPLF`, [docs/spooled-files.md](docs/spooled-files.md)), and objects have owners and authorities ([docs/object-authority.md](docs/object-authority.md)). The Phase 3 gate is met: three clerks enter orders at once while batch jobs print, and the files add up (`tools/gate3_5250.py`).

## How it fits together

```
 ibmish call PGM ──► Rust core ──(starts)──► JVM: Jariko runs PGM.rpgle
                         ▲                         │
                         └── bridge: JSON lines ◄──┘  every file operation
                         │
                         ▼
                    PostgreSQL  (catalog + one table per physical file)
```

- **`core/`**: the Rust core (`ibmish` binary). It holds the object catalog, the DDS parser, record-level access over PostgreSQL, and the bridge server.
- **`jvm/`**: the JVM side. `io.ibmish.bridge` is a dependency-free Java bridge client. `io.ibmish.jvm` plugs that client into Jariko as a reload `DBMManager` and provides the runner (`ibmish-jvm.jar`).
- **`samples/`**: DDS sources, CSV data, RPG programs and their expected (golden) output.
- **`docs/`**: [5250 server](docs/5250-server.md), [browser terminal](docs/web-terminal.md), [display files](docs/display-files.md), [logical files](docs/logical-files.md), [SQL](docs/sql.md), [CL programs](docs/cl-programs.md), [data areas](docs/data-areas.md), [work management](docs/work-management.md), [record locks](docs/record-locks.md), [spooled files](docs/spooled-files.md), [object authority](docs/object-authority.md), [menus](docs/menus.md), [order entry sample](docs/order-entry.md), [bridge protocol](docs/bridge-protocol.md), [known fidelity gaps](docs/fidelity-gaps.md), [spike 01 report](docs/spike-01.md).

## Quick start (Docker)

```sh
docker compose up --build
```

This starts PostgreSQL and the ibmish 5250 server, with the sample files (`CUSTMAST`, `ITEMMAST`, `ORDHDR`, `ORDLIN`), display files and CL programs in `QGPL` (`scripts/samples.sh`). Open **http://localhost:5250** for a 5250 terminal in your browser, with clickable function keys ([docs/web-terminal.md](docs/web-terminal.md)), or connect a 5250 emulator to **localhost:2323** (tn5250, [tn5250j](https://github.com/tn5250j/tn5250j), or IBM ACS). Sign on as `QSECOFR` with password `QSECOFR`. You will be asked to choose a new password. You land on the main menu ([docs/menus.md](docs/menus.md)): pick an option, or type commands on its command line (option 7 or `CALL QCMD` opens Command Entry). Then try:

```
CRTLIB MYLIB TEXT('My first library')
DSPLIB QSYS
CALL CUSTLIST PARM('3')
CALL CUSTINQ
CALL WRKCUST
CALL CUSTRPT PARM(L)
CALL ORDENTC
DSPDTAARA ORDNXT
CALL CUSTMNT
SBMJOB CMD(CALL PGM(CUSTRPT) PARM(L)) JOB(RPT)
SBMJOB CMD(CALL CUSTPRT)
WRKSPLF
WRKSBMJOB
WRKACTJOB
WRKOBJ QGPL/*ALL
DSPPFM CUSTMAST
STRSQL
SIGNOFF
```

Type a command name and press **F4** to prompt it, or press F4 on an empty line for a numbered menu of commands. QSECOFR can create more users with `CRTUSRPRF`.

`docker compose run --rm demo` runs the sample programs and checks their output against `samples/expected/`.

## Building from source

Requirements: Rust (stable), JDK 17, and PostgreSQL 16 for the integration tests.

```sh
# Rust core
cargo build
IBMISH_TEST_DATABASE_URL=postgres://ibmish:ibmish@127.0.0.1/ibmish cargo test

# JVM runner (downloads Jariko from Maven Central)
cd jvm && ./gradlew fatJar        # -> jvm/build/libs/ibmish-jvm.jar
```

Run the samples against a local database:

```sh
export IBMISH_DATABASE_URL=postgres://ibmish:ibmish@127.0.0.1/ibmish
export IBMISH_BIN=target/debug/ibmish
export IBMISH_RUNNER="java -jar jvm/build/libs/ibmish-jvm.jar"
scripts/demo.sh
```

### Without Maven access

The bridge can be tested end to end with just a JDK. `BridgeConformance` is a stand-in runner that replays the sample programs' file operations in Java:

```sh
javac -d target/conformance-classes jvm/src/main/java/io/ibmish/bridge/*.java jvm/src/test/java/io/ibmish/bridge/*.java
IBMISH_RUNNER="java -cp target/conformance-classes io.ibmish.bridge.BridgeConformance" scripts/demo.sh
target/debug/ibmish call SELFTEST --runner "java -cp target/conformance-classes io.ibmish.bridge.BridgeConformance"
```

## CLI

| Command | IBM i equivalent | What it does |
| --- | --- | --- |
| `ibmish init` | | Creates the catalog and the `QSYS`, `QGPL` and `QTEMP` libraries |
| `ibmish crtlib LIB` | `CRTLIB` | Creates a library (a PostgreSQL schema) |
| `ibmish system "COMMAND"` | `system` (QSH) | Runs one CL command as QSECOFR, e.g. `ibmish system "CRTDTAARA DTAARA(QGPL/X) TYPE(*DEC)"` |
| `ibmish crtpf LIB/FILE --src x.pf` | `CRTPF` | Creates a physical file from DDS |
| `ibmish crtbndcl LIB/PGM --src x.clle` | `CRTBNDCL` | Creates a CL program ([docs/cl-programs.md](docs/cl-programs.md)); `CALL` runs it |
| `ibmish sql "SELECT ..."` | `RUNSQL` / `STRSQL` | Runs one SQL statement in the DB2 for i dialect ([docs/sql.md](docs/sql.md)) |
| `ibmish crtlf LIB/FILE --src x.lf` | `CRTLF` | Creates a logical file over a physical file from DDS ([docs/logical-files.md](docs/logical-files.md)) |
| `ibmish crtdspf LIB/FILE --src x.dspf` | `CRTDSPF` | Creates a display file from DDS |
| `ibmish load FILE --csv x.csv` | `CPYFRMIMPF` (roughly) | Writes CSV rows with `WRITE` |
| `ibmish call PGM [parms]` | `CALL` | Runs an RPG program through the runner |
| `ibmish serve --port 7250` | | Runs a standalone bridge server |
| `ibmish tn5250 --listen 0.0.0.0:23` | | Runs the 5250 display server (sign-on, menus, command entry) and the QBATCH subsystem for submitted jobs. `--web-listen 0.0.0.0:8080` also serves the browser 5250 terminal |

Settings: `IBMISH_DATABASE_URL`, `IBMISH_QSECOFR_PASSWORD` (initial QSECOFR password), `IBMISH_SYSTEM_NAME`, `IBMISH_LIBL` (library list, comma-separated, default `QGPL`), `IBMISH_RUNNER`, `IBMISH_PGM_PATH`.

## Roadmap

| Phase | Scope | Exit gate |
| --- | --- | --- |
| 0 · Foundations | Object store, EBCDIC/decimal library, TN5250 sign-on, CL shell | Sign on with tn5250, run `CRTLIB` and `DSPLIB` |
| 1 · Data | DDS physical and logical files, record-level access, SQL | `CRTPF` from DDS, read it with RLA and SQL |
| 2 · Programs (v1) | CL compiler, RPG IV on Jariko, display files | A sample order-entry app runs unmodified (met: [docs/order-entry.md](docs/order-entry.md)) |
| 3 · Operations | Jobs, subsystems, spool, security | Multi-user interactive and batch workload (met: `tools/gate3_5250.py`) |
| 4 · Ecosystem | RPG/400, COBOL, host servers, migration tools | ACS and ODBC tools connect |

Spike 01 brings forward part of phase 1 (record-level access) and phase 2 (Jariko integration), because these were the riskiest assumptions.

## Contributing

Contributions are welcome. Please read [CONTRIBUTING.md](CONTRIBUTING.md) first, especially the **clean-room rules**: ibmish is built only from public documentation and observed behavior, never from IBM code. Every commit needs a DCO sign-off (`git commit -s`).

## Legal

Apache License 2.0. IBM, IBM i, AS/400, iSeries and DB2 are trademarks of IBM. ibmish is an independent project, not affiliated with or endorsed by IBM. See [NOTICE](NOTICE).
