# CLAUDE.md

Guidance for AI coding sessions working in this repository.

## What as4tainr is

An IBM i-compatible runtime for containers. It reimplements IBM i interfaces (objects and libraries, DDS files, record-level access, 5250, CL, jobs) on Linux and PostgreSQL. RPG runs on as4tainr's own RPG engine in Rust (`core/src/rpg/`, docs/rpg-engine.md). The Jariko runner on the JVM (`jvm/`, the bridge) is still available with `AS4TAINR_RPG_ENGINE=jariko` and is being retired. It is NOT a hardware emulator.

## Non-negotiable rules

1. **Clean room.** Implement only from public IBM documentation, RFCs, compatible open-source code, and documented observed behavior. Never reproduce IBM source code, IBM message texts or help texts, or code from memory of proprietary IBM software. Identifiers (command names, opcodes, message IDs like `CPF5026`) are fine. Write message texts fresh.
2. **No floating point for business values.** Packed and zoned decimals stay exact: they are strings at the boundaries and `numeric(p,s)` in PostgreSQL. Never parse them into `f64`/`double`.
3. **Fidelity changes need tests.** Every behavior change gets a test whose expected value comes from IBM documentation or a real system, not from running the new code. Say where the expected value came from in a comment.
4. **Document gaps.** If you implement something only partly, add it to `docs/fidelity-gaps.md`.
5. **Do not weaken tests to make them pass.** If a test looks wrong, explain why instead of editing it.

## Layout

| Path | What |
| --- | --- |
| `core/src/dds.rs` | DDS parser: physical files (R, K lines with `DESCEND`, fields, `UNIQUE`, `TEXT`) and logical files (`PFILE`, field subsets, keys, select/omit) |
| `core/src/catalog.rs` | Libraries = PostgreSQL schemas; file descriptions in schema `as4tainr`; library list resolution |
| `core/src/rla.rs` | Record-level access: cursor position per open file, keyset queries ordered by (keys, rrn), record locks (advisory locks on the physical file + rrn) |
| `core/src/sql.rs` | SQL in the DB2 for i dialect: translation to PostgreSQL, run on a separate connection as the restricted `as4tainr_sql` login |
| `core/src/bridge.rs` | JSON-lines TCP server the JVM calls for every file operation |
| `core/src/dspf.rs` | DDS display files: parser, drawing a record on a 5250 screen, input validation, response indicators |
| `core/src/workstn.rs` | A program's display state: records on the screen (OVERLAY), subfiles (WRITE/READC/CHAIN/UPDATE), one EXFMT's interaction and paging |
| `core/src/tn5250/` | 5250 display server: telnet + records (`telnet.rs`), data stream (`ds.rs`), EBCDIC (`ebcdic.rs`), screens (`session.rs`) |
| `core/src/web/` | Browser 5250 terminal: HTTP + WebSocket (`mod.rs`, `ws.rs`, dependency-free), a 5250 display station that talks TN5250 to this process's server (`display.rs`), the page (`static/`). See `docs/web-terminal.md` |
| `core/src/cl.rs` | CL parser and the command definition table (add new commands here first) |
| `core/src/commands.rs` | CL command implementations for a job |
| `core/src/clpgm.rs` | CL programs: parse/check (CRTBNDCL), expressions (exact decimals), run in the calling job with MONMSG |
| `core/src/dtaara.rs` | Data areas: CRTDTAARA/CHGDTAARA/RTVDTAARA values, RPG `IN *LOCK`/`OUT` (PostgreSQL advisory locks) |
| `core/src/jobs.rs` | Work management: jobs (`as4tainr.jobs`), job logs, job queue QBATCH and its subsystem loop, user message queues |
| `core/src/spool.rs` | Spooled files (`as4tainr.spooled_files`, `as4tainr.spool_lines`): printer output, QSYSPRT listings of batch display commands, QPJOBLOG |
| `core/src/source.rs` | Source physical files (`PF-SRC` objects) and members (`as4tainr.source_members`), stream files for includes (`as4tainr.stream_files`), CL `INCLUDE` expansion |
| `core/src/import.rs` | `as4tainr import DIR --lib LIB`: members (folders like QRPGLESRC/, TOBi names) and CSV data into a library, then CRTPF/CRTLF/CRTDSPF/CRTBNDCL/CRTBNDRPG. See `docs/import.md` |
| `core/src/rpg/` | The native RPG engine (docs/rpg-engine.md): `dec.rs` exact decimals, `data.rs` storage formats (EBCDIC, zoned, packed, binary), `expr.rs`, `parse.rs` (fixed, mixed, **FREE, directives), `program.rs` (names, layout, external files), `code.rs` (blocks → jumps), `interp.rs` (cycle, opcodes, BIFs, file I/O on `rla` in its own session), `mod.rs` (compile, call) |
| `core/src/menus.rs` | Menus: the system menus (MAIN...) as data, GO/CRTMNU TYPE(*PGM)/DLTMNU; the session shows them (`tn5250/session.rs`: initial program, initial menu, menu stack) |
| `core/src/authority.rs` | Object owners, *PUBLIC and private authorities; `check` before every object use (CPF9802) |
| `core/src/users.rs` | User profiles, Argon2id password hashes |
| `core/src/main.rs` | `as4tainr` CLI |
| `core/tests/rla_test.rs` | Integration tests (need `AS4TAINR_TEST_DATABASE_URL`) |
| `jvm/src/main/java/io/as4tainr/bridge` | Dependency-free Java bridge client (keep it dependency-free) |
| `jvm/src/main/java/io/as4tainr/jvm` | Runner (`Launcher`, `Runner`): Jariko configuration, display files, EXFMT. `Workstation` rewrites WORKSTN programs (display record formats as DISK files, READC); `DisplayDBMManager`/`DisplayRecordFile` send those operations to core. `RecordLocks` adds signals for update files, N/E extenders and UNLOCK (record locks). `Printers` reads PRINTER F specs and O specs and turns EXCEPT into a signal that prints to a spooled file. `SourceForms` blanks `//` lines in columns 1-5 and expands quoted `/INCLUDE` stream files. RPG program objects (CRTBNDRPG) come from the bridge (`pgmsrc`), before `AS4TAINR_PGM_PATH`. Java, so it compiles with plain javac against the jar |
| `jvm/src/main/kotlin/io/as4tainr/jvm` | reload `DBMManager`/`DBFile` adapter (database I/O through the bridge) |
| `jvm/src/test/java/.../BridgeConformance.java` | JDK-only stand-in runner used for end-to-end tests without Maven |
| `tools/tn5250_driver.py`, `tools/e2e_5250.py` | Drive the real tn5250 client for end-to-end tests |
| `tools/e2e_web.py` | Drives the browser terminal in Chromium (Playwright) |
| `tools/gate3_5250.py` | Phase 3 gate: three clerks enter orders at once with batch jobs running; the files must add up. Run after e2e_5250.py |
| `samples/` | DDS, CSV data, RPG and CL programs, golden outputs in `expected/`. `samples/apps/contacts` is an application laid out as a git repository, imported with `as4tainr import` (into CONTACTS by samples.sh). The order entry app (Phase 2 gate) is described in `docs/order-entry.md` |
| `scripts/samples.sh` | Creates every sample object in QGPL (used by demo.sh, the Docker entrypoint and CI). Add new sample objects here |
| `tools/corpus.sh` | Clone a public repository of IBM i source and import it (real-world test; never vendored) |
| `docs/bridge-protocol.md` | Protocol spec. Update it in the same change as the protocol |

## Commands

```sh
cargo fmt && cargo clippy --all-targets -- -D warnings
AS4TAINR_TEST_DATABASE_URL=postgres://as4tainr:as4tainr@127.0.0.1/as4tainr cargo test
javac -d target/conformance-classes jvm/src/main/java/io/as4tainr/bridge/*.java jvm/src/test/java/io/as4tainr/bridge/*.java
AS4TAINR_BIN=target/debug/as4tainr AS4TAINR_RUNNER="java -cp target/conformance-classes io.as4tainr.bridge.BridgeConformance" scripts/demo.sh
cd jvm && ./gradlew fatJar   # needs Maven Central
TN5250=/path/to/tn5250 python3 tools/e2e_5250.py 127.0.0.1:2323   # against a running `as4tainr tn5250`
python3 tools/e2e_web.py http://127.0.0.1:5250   # browser terminal in Chromium (pip install playwright), with --web-listen 127.0.0.1:5250
TN5250=/path/to/tn5250 AS4TAINR_BIN=target/debug/as4tainr python3 tools/gate3_5250.py 127.0.0.1:2323   # then the Phase 3 gate
```

## Conventions

- Errors that a program could monitor for start with the IBM-style message ID, e.g. `CPF5026: ...`.
- SQL is built dynamically only with `catalog::ident()` for identifiers. Values are always bound parameters (as text, then `CAST`). The exception is user SQL (`sql.rs`), which only ever runs as the `as4tainr_sql` login: never run user-supplied SQL on the main connection.
- Bridge protocol changes are versioned: bump `PROTOCOL_VERSION` in both `bridge.rs` and `BridgeClient.java`.
- Commits are signed off (`git commit -s`). Commit messages describe the IBM i behavior implemented.
