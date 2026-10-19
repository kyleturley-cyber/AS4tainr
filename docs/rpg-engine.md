# The ibmish RPG engine

ibmish compiles and runs ILE RPG (RPG IV) itself, in Rust. Fixed-form, mixed and fully free-form (`**FREE`) source are all supported. This replaces Jariko on the JVM, which ibmish used through Phase 3.

*Decision, 2026-10-19:* Jariko cannot run fully free-form RPG or free-form declarations. Most RPG written in the last ten years uses both. Jariko also has gaps in fixed form (see fidelity-gaps.md), and each CALL starts a JVM. The project has no deadline, so ibmish gets its own engine: complete, and self-contained, with no JVM in the container at the end.

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
| `io.rs` | File operations on `rla` in the same process (no bridge), display files on `workstn`, printer files on `spool`, data areas on `dtaara` |

Character data is kept in EBCDIC, as on IBM i. Comparisons and sorting follow the EBCDIC order: lower case before upper case, letters before digits. Hex literals (`X'C1'`) and byte-level code work as written. Text is converted at the edges: the database, the display and DSPLY.

## How it plugs in

- **CRTBNDRPG** parses and checks the program. Errors are reported with line numbers at create time, as IBM's compiler does, instead of at CALL. The checked program is kept with the object.
- **CALL** of an RPG program object runs it in the job's own process. It uses the job's database connection, display and spooled files.
- **The switch:** `IBMISH_RPG_ENGINE=native` uses the new engine, `jariko` the old one. The default moves to `native` once it passes everything Jariko passes: the samples, the demos, e2e, the Phase 3 gate, and the corpus. Jariko and the JVM are then removed.

## Status

**R2 is done (Oct 19, 2026).** Interactive programs run on the native engine: CUSTINQ, WRKCUST (load-all subfile, READC, SFLNXTCHG), CUSTMNT (record locks: a second job waits WAITRCD, then gets the error indicator), ORDENT (input subfile, CHAIN/UPDATE of subfile records, data area ORDNXT with `IN *LOCK`/`OUT`) and CONTLST pass the 5250 end-to-end test with `IBMISH_RPG_ENGINE=native` (CI runs it). The three-clerk order entry of the Phase 3 gate works too; the gate's batch jobs print, which is R3.

What R2 adds:
- **Display files** (externally described WORKSTN): the record formats' fields become program fields (numbers zoned); EXFMT, WRITE, READC; subfiles with `SFILE(format:rrn)`: WRITE, CHAIN and UPDATE by relative record number; `INDDS`; response indicators; `*INKA`-`*INKY` from the function key pressed. The display work itself (overlay, paging, validation) is the same code Jariko used (`workstn.rs`, `dspf.rs`).
- **Data areas:** `*DTAARA DEFINE`, the `DTAARA` keyword, `IN` (with `*LOCK`), `OUT`, `IN *DTAARA`.
- **Record locks:** the job's database session is named after the job and waits WAITRCD for a locked record, as with Jariko; positions 73-74 (or `(E)`) catch the error.

**R1 (Oct 19, 2026).** The batch samples (CUSTLIST, ORDTOTAL, OWING, CUSTUPD, the CL program CUSTRPT calling RPG, CONTCNT) produce their golden output on the native engine. CI runs the demo on it (`IBMISH_RPG_ENGINE=native`). Fully free-form programs run too: HELLOWORLD from the corpus prints its message, where Jariko did nothing.

To try it: `IBMISH_RPG_ENGINE=native ibmish call CUSTLIST 3`. With the variable set, CRTBNDRPG checks programs (syntax, names, files) when it creates them.

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
4. **R4, the rest of the job.**
   - Record locks, data areas, `*LDA`, commitment control.
   - CALL and CALLP between programs (RPG and CL), parameters by reference.
   - Procedures, prototypes, `RETURN` values, `EXPORT`.
   - Gate: the Phase 3 gate on the native engine.
5. **R5, real code.**
   - The corpus (`tools/corpus.sh`): free-form programs, data structures (`LIKEDS`, `QUALIFIED`, `DIM`, `OVERLAY`), dates, `MONITOR`, `%BIFs`, `/IF DEFINED`.
   - Gate: the default switches to native.
6. **R6, beyond Jariko.**
   - Embedded SQL (SQLRPGLE) on `sql.rs`.
   - Service programs and modules: CRTRPGMOD, CRTSRVPGM, binding directories.
   - RPG/400 (RPG III) source.

Each milestone ends with its gate passing in CI, and with the gaps found written down in fidelity-gaps.md.
