# The as4tainr RPG engine

as4tainr compiles and runs ILE RPG (RPG IV) itself, in Rust. Fixed-form, mixed and fully free-form (`**FREE`) source are all supported. It replaced Jariko on the JVM, which as4tainr used through Phase 3; since milestone R5 there is no JVM.

*Decision, 2026-10-19:* Jariko cannot run fully free-form RPG or free-form declarations. Most RPG written in the last ten years uses both. Jariko also has gaps in fixed form (see fidelity-gaps.md), and each CALL starts a JVM. The project has no deadline, so as4tainr gets its own engine: complete, and self-contained, with no JVM in the container at the end.

It is written from IBM's public *ILE RPG Reference* and *ILE RPG Programmer's Guide*. The clean-room rules in CLAUDE.md apply. No code is taken from Jariko or any other RPG implementation.

## Shape

```
source member ──► source.rs ──► parse ──► check ──► Program ──► interp ──► rla / workstn / spool / dtaara
 (fixed, mixed,   (lines, specs,  (one AST for   (names, types,  (CRTBNDRPG   (async, in the
  **FREE, /COPY)   free regions,    both forms)    storage layout)  keeps it)    core process)
                   directives)
```

| Module (`core/src/rpg/`) | What |
| --- | --- |
| `dec.rs` | Exact decimal numbers (no floating point): add, subtract, multiply, divide with RPG's precision rules, half-adjust, overflow |
| `source.rs` | Source lines: fixed specs by position 6, `/FREE` regions, free-form calculations in positions 8-80, `**FREE`, comments, `/COPY` and `/INCLUDE`, conditional directives, `**CTDATA` |
| `ast.rs` | The program: declarations (files, standalone fields, constants, data structures, prototypes, procedure interfaces), procedures, subroutines, statements, expressions |
| `parse_fixed.rs`, `parse_free.rs`, `expr.rs` | H, F, D, I, C, O, P specs; free-form statements and `DCL-*` declarations; the expression language (used by both forms) |
| `storage.rs` | Every variable lives in bytes in its declared format: character in EBCDIC (CCSID 37), zoned, packed, binary, integer, date/time. Data structure subfields are views of the structure's bytes, so overlays behave as on IBM i |
| `interp.rs` | The RPG cycle, indicators, opcodes, built-in functions, procedures and calls |
| `io.rs` | File operations on `rla` in the same process, display files on `workstn`, printer files on `spool`, data areas on `dtaara` |

Character data is kept in EBCDIC, as on IBM i. Comparisons and sorting follow the EBCDIC order: lower case before upper case, letters before digits. Hex literals (`X'C1'`) and byte-level code work as written. Text is converted at the edges: the database, the display and DSPLY.

## How it plugs in

- **CRTBNDRPG** parses and checks the program. Errors are reported with line numbers at create time, as IBM's compiler does, instead of at CALL. The checked program is kept with the object.
- **CALL** of an RPG program object runs it in the job's own process. It uses the job's database connection, display and spooled files.
- **No switch any more:** the native engine is the only one. Jariko, the JVM runner and the JSON bridge it used were removed in milestone R5 (Oct 19, 2026).

## Status

**R5 is done (Oct 19, 2026): the native engine is the only RPG engine.** It passed everything Jariko passed and more: the samples and demos, the 5250 end-to-end test (130 checks, two more than Jariko: DSPLY with a reply), the Phase 3 gate and the browser terminal test. Then Jariko, the JVM runner, the JSON bridge and its conformance runner were removed: as4tainr is one Rust binary, and the container image has no JRE. The bridge's integration tests became RPG programs run on the engine (display files, subfiles, data area locks, spooled files, authority) or tests of the record-level access layer (record locks).

What R5 added, from running the corpus (Nick Litten's examples, `tools/corpus.sh`):
- **DSPLY with a response** in an interactive job: a Display Program Messages screen with the message and a reply line; the reply goes into the response field (lower case kept). In batch the response stays as it was.
- **System programs** RPG programs call: `QCMDEXC` (runs a CL command in the job), `QDCXLATE` (EBCDIC/ASCII translation with QTCPASC and QTCPEBC), and the C runtime's `Qp0zLprintf` (printf-style text to the job log).
- **CALL of a CL program** from RPG, in the same job (parameters go in as values).
- A program that calls itself is refused: RPG programs are not recursive (procedures are).
- One-line empty interfaces (`dcl-pi *n int(10) end-pi;`).

Corpus status: 23 of its programs (RPG and CL) are created. Most of the others need things outside RPG: embedded SQL (R6), SQL DDL for their files (`CREATE TABLE`), files or includes missing from the repository, or they are snippets that do not compile on IBM i either (fixed C specs inside `**FREE`, unbalanced parentheses). - **Nested data structures:** a subfield defined with `LIKEDS` is a data structure inside the other one (`list.users(i).name`), also through a `LIKEDS` copy of the whole structure and as a parameter; `ds(i).field` for data structure arrays.

**R4 is done (Oct 19, 2026).** Programs call each other and are built from procedures:
- **Procedures** (`DCL-PROC`, P specs) with their own local storage on each call, `STATIC` locals, parameters by reference, `CONST` and `VALUE`, `LIKEDS` parameters, `OPTIONS(*NOPASS : *OMIT)`, `%PARMS`, return values in the declared type, recursion, subroutines inside procedures, `ON-EXIT`.
- **Prototypes** (`DCL-PR`, PR specs): calls to procedures (`EXTPROC`, or the prototype's name) and to programs (`EXTPGM`).
- **Program calls:** `CALL` with `PARM` (factor 2 moved in first; error indicator in 73-74) and `CALLP` through an `EXTPGM` prototype. The called RPG program comes from the library list (CRTBNDRPG) or `AS4TAINR_PGM_PATH`; its parameters are passed by reference (`*ENTRY PLIST`, or a procedure interface outside procedures). Its DSPLY and printed output go to the caller's job.
- **`ctl-opt main(name)`:** the program runs that procedure (no cycle); its interface is the program's parameter list.
- The tests: `native_rpg_engine_runs_a_program` (rla_test.rs) calls programs with CALL and with prototypes, runs procedures with every kind of parameter, STATIC and automatic locals, recursion and ON-EXIT.

**R3 (Oct 19, 2026).** The native engine prints: CUSTPRT matches its golden output, and with `AS4TAINR_RPG_ENGINE=native` the whole 5250 end-to-end test (128 checks) and the Phase 3 gate pass (three clerks entering orders while batch jobs print). CI runs both on the native engine.

What R3 adds:
- **Printer files** (program-described, `PRINTER`) and **O specs**: heading, detail, total and exception records; conditioning indicators with AND/OR lines; space and skip before and after; field end positions and `+n` placement; constants; edit codes; `PAGE`, `UDATE`, `*DATE`; blank after; `OFLIND` (the overflow indicator comes on when line 60 of 66 is printed on or passed). `EXCEPT` with and without a name. Lines go to the job's spooled file (`WRKSPLF`); without a job (`as4tainr call`) they are printed to standard output.
- **The cycle:** first-cycle output with `1P`, a primary file (`IP`) read once per cycle with LR at end of file, total output, detail output after the detail calculations, the overflow indicator set off after detail output.
- **Edit code Y** for six and eight digits with the leftmost zero suppressed; codes that do not blank zero show `0` for a zero integer.

**R2 (Oct 19, 2026).** Interactive programs run on the native engine: CUSTINQ, WRKCUST (load-all subfile, READC, SFLNXTCHG), CUSTMNT (record locks: a second job waits WAITRCD, then gets the error indicator), ORDENT (input subfile, CHAIN/UPDATE of subfile records, data area ORDNXT with `IN *LOCK`/`OUT`) and CONTLST pass the 5250 end-to-end test with `AS4TAINR_RPG_ENGINE=native` (CI runs it). The three-clerk order entry of the Phase 3 gate works too; the gate's batch jobs print, which is R3.

What R2 adds:
- **Display files** (externally described WORKSTN): the record formats' fields become program fields (numbers zoned); EXFMT, WRITE, READC; subfiles with `SFILE(format:rrn)`: WRITE, CHAIN and UPDATE by relative record number; `INDDS`; response indicators; `*INKA`-`*INKY` from the function key pressed. The display work itself (overlay, paging, validation) is the same code Jariko used (`workstn.rs`, `dspf.rs`).
- **Data areas:** `*DTAARA DEFINE`, the `DTAARA` keyword, `IN` (with `*LOCK`), `OUT`, `IN *DTAARA`.
- **Record locks:** the job's database session is named after the job and waits WAITRCD for a locked record, positions 73-74 (or `(E)`) catch the error.

**R1 (Oct 19, 2026).** The batch samples (CUSTLIST, ORDTOTAL, OWING, CUSTUPD, the CL program CUSTRPT calling RPG, CONTCNT) produce their golden output on the native engine. CI runs the demo on it (`AS4TAINR_RPG_ENGINE=native`). Fully free-form programs run too: HELLOWORLD from the corpus prints its message, where Jariko did nothing.

To try it: `as4tainr call CUSTLIST 3`. CRTBNDRPG checks programs (syntax, names, files) when it creates them.

What R1 covers:
- **Source:** fixed form (H F D I C O P specs), `/FREE` regions, free-form calculations in positions 8-80, `**FREE`, `//` comments, `/COPY` and `/INCLUDE` (members and stream files), `/IF DEFINED` / `/DEFINE` / `/EOF`, `**CTDATA`.
- **Declarations:** standalone fields, constants, data structures (from/to positions, POS, OVERLAY, LEN, QUALIFIED, LIKEDS, EXTNAME, DIM, INZ), LIKE, DIM arrays, compile-time arrays, external DISK files (RENAME, PREFIX, EXTDESC), result-field definitions.
- **Calculations:**
  - structured operations: IF, ELSEIF, DOW, DOU, DO, FOR, SELECT/WHEN/OTHER and their fixed-form xx forms with AND/OR;
  - control flow: ITER, LEAVE, subroutines, `*INZSR`, GOTO/TAG, CABxx, MONITOR/ON-ERROR;
  - EVAL/EVALR (including `+=`), arithmetic (Z-ADD, Z-SUB, ADD, SUB, MULT, DIV, MVR), MOVE/MOVEL, COMP, CAT, TIME, CLEAR;
  - SETON/SETOFF, conditioning and resulting indicators;
  - file operations, `*ENTRY PLIST`, KLIST, DSPLY.
- **Built-in functions:** %EOF, %FOUND, %EQUAL, %ERROR, %CHAR, %TRIM(L/R), %SUBST (also as a target), %LEN, %SIZE, %ELEM, %SCAN, %REPLACE, %XLATE, %UPPER, %LOWER, %CHECK(R), %INT(H), %DEC(H), %ABS, %DIV, %REM, %EDITC.
- **The cycle:** detail calculations repeat until LR (no primary file), then total calculations (`LR` in positions 7-8). RETURN ends the program.

## Milestones

1. **R1, batch programs.**
   - Decimal arithmetic, storage and expressions.
   - Fixed and free-form declarations and calculations.
   - Database files (READ, CHAIN, SETLL, READE, WRITE, UPDATE, DELETE) with their indicators and BIFs.
   - DSPLY and `*ENTRY PLIST`.
   - Gate: the batch samples (CUSTLIST, ORDTOTAL, OWING, CUSTUPD, CONTCNT) match their golden outputs on the native engine.
2. **R2, interactive.**
   - Display files: EXFMT, WRITE, READ, subfiles, INDARA, `*INKx`.
   - Gate: CUSTINQ, WRKCUST, CUSTMNT, ORDENT and CONTLST in the e2e test.
3. **R3, the cycle and printing.**
   - Primary files and level breaks.
   - Program-described files with I and O specs, EXCEPT, overflow.
   - Gate: CUSTPRT.
4. **R4, the rest of the job.** Done, except `*LDA` and commitment control (see fidelity-gaps.md).
   - `*LDA`, commitment control (record locks and data areas came with R2).
   - CALL and CALLP between programs (RPG and CL), parameters by reference.
   - Procedures, prototypes, `RETURN` values, `EXPORT`, `ctl-opt main`.
   - Gate: programs that call each other with parameters, and procedure-based programs from the corpus (HELLOADV). (The Phase 3 gate, first planned for R4, already passes on the native engine since R3.)
5. **R5, real code.**
   - The corpus (`tools/corpus.sh`): free-form programs, data structures (`LIKEDS`, `QUALIFIED`, `DIM`, `OVERLAY`), dates, `MONITOR`, `%BIFs`, `/IF DEFINED`.
   - Gate: the default switches to native, then Jariko and the JVM are removed (done).
6. **R6, beyond.**
   - Embedded SQL (SQLRPGLE) on `sql.rs`.
   - Service programs and modules: CRTRPGMOD, CRTSRVPGM, binding directories.
   - RPG/400 (RPG III) source.

Each milestone ends with its gate passing in CI, and with the gaps found written down in fidelity-gaps.md.
