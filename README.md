# OpenEye

**An open, IBM i-compatible runtime that runs in a container.**

OpenEye runs IBM i business applications (RPG, CL, DDS files) on ordinary Linux and cloud hosts. It does not emulate POWER hardware or boot IBM's operating system. It reimplements the interfaces that programs and people use, the same way Wine runs Windows programs: libraries and objects, database files with record-level access, 5250 green screens, and CL commands. RPG runs on [Jariko](https://github.com/smeup/jariko), an open-source RPG interpreter for the JVM.

> Status: **early prototype (spike 01)**. OpenEye can create DDS physical files, load them, and run RPG programs whose file I/O (`SETLL`, `CHAIN`, `READ`, `READE`, `READP`, `READPE`, `WRITE`, `UPDATE`, `DELETE`) goes through the Rust core into PostgreSQL. There are no 5250 screens, CL, or jobs yet. See the roadmap below.

## How it fits together

```
 openeye call PGM ──► Rust core ──(starts)──► JVM: Jariko runs PGM.rpgle
                         ▲                         │
                         └── bridge: JSON lines ◄──┘  every file operation
                         │
                         ▼
                    PostgreSQL  (catalog + one table per physical file)
```

- **`core/`**: the Rust core (`openeye` binary). It holds the object catalog, the DDS parser, record-level access over PostgreSQL, and the bridge server.
- **`jvm/`**: the JVM side. `io.openeye.bridge` is a dependency-free Java bridge client. `io.openeye.jvm` plugs that client into Jariko as a reload `DBMManager` and provides the runner (`openeye-jvm.jar`).
- **`samples/`**: DDS sources, CSV data, RPG programs and their expected (golden) output.
- **`docs/`**: [bridge protocol](docs/bridge-protocol.md), [known fidelity gaps](docs/fidelity-gaps.md), [spike 01 report](docs/spike-01.md).

## Quick start (Docker)

```sh
docker compose up --build --abort-on-container-exit
```

This starts PostgreSQL, creates `QGPL/CUSTMAST` and `QGPL/ORDLIN` from DDS, loads the sample data, and runs `CUSTLIST` and `ORDTOTAL` on Jariko. It then compares their output with `samples/expected/`.

## Building from source

Requirements: Rust (stable), JDK 17, and PostgreSQL 16 for the integration tests.

```sh
# Rust core
cargo build
OPENEYE_TEST_DATABASE_URL=postgres://openeye:openeye@127.0.0.1/openeye cargo test

# JVM runner (downloads Jariko from Maven Central)
cd jvm && ./gradlew fatJar        # -> jvm/build/libs/openeye-jvm.jar
```

Run the samples against a local database:

```sh
export OPENEYE_DATABASE_URL=postgres://openeye:openeye@127.0.0.1/openeye
export OPENEYE_BIN=target/debug/openeye
export OPENEYE_RUNNER="java -jar jvm/build/libs/openeye-jvm.jar"
scripts/demo.sh
```

### Without Maven access

The bridge can be tested end to end with just a JDK. `BridgeConformance` is a stand-in runner that replays the sample programs' file operations in Java:

```sh
javac -d target/conformance-classes jvm/src/main/java/io/openeye/bridge/*.java jvm/src/test/java/io/openeye/bridge/*.java
OPENEYE_RUNNER="java -cp target/conformance-classes io.openeye.bridge.BridgeConformance" scripts/demo.sh
target/debug/openeye call SELFTEST --runner "java -cp target/conformance-classes io.openeye.bridge.BridgeConformance"
```

## CLI

| Command | IBM i equivalent | What it does |
| --- | --- | --- |
| `openeye init` | | Creates the catalog and the `QSYS`, `QGPL` and `QTEMP` libraries |
| `openeye crtlib LIB` | `CRTLIB` | Creates a library (a PostgreSQL schema) |
| `openeye crtpf LIB/FILE --src x.pf` | `CRTPF` | Creates a physical file from DDS |
| `openeye load FILE --csv x.csv` | `CPYFRMIMPF` (roughly) | Writes CSV rows with `WRITE` |
| `openeye call PGM [parms]` | `CALL` | Runs an RPG program through the runner |
| `openeye serve --port 7250` | | Runs a standalone bridge server |

Settings: `OPENEYE_DATABASE_URL`, `OPENEYE_LIBL` (library list, comma-separated, default `QGPL`), `OPENEYE_RUNNER`, `OPENEYE_PGM_PATH`.

## Roadmap

| Phase | Scope | Exit gate |
| --- | --- | --- |
| 0 · Foundations | Object store, EBCDIC/decimal library, TN5250 sign-on, CL shell | Sign on with tn5250, run `CRTLIB` and `DSPLIB` |
| 1 · Data | DDS physical and logical files, record-level access, SQL | `CRTPF` from DDS, read it with RLA and SQL |
| 2 · Programs (v1) | CL compiler, RPG IV on Jariko, display files | A sample order-entry app runs unmodified |
| 3 · Operations | Jobs, subsystems, spool, security | Multi-user interactive and batch workload |
| 4 · Ecosystem | RPG/400, COBOL, host servers, migration tools | ACS and ODBC tools connect |

Spike 01 brings forward part of phase 1 (record-level access) and phase 2 (Jariko integration), because these were the riskiest assumptions.

## Contributing

Contributions are welcome. Please read [CONTRIBUTING.md](CONTRIBUTING.md) first, especially the **clean-room rules**: OpenEye is built only from public documentation and observed behavior, never from IBM code. Every commit needs a DCO sign-off (`git commit -s`).

## Legal

Apache License 2.0. IBM, IBM i, AS/400, iSeries and DB2 are trademarks of IBM. OpenEye is an independent project, not affiliated with or endorsed by IBM. See [NOTICE](NOTICE).
