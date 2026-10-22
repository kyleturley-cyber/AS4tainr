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
| `core/src/ddl.rs` | SQL data definition (CREATE SCHEMA/TABLE/INDEX/VIEW/TRIGGER, ALTER TABLE, DROP, LABEL ON, GRANT, SET SCHEMA) parsed into file descriptions, never passed on (a view's query is translated and defined by the restricted SQL login, `sql::create_view`); system names for long SQL names; RUNSQLSTM's statement splitting |
| `core/src/ile.rs` | ILE objects: modules, service programs, binding directories (`as4tainr.modules`, `as4tainr.service_programs`, `as4tainr.bnddir_entries`), the binder (imports, exports, binder language, signatures), service program activations per job |
| `core/src/catalog.rs` | Libraries = PostgreSQL schemas; file descriptions in schema `as4tainr`; library list resolution |
| `core/src/rla.rs` | Record-level access: cursor position per open file, keyset queries ordered by (keys, rrn), record locks (advisory locks on the physical file + rrn) |
| `core/src/sqlpl.rs` | SQL procedures and functions (SQL PL): CREATE PROCEDURE/FUNCTION parsed into statements (`as4tainr.routines`); procedures run here statement by statement (handlers, cursors, loops, SIGNAL, GET DIAGNOSTICS), functions written as PostgreSQL functions; CALL |
| `core/src/cmtctl.rs` | Commitment control: commitment definitions per job and activation group (one PostgreSQL session with a transaction each), STRCMTCTL/ENDCMTCTL, COMMIT/ROLLBACK, the job's end |
| `core/src/services.rs` | IBM i services for SQL: the QSYS2 catalog views (SYSTABLES, SYSCOLUMNS, SYSROUTINES...), OBJECT_STATISTICS, LIBRARY_LIST_INFO, USER_INFO, SYSTEM_VALUE_INFO, JOBLOG_INFO, ACTIVE_JOB_INFO, IFS_READ (IFS_WRITE and QCMDEXC are procedures in `sqlpl.rs`), as PostgreSQL views and functions over the catalog (made again when `VERSION` changes) |
| `core/src/sqljson.rs` | JSON_TABLE rewritten for PostgreSQL's SQL/JSON path functions |
| `core/src/sqljsonfn.rs` | JSON_OBJECT, JSON_ARRAY, JSON_ARRAYAGG, JSON_OBJECTAGG, JSON_VALUE, JSON_QUERY, JSON_EXISTS rewritten to PostgreSQL json with Db2's compact text (the `as4tainr_svc.json_*` functions) |
| `core/src/sqlcat.rs` | Catalog views SYSSEQUENCES, SYSCST, SYSKEYCST, SYSCHKCST, SYSREFCST, SYSTRIGGERS and services JOB_INFO, OUTPUT_QUEUE_ENTRIES(_BASIC), OBJECT_PRIVILEGES (made with the services) |
| `core/src/sql.rs` | SQL in the DB2 for i dialect: translation to PostgreSQL, run on a separate connection as the restricted `as4tainr_sql` login |
| `core/src/runtime.rs` | What a running program shares with its job: database sessions (`connect_db`, `job_session`: application name and record wait), the display channel (`DisplayRequest`) |
| `core/src/dspf.rs` | DDS display files: parser, drawing a record on a 5250 screen, input validation, response indicators |
| `core/src/workstn.rs` | A program's display state: records on the screen (OVERLAY), subfiles (WRITE/READC/CHAIN/UPDATE), one EXFMT's interaction and paging |
| `core/src/tn5250/` | 5250 display server: telnet + records (`telnet.rs`), data stream (`ds.rs`), EBCDIC (`ebcdic.rs`), screens (`session.rs`) |
| `core/src/web/` | Browser 5250 terminal: HTTP + WebSocket (`mod.rs`, `ws.rs`, dependency-free), a 5250 display station that talks TN5250 to this process's server (`display.rs`), the page (`static/`). See `docs/web-terminal.md` |
| `core/src/cl.rs` | CL parser and the command definition table (add new commands here first) |
| `core/src/commands.rs` | CL command implementations for a job |
| `core/src/clpgm.rs` | CL programs: parse/check (CRTBNDCL), expressions (exact decimals), run in the calling job with MONMSG, *DEFINED and *BASED storage, program messages (SNDPGMMSG, RCVMSG, RTVMSG), files (DCLF, RCVF, SNDF, SNDRCVF, CLOSE), subroutines, parameters by reference, RTVSYSVAL, RTVOBJD, RTVMBRD |
| `core/src/msgf.rs` | Message files (*MSGF) and message descriptions (`as4tainr.message_descriptions`): CRTMSGF, ADDMSGD, CHGMSGD, RMVMSGD, substitution of message data (&1-&99 by FMT) |
| `core/src/pgmmsg.rs` | The job's CL call stack (`Job.calls`): program message queues (SNDPGMMSG, RCVMSG), the escape message being raised, file overrides by call level |
| `core/src/ovr.rs` | File overrides (OVRDBF, OVRPRTF, DLTOVR): which override counts, the file a program opens |
| `core/src/cmd.rs` | Commands made with CRTCMD (*CMD): command definition statements, checking what was typed, CPP parameters (docs/commands.md) |
| `core/src/prtf.rs` | Printer files (CRTPRTF): page size and overflow line, DDS record formats and their layout |
| `core/src/dtaara.rs` | Data areas: CRTDTAARA/CHGDTAARA/RTVDTAARA values, RPG `IN *LOCK`/`OUT` (PostgreSQL advisory locks), the local data area `*LDA` (`as4tainr.local_data_areas`) |
| `core/src/jobs.rs` | Work management: jobs (`as4tainr.jobs`), job logs, job queue QBATCH and its subsystem loop, user message queues |
| `core/src/report.rs` | Printed reports in IBM's layouts (page headers A and B, columns, end-of-listing lines) for display commands with OUTPUT(*PRINT), batch and `system` |
| `core/src/pase_system.rs` | The PASE `system` utility (`as4tainr system`): options -h -n -q -s -v, messages to standard output or error, exit status 0/255/2 |
| `core/src/spool.rs` | Spooled files (`as4tainr.spooled_files`, `as4tainr.spool_lines`): printer output, QSYSPRT listings of batch display commands, QPJOBLOG |
| `core/src/source.rs` | Source physical files (`PF-SRC` objects) and members (`as4tainr.source_members`), include files found in the IFS, CL `INCLUDE` expansion |
| `core/src/ifs.rs` | The integrated file system: directories and stream files (`as4tainr.ifs`), errno values, CCSID conversion (docs/ifs.md) |
| `core/src/journal.rs` | Journals: CRTJRNRCV, CRTJRN, CHGJRN, DLTJRN, DLTJRNRCV, STRJRNPF, ENDJRNPF, DSPJRN, APYJRNCHG, RMVJRNCHG (`as4tainr.journals`, `journal_receivers`, `journaled_files`, `journal_entries`), the PostgreSQL trigger that journals record changes, record images (and back to column values, `image_json`), commit cycles, rollbacks journaled (`hold`/`restore`, `rollback_to`), QSYS2.DISPLAY_JOURNAL, INTERPRET (docs/journals.md) |
| `core/src/pftrg.rs` | Trigger programs: ADDPFTRG, RMVPFTRG (`as4tainr.file_triggers`), the PostgreSQL triggers that note changes, the trigger buffer, `Change` (a change with its programs in the caller's session: *BEFORE programs on a trial run, ALWREPCHG, commitment control rules) and `run` for SQL; the RPG engine drives `Change` itself (`interp.rs` `triggered_change`, `call_trigger`) |
| `core/src/save.rs` | Save and restore: CRTSAVF, CLRSAVF, DSPSAVF, SAVOBJ, SAVLIB, RSTOBJ, RSTLIB, DLTLIB, CLRLIB (`as4tainr.save_files`); a save is as4tainr's own format (catalog rows as JSON, PostgreSQL definitions rebuilt from the system catalogs, COPY data), F MS/MR journal entries for APYJRNCHG *LASTSAVE (docs/save-restore.md) |
| `core/src/objdesc.rs` | DSPFD (all types; the printed *ATR report is `commands.rs` `dspfd_atr`) and DSPFFD; CHKOBJ, DSPOBJD and DSPSYSVAL are in `commands.rs`; CPYF in `cpyf.rs` |
| `core/src/ifs_commands.rs` | IFS CL commands: MKDIR, RMVDIR, RMVLNK, RNM, WRKLNK, CPYTOSTMF, CPYFRMSTMF (members and save files) |
| `core/src/import.rs` | `as4tainr import DIR --lib LIB`: members (folders like QRPGLESRC/, TOBi names) and CSV data into a library, then CRTPF, SQL scripts, CRTLF/CRTDSPF/CRTBNDCL/CRTBNDRPG/CRTSQLRPGI/CRTRPGPGM, modules and service programs. See `docs/import.md` |
| `core/src/rpg/` | The native RPG engine (docs/rpg-engine.md): `dec.rs` exact decimals, `data.rs` storage formats (EBCDIC, zoned, packed, binary), `expr.rs`, `parse.rs` (fixed, mixed, **FREE, directives), `program.rs` (names, layout, external files), `code.rs` (blocks → jumps), `interp.rs` (opcodes, BIFs, file I/O on `rla` in its own session), `cycle.rs` (the RPG cycle, input specifications, control levels, matching fields, program-described record images), `ops.rs` (fixed-form string, array and bit operations), `bifs.rs` and `dates.rs` (built-in functions; dates, times, timestamps and their formats), `printer.rs` (WRITE to externally described printer files), `mem.rs` and `ptrs.rs` (storage as pointers see it: heap, lent caller storage, BASED, %ADDR, %ALLOC, %STR), `crt.rs` (the C runtime and IFS APIs bound from QC2LE), `errors.rs` (status codes, the program status and file information data structures, *PSSR and INFSR), `tofree.rs` (fixed form to free form, CVTRPGFREE), `rpg3.rs` (RPG/400 converted to RPG IV, as CVTRPGSRC), `exec_sql.rs` (embedded SQL: SQLCA, host variables, cursors, on `sql::Session`), `mod.rs` (compile, call) |
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
| `tools/golden/`, `tools/golden_compare.py`, `tools/golden.py` | Capture output on a real IBM i (PUB400 over SSH, outputs kept in target/ibmi/golden, never committed) and compare it with `as4tainr system`; `screens.ps1` with `Tn5250Capture.cs` captures 5250 screens (docs/golden-outputs.md) |
| `tools/golden.py` | Compares as4tainr with captures from a real IBM i (exit status, message IDs, printed output) and looks for IBM's message texts in the source (`--audit`); the captures stay in `target/ibmi/golden` (docs/golden-outputs.md) |
| `tools/corpus.sh` | Clone a public repository of IBM i source and import it (real-world test; never vendored) |
| `tools/legacy.sh` | Install real applications (Northwind for IBM i, COM order management) the way their READMEs say, on an empty database (cloned at test time; never vendored). docs/import.md |

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

- Errors that a program could monitor for start with the IBM-style message ID, e.g. `CPF5026: ...`. A command's diagnostics come first, one per line, and the escape message last (`CPD0071: ...\nCPF0006: ...`, `cl::diag_error`); `jobs::escape_id` takes the last ID. Completion messages carry their ID (`CPC2191: ...`) only where a capture from a real system shows it. Message texts are always as4tainr's own: `tools/golden.py GOLDEN_DIR --audit core/src` finds IBM's texts in the source.
- SQL is built dynamically only with `catalog::ident()` for identifiers. Values are always bound parameters (as text, then `CAST`), except in data definition, which takes no parameters: DEFAULT and CHECK constants are written with `dds::quote()` after `ddl.rs` has parsed them. The exception is user SQL (`sql.rs`), which only ever runs as the `as4tainr_sql` login: never run user-supplied SQL on the main connection.
- Commits are signed off (`git commit -s`). Commit messages describe the IBM i behavior implemented.
