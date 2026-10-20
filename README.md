# AS4tainr

**Run Legacy AS/400 IBM System I code in a container. No official hardware or license required**

AS4tainr (`as4tainr`) is an open, IBM i-compatible (AS/400) runtime that runs in a container.

It runs IBM i business applications (RPG, CL, DDS files) on ordinary Linux and cloud hosts. It does not emulate POWER hardware or boot IBM's operating system. It reimplements the interfaces that programs and people use, the same way Wine runs Windows programs: libraries and objects, database files with record-level access, 5250 green screens, and CL commands. RPG runs on AS4tainr's own RPG compiler and runtime, written in Rust ([docs/rpg-engine.md](docs/rpg-engine.md)): fixed form and fully free form, display files, printing, procedures and program calls.

> Status: **early prototype**. Sign on with a real 5250 emulator or browser (localhost:5250), run CL commands, and `CALL` CL and RPG programs, all against PostgreSQL. That includes **interactive programs that use DDS display files** (`EXFMT`, and subfiles: the scrolling lists behind "Work with..." screens), DDS physical and logical files, SQL, and data areas. The Phase 2 gate is met: a sample **order entry application** (CL driver, RPG with an input subfile, five files, a data area) runs unmodified. See [docs/order-entry.md](docs/order-entry.md), [docs/5250-server.md](docs/5250-server.md) and [docs/display-files.md](docs/display-files.md). Phase 3 has started: jobs, `SBMJOB` with a QBATCH subsystem, job logs and message queues ([docs/work-management.md](docs/work-management.md)). Two users changing the same record are kept apart with IBM i-style record locks ([docs/record-locks.md](docs/record-locks.md)). Printed reports become spooled files (`WRKSPLF`, [docs/spooled-files.md](docs/spooled-files.md)), and objects have owners and authorities ([docs/object-authority.md](docs/object-authority.md)). The Phase 3 gate is met: three clerks enter orders at once while batch jobs print, and the files add up (`tools/gate3_5250.py`).

## How it fits together

```
 5250 / browser ──► as4tainr (one Rust binary) ──► PostgreSQL
                     CL, jobs, display files,      catalog + one table
                     spool, RPG compiler and       per physical file
                     runtime (core/src/rpg/)
```

- **`core/`**: the `as4tainr` binary: the object catalog, the DDS parser, record-level access over PostgreSQL, CL, jobs, the 5250 server and browser terminal, and the RPG engine.
- **`samples/`**: DDS sources, CSV data, RPG programs and their expected (golden) output.
- **`docs/`**: [5250 server](docs/5250-server.md), [browser terminal](docs/web-terminal.md), [display files](docs/display-files.md), [logical files](docs/logical-files.md), [SQL](docs/sql.md), [CL programs](docs/cl-programs.md), [data areas](docs/data-areas.md), [work management](docs/work-management.md), [record locks](docs/record-locks.md), [spooled files](docs/spooled-files.md), [object authority](docs/object-authority.md), [menus](docs/menus.md), [importing an application](docs/import.md), [the RPG engine](docs/rpg-engine.md), [order entry sample](docs/order-entry.md), [known fidelity gaps](docs/fidelity-gaps.md), [spike 01 report](docs/spike-01.md).

## Quick start (Docker)

```sh
docker compose up --build
```

This starts PostgreSQL and the as4tainr 5250 server, with the sample files (`CUSTMAST`, `ITEMMAST`, `ORDHDR`, `ORDLIN`), display files and CL programs in `QGPL` (`scripts/samples.sh`). Open **http://localhost:5250** for a 5250 terminal in your browser, with clickable function keys ([docs/web-terminal.md](docs/web-terminal.md)), or connect a 5250 emulator to **localhost:2323** (tn5250, [tn5250j](https://github.com/tn5250j/tn5250j), or IBM ACS). Sign on as `QSECOFR` with password `QSECOFR`. You will be asked to choose a new password. You land on the main menu ([docs/menus.md](docs/menus.md)): pick an option, or type commands on its command line (option 7 or `CALL QCMD` opens Command Entry). Then try:

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

Type a command name and press **F4** to prompt it, or press F4 on an empty line for a numbered menu of commands. QSECOFR can create more users with `CRTUSRPRF`. To bring in your own application (a git repository of IBM i source), use `as4tainr import DIR --lib LIB`: see [docs/import.md](docs/import.md).

`docker compose run --rm demo` runs the sample programs and checks their output against `samples/expected/`.

## Building from source

Requirements: Rust (stable), and PostgreSQL 16 for the integration tests and the samples.

```sh
cargo build
AS4TAINR_TEST_DATABASE_URL=postgres://as4tainr:as4tainr@127.0.0.1/as4tainr cargo test
```

Run the samples against a local database:

```sh
export AS4TAINR_DATABASE_URL=postgres://as4tainr:as4tainr@127.0.0.1/as4tainr
export AS4TAINR_BIN=target/debug/as4tainr
scripts/demo.sh
```

## CLI

| Command | IBM i equivalent | What it does |
| --- | --- | --- |
| `as4tainr init` | | Creates the catalog and the `QSYS`, `QGPL` and `QTEMP` libraries |
| `as4tainr crtlib LIB` | `CRTLIB` | Creates a library (a PostgreSQL schema) |
| `as4tainr system "COMMAND"` | `system` (QSH) | Runs one CL command as QSECOFR, e.g. `as4tainr system "CRTDTAARA DTAARA(QGPL/X) TYPE(*DEC)"` |
| `as4tainr crtpf LIB/FILE --src x.pf` | `CRTPF` | Creates a physical file from DDS |
| `as4tainr crtbndcl LIB/PGM --src x.clle` | `CRTBNDCL` | Creates a CL program ([docs/cl-programs.md](docs/cl-programs.md)); `CALL` runs it |
| `as4tainr sql "SELECT ..."` | `RUNSQL` / `STRSQL` | Runs one SQL statement in the DB2 for i dialect, including CREATE TABLE and CREATE INDEX ([docs/sql.md](docs/sql.md)) |
| `as4tainr crtlf LIB/FILE --src x.lf` | `CRTLF` | Creates a logical file over a physical file from DDS ([docs/logical-files.md](docs/logical-files.md)) |
| `as4tainr crtdspf LIB/FILE --src x.dspf` | `CRTDSPF` | Creates a display file from DDS |
| `as4tainr load FILE --csv x.csv` | `CPYFRMIMPF` (roughly) | Writes CSV rows with `WRITE` |
| `as4tainr call PGM [parms]` | `CALL` | Runs a CL or RPG program; DSPLY and printed output go to standard output |
| `as4tainr tn5250 --listen 0.0.0.0:23` | | Runs the 5250 display server (sign-on, menus, command entry) and the QBATCH subsystem for submitted jobs. `--web-listen 0.0.0.0:8080` also serves the browser 5250 terminal |

Settings: `AS4TAINR_DATABASE_URL`, `AS4TAINR_QSECOFR_PASSWORD` (initial QSECOFR password), `AS4TAINR_SYSTEM_NAME`, `AS4TAINR_LIBL` (library list, comma-separated, default `QGPL`), `AS4TAINR_PGM_PATH` (directories with RPG sources run by name, the samples), `AS4TAINR_WAITRCD` (seconds to wait for a locked record).

## Roadmap

| Phase | Scope | Exit gate |
| --- | --- | --- |
| 0 · Foundations | Object store, EBCDIC/decimal library, TN5250 sign-on, CL shell | Sign on with tn5250, run `CRTLIB` and `DSPLIB` |
| 1 · Data | DDS physical and logical files, record-level access, SQL | `CRTPF` from DDS, read it with RLA and SQL |
| 2 · Programs (v1) | CL compiler, RPG IV (first on Jariko, now as4tainr's own RPG engine), display files | A sample order-entry app runs unmodified (met: [docs/order-entry.md](docs/order-entry.md)) |
| 3 · Operations | Jobs, subsystems, spool, security | Multi-user interactive and batch workload (met: `tools/gate3_5250.py`) |
| 4 · Ecosystem | RPG/400, COBOL, host servers, migration tools | ACS and ODBC tools connect |

Spike 01 brings forward part of phase 1 (record-level access) and phase 2 (Jariko integration), because these were the riskiest assumptions.

## Contributing

Contributions are welcome. Please read [CONTRIBUTING.md](CONTRIBUTING.md) first, especially the **clean-room rules**: as4tainr is built only from public documentation and observed behavior, never from IBM code. Every commit needs a DCO sign-off (`git commit -s`).

## Legal

Apache License 2.0. IBM, IBM i, AS/400, iSeries and DB2 are trademarks of IBM. as4tainr is an independent project, not affiliated with or endorsed by IBM. See [NOTICE](NOTICE).
