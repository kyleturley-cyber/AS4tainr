# CLAUDE.md

Guidance for AI coding sessions working in this repository.

## What as4tainr is

An IBM i-compatible runtime for containers. It reimplements IBM i interfaces (objects and libraries, DDS files, record-level access, 5250, CL, jobs) on Linux and PostgreSQL. RPG runs on as4tainr's own RPG compiler and runtime in Rust (`core/src/rpg/`, docs/rpg-engine.md); there is no JVM. It is NOT a hardware emulator.

## Non-negotiable rules

1. **Clean room.** Implement only from public IBM documentation, RFCs, compatible open-source code, and documented observed behavior. Never reproduce IBM source code, IBM message texts or help texts, or code from memory of proprietary IBM software. Identifiers (command names, opcodes, message IDs like `CPF5026`) are fine. Write message texts fresh.
2. **No floating point for business values.** Packed and zoned decimals stay exact: they are strings at the boundaries and `numeric(p,s)` in PostgreSQL. Never parse them into `f64`/`double`.
3. **Fidelity changes need tests.** Every behavior change gets a test whose expected value comes from IBM documentation or a real system, not from running the new code. Say where the expected value came from in a comment.
4. **Document gaps.** If you implement something only partly, add it to `docs/fidelity-gaps.md`.
5. **Do not weaken tests to make them pass.** If a test looks wrong, explain why instead of editing it.

## Layout

| Path | What |
| --- | --- |
| `core/src/dds.rs` | DDS parser: physical files (R, K lines with `DESCEND`, fields A/P/S/B/L/T/Z, `UNIQUE`, `TEXT`, `VARLEN`, `ALWNULL`, `DFT`) and logical files (`PFILE`, field subsets, keys, select/omit); a field's PostgreSQL type, default and external text form |
| `core/src/ddl.rs` | SQL data definition (CREATE TABLE/INDEX/VIEW, ALTER TABLE, DROP, LABEL ON, SET SCHEMA) parsed into file descriptions, never passed on (a view's query is translated and defined by the restricted SQL login, `sql::create_view`); system names for long SQL names; RUNSQLSTM's statement splitting |
| `core/src/ile.rs` | ILE objects: modules, service programs, binding directories (`as4tainr.modules`, `as4tainr.service_programs`, `as4tainr.bnddir_entries`), the binder (imports, exports, binder language, signatures), service program activations per job |
| `core/src/catalog.rs` | Libraries = PostgreSQL schemas; file descriptions in schema `as4tainr`; library list resolution |
| `core/src/rla.rs` | Record-level access: cursor position per open file, keyset queries ordered by (keys, rrn), record locks (advisory locks on the physical file + rrn) |
| `core/src/sql.rs` | SQL in the DB2 for i dialect: translation to PostgreSQL, run on a separate connection as the restricted `as4tainr_sql` login |
| `core/src/runtime.rs` | What a running program shares with its job: database sessions (`connect_db`, `job_session`: application name and record wait), the display channel (`DisplayRequest`) |
| `core/src/dspf.rs` | DDS display files: parser, drawing a record on a 5250 screen, input validation, response indicators |
| `core/src/workstn.rs` | A program's display state: records on the screen (OVERLAY), subfiles (WRITE/READC/CHAIN/UPDATE), one EXFMT's interaction and paging |
| `core/src/tn5250/` | 5250 display server: telnet + records (`telnet.rs`), data stream (`ds.rs`), EBCDIC (`ebcdic.rs`), screens (`session.rs`) |
| `core/src/web/` | Browser 5250 terminal: HTTP + WebSocket (`mod.rs`, `ws.rs`, dependency-free), a 5250 display station that talks TN5250 to this process's server (`display.rs`), the page (`static/`). See `docs/web-terminal.md` |
| `core/src/cl.rs` | CL parser and the command definition table (add new commands here first) |
| `core/src/commands.rs` | CL command implementations for a job |
| `core/src/clpgm.rs` | CL programs: parse/check (CRTBNDCL), expressions (exact decimals), run in the calling job with MONMSG, *DEFINED and *BASED storage |
| `core/src/cmd.rs` | Commands made with CRTCMD (*CMD): command definition statements, checking what was typed, CPP parameters (docs/commands.md) |
| `core/src/prtf.rs` | Printer files (CRTPRTF): page size and overflow line, DDS record formats and their layout |
| `core/src/dtaara.rs` | Data areas: CRTDTAARA/CHGDTAARA/RTVDTAARA values, RPG `IN *LOCK`/`OUT` (PostgreSQL advisory locks), the local data area `*LDA` (`as4tainr.local_data_areas`) |
| `core/src/jobs.rs` | Work management: jobs (`as4tainr.jobs`), job logs, job queue QBATCH and its subsystem loop, user message queues |
| `core/src/spool.rs` | Spooled files (`as4tainr.spooled_files`, `as4tainr.spool_lines`): printer output, QSYSPRT listings of batch display commands, QPJOBLOG |
| `core/src/source.rs` | Source physical files (`PF-SRC` objects) and members (`as4tainr.source_members`), stream files for includes (`as4tainr.stream_files`), CL `INCLUDE` expansion |
| `core/src/import.rs` | `as4tainr import DIR --lib LIB`: members (folders like QRPGLESRC/, TOBi names) and CSV data into a library, then CRTPF, SQL scripts, CRTLF/CRTDSPF/CRTBNDCL/CRTBNDRPG/CRTSQLRPGI/CRTRPGPGM, modules and service programs. See `docs/import.md` |
| `core/src/rpg/` | The native RPG engine (docs/rpg-engine.md): `dec.rs` exact decimals, `data.rs` storage formats (EBCDIC, zoned, packed, binary), `expr.rs`, `parse.rs` (fixed, mixed, **FREE, directives), `program.rs` (names, layout, external files), `code.rs` (blocks → jumps), `interp.rs` (opcodes, BIFs, file I/O on `rla` in its own session), `cycle.rs` (the RPG cycle, input specifications, control levels, matching fields, program-described record images), `ops.rs` (fixed-form string, array and bit operations), `bifs.rs` and `dates.rs` (built-in functions; dates, times, timestamps and their formats), `printer.rs` (WRITE to externally described printer files), `errors.rs` (status codes, the program status and file information data structures, *PSSR and INFSR), `tofree.rs` (fixed form to free form, CVTRPGFREE), `rpg3.rs` (RPG/400 converted to RPG IV, as CVTRPGSRC), `exec_sql.rs` (embedded SQL: SQLCA, host variables, cursors, on `sql::Session`), `mod.rs` (compile, call) |
| `core/src/menus.rs` | Menus: the system menus (MAIN...) as data, GO/CRTMNU TYPE(*PGM)/DLTMNU; the session shows them (`tn5250/session.rs`: initial program, initial menu, menu stack) |
| `core/src/authority.rs` | Object owners, *PUBLIC and private authorities; `check` before every object use (CPF9802) |
| `core/src/users.rs` | User profiles, Argon2id password hashes |
| `core/src/main.rs` | `as4tainr` CLI |
| `core/tests/rla_test.rs` | Integration tests (need `AS4TAINR_TEST_DATABASE_URL`) |
| `tools/tn5250_driver.py`, `tools/e2e_5250.py` | Drive the real tn5250 client for end-to-end tests |
| `tools/e2e_web.py` | Drives the browser terminal in Chromium (Playwright) |
| `tools/gate3_5250.py` | Phase 3 gate: three clerks enter orders at once with batch jobs running; the files must add up. Run after e2e_5250.py |
| `samples/` | DDS, CSV data, RPG and CL programs, golden outputs in `expected/`. `samples/apps/contacts` is an application laid out as a git repository, imported with `as4tainr import` (into CONTACTS by samples.sh). The order entry app (Phase 2 gate) is described in `docs/order-entry.md` |
| `scripts/samples.sh` | Creates every sample object in QGPL (used by demo.sh, the Docker entrypoint and CI). Add new sample objects here |
| `tools/corpus.sh` | Clone a public repository of IBM i source and import it (real-world test; never vendored) |

## Commands

```sh
cargo fmt && cargo clippy --all-targets -- -D warnings
AS4TAINR_TEST_DATABASE_URL=postgres://as4tainr:as4tainr@127.0.0.1/as4tainr cargo test
AS4TAINR_BIN=target/debug/as4tainr scripts/demo.sh
TN5250=/path/to/tn5250 python3 tools/e2e_5250.py 127.0.0.1:2323   # against a running `as4tainr tn5250`
python3 tools/e2e_web.py http://127.0.0.1:5250   # browser terminal in Chromium (pip install playwright), with --web-listen 127.0.0.1:5250
TN5250=/path/to/tn5250 AS4TAINR_BIN=target/debug/as4tainr python3 tools/gate3_5250.py 127.0.0.1:2323   # then the Phase 3 gate
```

## Conventions

- Errors that a program could monitor for start with the IBM-style message ID, e.g. `CPF5026: ...`.
- SQL is built dynamically only with `catalog::ident()` for identifiers. Values are always bound parameters (as text, then `CAST`), except in data definition, which takes no parameters: DEFAULT and CHECK constants are written with `dds::quote()` after `ddl.rs` has parsed them. The exception is user SQL (`sql.rs`), which only ever runs as the `as4tainr_sql` login: never run user-supplied SQL on the main connection.
- Commits are signed off (`git commit -s`). Commit messages describe the IBM i behavior implemented.
