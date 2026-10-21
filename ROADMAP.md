# AS4tainr: Architecture and Roadmap

This file is the master roadmap. Update it in the same commit as the work that changes it.

**Status (October 2026):** phases 0 to 3 have met their gates; phase 4 has not started.

## Vision and scope

AS4tainr is an open, containerized runtime that behaves like IBM i from the user's and developer's side: green-screen 5250 sessions, CL commands, libraries and objects, DB2-style database files, and RPG and CL programs. It runs anywhere Docker runs. No IBM code and no POWER hardware are involved.

**What it is:** a clean-room *compatible* environment. It works the way Wine runs Windows apps on Linux, by reimplementing the interfaces programs and people use. It does not emulate the CPU.

**What it is not:** a hardware emulator that boots real IBM i (SLIC/OS400) images. That path is blocked by missing POWER tags-active and PowerVM support in QEMU, by IBM's hardware-tied licensing, and by restricted install media. It stays a research track (see Risks).

**Who it is for, in priority order:**

1. **Legacy app hosting (primary).** Businesses run their existing RPG, CL and DDS applications on commodity Linux and cloud hosts, off Power hardware and IBM i licensing.
2. **Teams testing in CI** who want a disposable environment in a pipeline: `docker run`, compile, test, throw away.
3. **Developers and students** who need an IBM i-like box to learn on, without renting Power hardware.

**Definition of success for v1:** a typical green-screen order-entry application, made of DDS files, a display file, RPG IV and CL, compiles and runs unmodified. A user reaches it with a standard 5250 client such as tn5250 or IBM ACS.

## Legal and clean-room guardrails

The project is only viable if it never ships or derives from IBM code. The working rules for contributors are in [CONTRIBUTING.md](CONTRIBUTING.md). *(Not legal advice: have an IP lawyer review them before the public launch.)*

- **Build from public behavior and specs only.** Allowed sources are IBM's public documentation (IBM Docs, Redbooks, CL and DDS references), published RFCs (TN5250: RFC 1205, RFC 2877, RFC 4777), open-source projects with compatible licenses, and observed behavior on a legally licensed system such as PUB400.
- **Never use IBM binaries, disassembly, or licensed internal code.** Contributors certify this through a DCO sign-off on every commit.
- **Separate observers from implementers** wherever behavior is studied on a real system. Observers write behavior specs into the repo. Implementers code only from those specs.
- **Name with care.** "IBM i", "AS/400", "iSeries" and "DB2" are IBM trademarks. The product describes itself only as "IBM i-compatible" (nominative use) and uses no IBM logos or look-alike branding.
- **Treat command and API names as interface facts.** Reimplementing names like `CRTPF` or `WRKACTJOB` and their parameters follows the same reasoning that let Wine and ReactOS implement Windows APIs, and *Google v. Oracle* (2021) supports it. Message text and help text are written fresh.
- **Customer code stays the customer's.** Users may import their own source and data. The project ships with sample apps only.

## System architecture

```mermaid
flowchart TB
    emu["5250 emulator<br/>(tn5250, ACS)"]
    web["Web browser"]
    ci["CI pipeline or script"]
    subgraph app["as4tainr container: one Rust binary"]
        srv["5250 server<br/>telnet on 23, browser terminal on 8080"]
        wm["Work management<br/>jobs, subsystems, job queues"]
        cl["CL command processor<br/>parse, prompt, run *CMD"]
        dsp["Display and printer files<br/>DDS screens, subfiles, spool"]
        rt["Language runtimes<br/>RPG IV, RPG III and CL"]
        obj["Objects and libraries<br/>typed objects, library list"]
        sec["Security<br/>profiles, object authority"]
        dt["Data types<br/>EBCDIC, packed and zoned decimal, dates"]
        dba["Database access<br/>record-level I/O and DB2 for i SQL"]
    end
    db[("db container: PostgreSQL 16<br/>object catalog, database files")]
    emu -- "5250 sessions" --> srv
    web -- "browser terminal" --> srv
    ci -- "as4tainr CLI" --> app
    dba -- "SQL and keyed access" --> db
```

Clients reach the platform through 5250 sessions, the browser terminal or the `as4tainr` CLI. All logic lives in one Rust binary. The database access layer translates IBM i-style record I/O and SQL into PostgreSQL. That layer is the hardest piece to get right, and every application depends on it.

## Core subsystems

Eight subsystems make up the platform. Two of them, the object model and the data layer, carry the most design risk. The data layer is where IBM i behaves least like ordinary Linux and SQL software.

| Subsystem | What it reproduces | How it is built | Phase |
| --- | --- | --- | --- |
| Object model and libraries | Typed objects (`*PGM`, `*FILE`, `*DTAARA`, `*USRPRF`, `*MSGQ`, `*OUTQ`), libraries (`QSYS`, `QGPL`, per-job `QTEMP`), library list resolution | An object store whose catalog lives in PostgreSQL. Libraries are PostgreSQL schemas. Names are 10-character EBCDIC-safe. | 0 |
| Data types and EBCDIC | CCSID 37/500/1047, packed and zoned decimal, date/time formats | A shared Rust core library with exact decimal arithmetic. It never uses floating point for business math. | 0 |
| 5250 server | Green-screen sessions, sign-on, function keys, subfiles | A TN5250 server on port 23, with TLS on 992 planned. A browser 5250 terminal covers zero-install access. | 0 |
| CL command processor | Command parsing, prompting (F4), `*CMD` definitions, CL programs | A table-driven command definitions engine. Commands are declared once as data and drive the parser, the prompter and the help. | 0–2 |
| Database | DDS physical and logical files, keyed record-level access (`READ`, `CHAIN`, `SETLL`), journaling, SQL | PostgreSQL as the storage engine. A record-level access layer sits on top, plus a DB2 for i SQL dialect translator. | 1 |
| Display and printer files | DDS `*DSPF` and `*PRTF`, record formats, indicators, subfiles | A DDS compiler to an internal screen model, rendered over the 5250 data stream. Printed output becomes spooled files. | 2 |
| Language runtimes | RPG IV (fixed, mixed and free form), RPG/400 (RPG III), ILE CL; COBOL later | A native RPG compiler and runtime in Rust, including the RPG cycle, embedded SQL and ILE binding. CL runs in a native Rust interpreter. | 2 |
| Work management and security | Jobs, subsystems (`QINTER`, `QBATCH`), job queues, `SBMJOB`, message queues, user profiles, object authority | A job scheduler in the core. Each job is an isolated execution context. Authority checks run on every object access. | 3 |

## Container and deployment layout

The platform ships as one `docker compose` stack ([docker-compose.yml](docker-compose.yml)) of two services and one persistent volume. The `as4tainr` service is a single Rust binary in a debian:bookworm-slim image.

| Service | Role | Ports | State |
| --- | --- | --- | --- |
| `as4tainr` | Object store, CL and RPG, jobs and subsystems, security, the 5250 server and the browser 5250 terminal | 23 (5250, published as 2323), 8080 (browser terminal, published as 5250) | Stateless |
| `db` | PostgreSQL 16: object catalog plus all database files | 5432 (internal only) | `pgdata` volume |

A `demo` profile runs the sample programs against their golden output (`docker compose run --rm demo`).

On first start, `as4tainr` builds the system libraries (`QSYS`, `QGPL`, `QUSRSYS`) and the `QSECOFR` profile with a forced password change, unless `AS4TAINR_QSECOFR_PASSWORD` is set. Interactive jobs run in `QINTER` and submitted jobs in `QBATCH`. A system backup is a backup of the `pgdata` volume.

## Technology stack

Rust for the core and for the RPG and CL runtimes, PostgreSQL for storage. With its own RPG compiler, the whole system is one Rust binary beside PostgreSQL.

| Layer | Choice | Why |
| --- | --- | --- |
| Core services | Rust | Memory safety, predictable performance, and exact decimal arithmetic. The compiler's strictness catches many bugs in AI-generated code. |
| Storage | PostgreSQL 16 | Transactions, journaling via WAL, and mature tooling. Keyed access maps onto B-tree indexes. |
| RPG runtime | Native RPG compiler and runtime in Rust ([docs/rpg-engine.md](docs/rpg-engine.md)) | Compiles fixed, mixed and free-form RPG IV and RPG III, and runs inside the core process. |
| CL runtime | Native Rust interpreter | CL is small and tied tightly to the command processor and job model. |
| 5250 server | Rust, async (tokio) | Many idle sessions at low cost. |
| Browser 5250 terminal | Plain JavaScript page served by the core process ([docs/web-terminal.md](docs/web-terminal.md)) | Zero-install access for users without a 5250 client. |
| Packaging | Docker images; multi-arch (amd64, arm64) planned | Runs on laptops, Apple Silicon and cloud. Podman-compatible. |
| Testing | Golden-output tests, to be captured from a licensed system | Fidelity is the product, and these tests are the main safeguard for AI-generated code. |

## Phased roadmap

Each phase ends at a gate that can be shown in a demo, and nothing moves forward until the gate passes. Phase 2's gate is v1: real-world RPG, CL and DDS code runs without changes.

| Phase | Scope | Exit gate | Status |
| --- | --- | --- | --- |
| 0 · Foundations | Object store, EBCDIC and decimal library, TN5250 server and sign-on, CL shell | Sign on with tn5250, run `CRTLIB` and `DSPLIB` | Met |
| 1 · Data | DDS physical and logical files, record-level access, SQL in the DB2 for i dialect | `CRTPF` from DDS, then read it with RLA and SQL | Met |
| 2 · Programs (v1) | CL compiler (`CRTBNDCL`), RPG IV in fixed and free form, display files and subfiles | Sample order-entry app runs unmodified | Met ([docs/order-entry.md](docs/order-entry.md)) |
| 3 · Operations | Jobs, subsystems, `SBMJOB`, spooled files, message queues, user profiles, object authority | Multi-user interactive and batch workload | Met (`tools/gate3_5250.py`, run in CI) |
| 4 · Ecosystem | ODBC/JDBC and ACS host servers, COBOL, TLS 5250, `SAVLIB`/`RSTLIB` | ACS and ODBC tools connect unchanged | Not started; real-app readiness comes first |

The gates were met with the project's own sample apps, so real legacy code is the next test.

### What runs today

- **RPG:** RPG IV in fixed, mixed and free form up to IBM i 7.5 (date built-ins, FOR-EACH, IN, DCL-ENUM, OVERLOAD, SND-MSG). RPG/400 (RPG III) builds with CRTRPGPGM and converts with CVTRPGSRC. The full RPG cycle runs: control levels, matching records, program-described files, edit words. CVTRPGFREE converts fixed form to free.
- **RPG runtime:** status codes, the program status and file information data structures, \*PSSR, INFSR and ENDSR return points, overflow and fetch overflow, the local data area (\*LDA), and files declared in procedures. Pointers: %ADDR, BASED storage, pointer arithmetic, %ALLOC/%REALLOC/DEALLOC, %STR and OPTIONS(\*STRING).
- **ILE:** modules, service programs and binding directories (CRTRPGMOD, CRTSRVPGM, CRTPGM, binder language); activation groups, signature checking, exported data, and CL modules (CALLPRC).
- **SQL:** embedded SQL (SQLRPGLE, CRTSQLRPGI) with cursors, prepared statements, host structures and null indicators. SQL DDL (CREATE SCHEMA, CREATE TABLE, CREATE INDEX, ALTER TABLE, DROP, LABEL ON, GRANT) and RUNSQLSTM (from a member or a stream file) create real files that RPG reads and writes, with long names, views, identity columns, foreign keys, SQL triggers (which fire for record-level access too), CREATE TABLE AS, and the SQL data types including DECFLOAT, BOOLEAN, XML and TIMESTAMP(p). SQL procedures and functions (CREATE PROCEDURE, CREATE FUNCTION, compound statements with handlers, cursors and loops), CALL from STRSQL, RUNSQL, RUNSQLSTM and embedded SQL, dynamic compound statements, external procedures over RPG and CL programs, and SQL table functions. IBM i services: the QSYS2 catalog views (SYSSCHEMAS, SYSTABLES, SYSCOLUMNS, SYSINDEXES, SYSKEYS, SYSVIEWS, SYSROUTINES, SYSPARMS), OBJECT_STATISTICS, LIBRARY_LIST_INFO, USER_INFO, SYSTEM_VALUE_INFO, JOBLOG_INFO, ACTIVE_JOB_INFO, IFS_READ, IFS_WRITE and QCMDEXC; JSON_TABLE.
- **Commitment control:** STRCMTCTL, ENDCMTCTL, COMMIT and ROLLBACK in CL, RPG files with the COMMIT keyword and COMMIT/ROLBK, and embedded SQL under its commit level; pending changes roll back when the job ends.
- **CL:** program message queues (SNDPGMMSG, RCVMSG from the program's, its caller's and user message queues with WAIT, RTVMSG) and message files (CRTMSGF, ADDMSGD), files in CL (DCLF, RCVF, SNDF, SNDRCVF), overrides (OVRDBF, OVRPRTF, DLTOVR), subroutines (SUBR, CALLSUBR), parameters passed by reference between CL and RPG programs, RTVOBJD, RTVMBRD, CLRPFM, DLYJOB, RTVSYSVAL and the newer CL built-ins.
- **IFS:** directories and stream files with CCSIDs, the C runtime and UNIX-type APIs bound from QC2LE (open, read, write, close, lseek, unlink, mkdir, errno, strerror, string and memory functions, system()), and the CL commands MKDIR, RMVDIR, RMVLNK, RNM, WRKLNK, CPYTOSTMF and CPYFRMSTMF.
- **Real applications:** `tools/legacy.sh` installs Northwind for IBM i (its SQL database, then its own CL build program builds 13 SQLRPGLE service programs, 14 display files and 15 programs) and the COM order management application (customer, item and order maintenance with message subfiles and the QMHSNDPM API), and both run on a 5250 display.
- **System:** commands made with CRTCMD, DDS printer files (CRTPRTF), READ of a display record, and CL defined and based storage.

50 corpus objects (43 programs, 4 modules, 3 service programs) and 6 commands build; most remaining corpus failures would fail on IBM i too (see docs/import.md). Known gaps are listed in [docs/fidelity-gaps.md](docs/fidelity-gaps.md).

## Risks

The biggest risk is the size of the surface to cover. IBM i has thousands of CL commands and system APIs, and real applications use a long tail of them. We manage this by building to the needs of real sample apps, not to the whole manual.

| Risk | Impact | Mitigation |
| --- | --- | --- |
| Solo, AI-assisted development: plausible-looking code that is subtly wrong, plus a bus factor of one | Silent errors in decimal math, locking, or record I/O | Golden-output tests. Human review of every change to the decimal and database layers. Design docs and CLAUDE.md in the repo so any session or contributor can pick up the work. |
| Surface area: commands, APIs (`QSYS` APIs, `QUSRSPC`), RPG opcodes and BIFs | Legacy apps fail on the first unsupported call | Build a corpus of legacy-style RPG/CL apps. Implement in order of usage frequency. Unsupported calls fail with a clear `CPF`-style message. |
| Fidelity drift: decimal rounding, record locking, commitment control | Silent wrong answers, the worst failure for business software | Golden-output tests against a licensed system. Exact-decimal-only arithmetic. |
| Clean-room contamination | Legal exposure for the project | Guardrails above, DCO, and clean-room rules written into CLAUDE.md. |
| Trademark complaints | Forced rename after launch | The name "AS4tainr" evokes IBM's AS/400 mark ("AS4"). Get a trademark opinion before the public launch, and keep a fallback name ready. Use "IBM i-compatible" wording only. |
| Host-server protocols (ODBC/JDBC, ACS file transfer on ports 8470–8476) | Existing desktop tools can't connect, which matters for hosted legacy apps | Phase 4. IBM's open-source JTOpen toolbox documents the protocols in code. |
| Hardware-level emulation stays out of reach | No path to running real IBM i images | Keep it as a separate research track. Revisit only if IBM licensing changes. |

## Decisions

- **Name:** AS4tainr. The project will be public on GitHub after the first release, to attract contributors.
- **License:** Apache-2.0 ([LICENSE](LICENSE)).
- **Core language:** Rust.
- **Audience:** legacy app hosting first; a training environment second.
- **Fidelity:** as close to IBM i as possible. When IBM behavior and convenience differ, follow IBM.

## Next steps

- [ ] Get licensed access to a real system (for example PUB400) and capture golden outputs. Until then, expected values come from IBM's documentation.
- [x] Run real applications end to end and fix what they need: Northwind for IBM i and COM order management (`tools/legacy.sh`, docs/import.md).
- [ ] A third real application, fixed-form heavy (JCRCMDS, if its download can be used for testing), and what COM still lacks: journals (QSYS2.DISPLAY_JOURNAL) and external trigger programs (ADDPFTRG).
- [x] Close the SQL gaps: commitment control, compound statements, routines.
- [ ] Remaining SQL: external functions, INSTEAD OF triggers, sequences, the other QSYS2 catalog views (constraints, triggers) and IBM i services (JOB_INFO, OUTPUT_QUEUE_ENTRIES, OBJECT_PRIVILEGES...), the other JSON functions (JSON_VALUE, JSON_OBJECT, JSON_ARRAYAGG).
- [x] C runtime and IFS procedures (open, read, write, close) for RPG, with pointers.
- [ ] IFS beyond the basics: stat() and directory APIs, CPYTOIMPF/CPYFRMIMPF, procedure pointers, pointers between programs, IFS authority checks.
- [ ] Packaging: TLS on port 992, SAVLIB and RSTLIB with a portable archive format, multi-arch images (amd64, arm64).
- [ ] Get a trademark opinion on the name before the public launch.
- [ ] Phase 4: ODBC/JDBC and ACS host servers, so existing tools connect.
