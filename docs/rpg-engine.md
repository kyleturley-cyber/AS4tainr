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

**Pointers, the C runtime and the IFS (Oct 21, 2026).** Pointers hold addresses (`mem.rs`, `ptrs.rs`): %ADDR (of a variable, an array element, a data structure, or a varying field's data with `*DATA`), BASED on standalone fields and data structures (a basing pointer that is not defined is defined implicitly), pointer arithmetic (`p + n`, `p - n`, `p - q`), comparisons and *NULL, heap storage (%ALLOC, %REALLOC, ALLOC, REALLOC, DEALLOC with N), %STR to read and set null-terminated strings, and OPTIONS(*STRING) on pointer parameters. Storage used through a pointer that addresses nothing, or outside what it addresses, is status 00222 (MCH3601, MCH0601). A bound call lends the caller's storage and the heap to the called module, so its parameters by reference are the caller's own storage and pointers to it stay valid. The C runtime and UNIX-type APIs a program binds from QC2LE (`crt.rs`): open, read, write, close, lseek, unlink, mkdir, rmdir, access, rename, __errno, strerror, perror, strlen, strcpy, strncpy, strcat, strcmp, strncmp, memcpy, memmove, memset, memcmp, atoi, atol, system and Qp0zLprintf, over the integrated file system (docs/ifs.md). Tests: `rpg_pointers_and_based_storage`, `rpg_c_runtime_and_ifs`.

**R6 has started: embedded SQL (SQLRPGLE) runs (Oct 19, 2026).** `EXEC SQL` statements, in free form and between `C/EXEC SQL` and `C/END-EXEC` in fixed form, run on the SQL layer (docs/sql.md) as the restricted SQL login, with the program's library list as the search path. CRTSQLRPGI (`OBJTYPE(*PGM)`) creates the program, and `as4tainr import` builds SQLRPGLE members with it. What runs:
- **The SQLCA:** `SQLCODE`/`SQLCOD`, `SQLSTATE`/`SQLSTT`, `SQLERRMC`, `SQLERRD` (`SQLERRD(3)`: rows changed or fetched), set after every statement. SQL errors do not end the program; it looks at SQLCODE (+100 for no row, negative numbers for errors, as in the Db2 for i SQL messages).
- **Host variables:** `:name`, qualified subfields (`:ds.field`), a data structure as a host structure (its subfields in order), null indicators (`:var :ind`, `:var INDICATOR :ind`; -1 for null), `SQLTYPE(CLOB : n)` variables (`name_LEN`, `name_DATA`).
- **Statements:** `SELECT ... INTO`, `VALUES ... INTO`, `SET :var = expression`, INSERT, UPDATE and DELETE (+100 when no row changes), `DECLARE CURSOR` for a SELECT or a prepared statement, `OPEN` (with `USING`), `FETCH` (NEXT, PRIOR, FIRST, LAST, RELATIVE, ABSOLUTE) `INTO`, `CLOSE`, `PREPARE`, `EXECUTE` (with `USING`), `EXECUTE IMMEDIATE`, `SET OPTION`, `WHENEVER` and `INCLUDE SQLCA` (accepted), `COMMIT` and `ROLLBACK` (of the commitment definition when the program runs with a commit level: CRTSQLRPGI COMMIT, default *CHG, or SET OPTION COMMIT; SQL starts commitment control when there is none), `CALL` of SQL procedures (OUT and INOUT parameters into host variables; a SIGNAL sets SQLCODE -438 and its SQLSTATE).
- Not yet: see fidelity-gaps.md (multi-row FETCH, positioned UPDATE/DELETE, GET DIAGNOSTICS, result sets of called procedures).
- **Commitment control:** a file with the `COMMIT` keyword (or `COMMIT(indicator)`, when the indicator is on at open) is opened under the commitment definition of the program's activation group or job (STRCMTCTL); without one the open fails with CPF4326 (status 01216). Its writes, updates and deletes are pending until `COMMIT` or `ROLBK` (status 00802 when commitment control is not active). See docs/sql.md.
- The test: `rpg_embedded_sql` (rla_test.rs) runs free-form and fixed-form programs against the sample customer file.

Then SQL data definition (docs/sql.md): CREATE TABLE, CREATE INDEX, DROP, LABEL ON, SET SCHEMA and RUNSQLSTM, so the corpus's SQL-defined files exist. Files now have binary (B), date (L), time (T) and timestamp (Z) fields, variable-length (VARLEN) and null-capable (ALWNULL) fields; a program sees them as binary, date, time, timestamp and varying fields in *ISO form, and date, time and timestamp literals (`D'2026-10-19'`, `T'13.45.00'`, `Z'...'`) set them. Data types (`sql_float_binary_graphic_lob`): float (`float(8)`, F specs, %EDITFLT, %FLOAT; DSPLY shows +1.230000000000000E+002), UCS-2 and graphic (`ucs2(n)`, `graph(n)`, C and G specs), 8-byte BINDEC, EXTBININT(*YES); externally described float, hexadecimal and graphic fields; large objects are left out of a program's record. Null values follow ALWNULL (control keyword, or the CRTBNDRPG parameter): *NO, the default, makes reading a record with a null field a data mapping error (CPF5035, caught by the E extender); *INPUTONLY reads the default value; *USRCTL gives `%NULLIND(field)`, which can also be set to write the null value. Views, identity columns and ALTER TABLE (`sql_views_identity_and_alter`): a program reads an SQL view like a logical file and gets generated identity values on WRITE. SQL names longer than system names: the program uses the system names, or the SQL names with `ALIAS` on the file or on an `EXTNAME` data structure (`sql_long_names`). The test: `sql_tables_indexes_and_runsqlstm` (rla_test.rs).

Also from the corpus: `*ALLX'..'` figurative constants; prototypes and procedure interfaces may come before the definitions they use (`LIKEDS` of a later data structure); `%SIZE` and `%ELEM` in definition keywords (`INZ(%size(ds.field))`); a procedure's local data structure can have a subfield `LIKEDS` a global template. Corpus status (R6): 37 programs created (23 before R6; three sources that were built as programs are modules, per the repository's Rules.mk); 9 of its SQL scripts run without errors. Most of the others need other SQL data types (GRAPHIC, CLOB, FLOAT, ...), compound statements, command definitions (CMD), CL `DCL ... STG(*DEFINED) DEFVAR`, files or includes missing from the repository, or are snippets that do not compile on IBM i either.

**System objects (Oct 20, 2026).** Externally described printer files: CRTPRTF from DDS, `WRITE` of their record formats (`printer.rs`, layout in `core/src/prtf.rs`), the page size and overflow line of the printer file object also for program-described files; `READ` of a display file record (by format, or by the file's name) after `WRITE`; commands made with CRTCMD calling RPG or CL programs (docs/commands.md); CL `STG(*DEFINED)`, `STG(*BASED)` and pointers. Tests: `printer_files_crtprtf_and_rpg`, `rpg_read_of_a_display_record`, `user_commands_crtcmd`, `cl_defined_and_based_storage`.

**Exceptions, status data structures, the local data area (Oct 20, 2026).** What a program sees when something goes wrong (`errors.rs`), and the rest of the runtime gaps:
- **Status codes** (ILE RPG Reference, "Program Status Codes", "File Status Codes"): each error has its status: 00102 divide by zero, 00121 array index, 00907 decimal data, 00202 a called program failed, 00211 a program or procedure that cannot be called; for files 00011 end of file, 00012 no record found, 01021 duplicate key, 01211 file not open, 01218 record in use, 01221 update without a read, 01299 other I/O errors. `%STATUS` (with or without a file), `ON-ERROR` by code, `*PROGRAM` (00100-00999), `*FILE` (01000-09999) and `*ALL`. The E extender resets `%ERROR` and `%STATUS` before the operation; `CALLP(E)`; error indicators on IN, OUT and CALLP.
- **The program status data structure** (S in position 23, `PSDS`): `*PROC`, `*STATUS`, `*ROUTINE`, `*PARMS` and the positions of the IBM layout (previous status, statement, exception ID, message, library, last file in error and its status, job, dates, program, module, user).
- **File information data structures** (`INFDS`): `*FILE`, `*STATUS`, `*OPCODE`, `*ROUTINE`, `*RECORD`, the open, end-of-file and message positions, open feedback (file, library, record and key length, member, records, access type), I/O feedback (counts, record format, record length) and device feedback (relative record number, printer line and page, attention identifier), after every operation.
- **`*PSSR` and `INFSR`:** an error that no operation or MONITOR group takes goes to the procedure's `*PSSR` (a file error in the main procedure to the file's INFSR). GOTO from the subroutine goes back into the calculations (also from any subroutine to a TAG in the detail or total calculations), RETURN returns, and ENDSR's return point (`*DETL`, `*GETIN`, `*TOTC`, `*TOTL`, `*OFL`, `*DETC`, `*CANCL`) says where the cycle goes on; the return point field is blank after ENDSR. ENDSR with none: the default handler (the program ends abnormally; a subprocedure fails).
- **The cycle** follows IBM's detailed cycle step by step (`cycle.rs`, `Phase`): overflow indicators go off at the start of a cycle unless detail calculations or output set them on; LR set in the calculations sets L1-L9 on for the last totals; the overflow routine prints the lines conditioned by the overflow indicator after total output, once each time overflow occurs, and **fetch overflow** (F in position 18) prints them before the line that fetches them.
- **The local data area:** `*LDA`, 1024 characters per job (`as4tainr.local_data_areas`), blanks when an interactive job starts, a copy of the submitting job's for SBMJOB; `CHGDTAARA`, `RTVDTAARA` and `DSPDTAARA` with `DTAARA(*LDA)`. In RPG: a data area data structure (U in position 23, `DTAARA(*AUTO)`) is read (and locked, for a data area object) when the program starts and written when it ends with LR; without a name it is the local data area; `IN`/`OUT` of `*LDA`.
- **Files in procedures:** `DCL-F` (or F specs) inside a procedure: opened when it is called and closed when it returns, unless `STATIC`; they have no program fields of their own, so their records go through data structures (`LIKEREC`).
- Tests: `rpg_status_data_structures_and_status_codes`, `rpg_pssr_infsr_and_return_points`, `rpg_files_in_procedures`, `rpg_overflow_and_fetch_overflow`, `rpg_local_data_area` (rla_test.rs); what is not done is in fidelity-gaps.md.

**ILE activation (Oct 20, 2026).** Activation groups (ILE Concepts): a program runs in the default activation group (DFTACTGRP(*YES), the CRTBNDRPG default), in QILE (DFTACTGRP(*NO) with ACTGRP(*STGMDL); CRTPGM's *ENTMOD), in a named group, in a new one each call (*NEW), or in its caller's (*CALLER); a service program in its caller's or its named group. A program that ends without LR (RETURN, RT, or a linear main procedure) stays active: the next call in the job finds its storage, indicators and open files, without *INZSR or first-cycle output, until RCLACTGRP (named groups, *ELIGIBLE) or RCLRSC (the default group) ends it. Signatures: a program records the signature each service program had when it was bound; a call after the service program changed signature ends with MCH4431, unless a PGMLVL(*PRV) block keeps the old one; UPDPGM and UPDSRVPGM bind again (MODULE(*NONE), or replacing modules). Exported data (EXPORT, IMPORT on definitions). CL modules (CRTCLMOD) bound with RPG modules call them with CALLPRC. DSPPGM and DSPSRVPGM show modules, service programs, signatures and activation groups. Tests: `ile_activation_groups_and_signatures`, `ile_exported_data`, `ile_cl_module_callprc`.

**Free form, to 7.5 (Oct 20, 2026).** The built-in functions (`bifs.rs`, `dates.rs`): dates, times and timestamps (%DATE, %TIME, %TIMESTAMP, %DIFF, %SUBDT, %YEARS ... %MSECONDS, arithmetic with durations, %CHAR and %DEC in the external formats *ISO *USA *EUR *JIS *MDY *DMY *YMD *JUL *CYMD *CMDY *CDMY *LONGJUL and *HMS, TEST(D T Z)), arrays (%LOOKUP and %TLOOKUP with LT LE GE GT, %XFOOT, %SUBARR, %MAXARR, %MINARR), strings (%SCANR, %SCANRPL, %LEFT, %RIGHT, %CONCAT, %CONCATARR, %CHARCOUNT, %EDITW, %XLATE with a start position), numbers (%SQRT, %UNS, %UNSH, %DECPOS, %MAX, %MIN, %BITAND, %BITOR, %BITXOR, %BITNOT), state (%STATUS, %OPEN, %OCCUR, %PARMNUM, %PASSED, %OMITTED, %PROC, %SHTDN), and the 7.x language: FOR-EACH, IN with %RANGE, %LIST and arrays, %SPLIT, DCL-ENUM, DIM(*AUTO) and DIM(*VAR) with *NEXT and %ELEM, OVERLOAD, SND-MSG and %MSG, ON-EXCP, SELECT with an operand (WHEN-IS, WHEN-IN). Data structure I/O: a result data structure on CHAIN and the reads (LIKEREC), on WRITE and UPDATE; %KDS; UPDATE %FIELDS. Tests: `rpg_free_form_7x_features_and_builtins`, `rpg_data_structure_io_kds_and_fields`.

**Fixed form to free form.** CVTRPGFREE (as4tainr's own command; IBM i has none) writes a member converted to free form (`tofree.rs`): H, F, D and P specifications become CTL-OPT, DCL-F, DCL-S/C/DS/PR/PI and DCL-PROC; calculations become free-form statements, with conditioning and resulting indicators written out (IF *INxx; *INxx = %FOUND...). What has no free form, or would behave differently, stays fixed in the same member (columns 8-80 free, the rest fixed); with nothing left fixed the member is `**FREE`. The test `rpg_fixed_to_free_conversion_runs_the_same` runs programs before and after conversion and compares their output.

**RPG III and the full cycle (Oct 20, 2026).** CRTRPGPGM creates programs from RPG/400 source (QRPGSRC, type RPG): the source is converted to RPG IV as IBM's conversion aid CVTRPGSRC does (`core/src/rpg/rpg3.rs`; E specs become D specs with DIM, CTDATA, PERRCD, FROMFILE, ALT; I-spec data structures and named constants become D specs; L specs become FORMLEN/FORMOFL; several conditioning indicators become AN lines; the RPG III operation names, position 53 and `ARR,I` change) and then compiled. CVTRPGSRC writes the RPGLE member. For the programs RPG III code is made of, the engine gained:
- **Fixed-form operations** (`ops.rs`): SCAN, CHECK, CHECKR, SUBST, XLATE, CAT (`:n` blanks), LOOKUP (arrays with a starting index; tables with their alternate table), MOVEA, XFOOT, SQRT, SORTA, OCCUR (multiple-occurrence data structures, OCCURS), TESTN, TESTZ, TESTB, BITON, BITOFF, the move-zone operations, CASxx groups, `*LIKE DEFINE`, AN/OR lines of conditioning indicators, LIKE with a length change. Test: `rpg_fixed_form_string_array_and_bit_operations`.
- **Edit words** on O specs (body, status, expansion; zero suppression with 0 and *, fixed and floating currency symbol).
- **The cycle** (`cycle.rs`): record identifying indicators, control levels L1-L9 (total calculations and output before the new record's fields are moved in; the first record sets the levels on without total time; LR sets them all on), primary and secondary files with matching fields M1-M9 and MR, field indicators; total calculations run by their level. Program-described database files: input specifications with record identification codes and fields by position (zoned, packed, binary), output specifications with ADD and DEL that write record images; O specs that update or add records of externally described files. Tests: `rpg3_crtrpgpgm_and_cvtrpgsrc`, `rpg_cycle_control_levels_and_program_described_files`, `rpg_cycle_matching_records`.

**Service programs and modules (Oct 20, 2026).** CRTRPGMOD creates a module, CRTSRVPGM binds modules into a service program, and CRTPGM binds modules and service programs into a program. CRTBNDRPG binds too: `BNDDIR` (the parameter or `ctl-opt bnddir(...)`) names binding directories, and `DFTACTGRP(*NO)` is needed for that, as on IBM i. A procedure call whose prototype names no local procedure is an import: the binder looks for it in the program's modules, then in the service programs and binding directory entries, and reports CPD5D02 (definition not found) or CPD5D03 (defined more than once) at create time. A service program's exports are the ones its binder language lists (`STRPGMEXP`/`EXPORT SYMBOL(...)`/`ENDPGMEXP`), or every exported procedure with `EXPORT(*ALL)`. A bound call passes parameters by reference (the called module works on the caller's storage, lent to it for the call) and returns the procedure's value. A program's bound modules are copied when it is created; service programs are bound by reference, so recreating one changes its callers. A service program's global storage lives as long as the job's activation of it: values set by one call are there at the next, until RCLACTGRP. The test: `ile_modules_service_programs_bnddir` (rla_test.rs). In the corpus, `Rules.mk` says which sources are modules and service programs (docs/import.md): 4 modules, 3 service programs and 2 binding directories are built, and CONVERT4 binds through the CONVSRV binding directory.

**R5 is done (Oct 19, 2026): the native engine is the only RPG engine.** It passed everything Jariko passed and more: the samples and demos, the 5250 end-to-end test (130 checks, two more than Jariko: DSPLY with a reply), the Phase 3 gate and the browser terminal test. Then Jariko, the JVM runner, the JSON bridge and its conformance runner were removed: as4tainr is one Rust binary, and the container image has no JRE. The bridge's integration tests became RPG programs run on the engine (display files, subfiles, data area locks, spooled files, authority) or tests of the record-level access layer (record locks).

What R5 added, from running the corpus (Nick Litten's examples, `tools/corpus.sh`):
- **DSPLY with a response** in an interactive job: a Display Program Messages screen with the message and a reply line; the reply goes into the response field (lower case kept). In batch the response stays as it was.
- **System programs** RPG programs call: `QCMDEXC` (runs a CL command in the job), `QDCXLATE` (EBCDIC/ASCII translation with QTCPASC and QTCPEBC), `QMHSNDPM` and `QMHRMVPM` (program messages, for message subfiles; ERRC0100 error codes), and the C runtime's `Qp0zLprintf` (printf-style text to the job log).
- **CALL of a CL program** from RPG, in the same job (parameters by reference: the values the CL program leaves in them come back).
- A program that calls itself is refused: RPG programs are not recursive (procedures are).
- One-line empty interfaces (`dcl-pi *n int(10) end-pi;`).

- **Nested data structures:** a subfield defined with `LIKEDS` is a data structure inside the other one (`list.users(i).name`), also through a `LIKEDS` copy of the whole structure and as a parameter; `ds(i).field` for data structure arrays.

**R4 is done (Oct 19, 2026).** Programs call each other and are built from procedures:
- **Procedures** (`DCL-PROC`, P specs) with their own local storage on each call, `STATIC` locals, parameters by reference, `CONST` and `VALUE`, `LIKEDS` parameters, `OPTIONS(*NOPASS : *OMIT)`, `%PARMS`, return values in the declared type, recursion, subroutines inside procedures, `ON-EXIT`.
- **Prototypes** (`DCL-PR`, PR specs): calls to procedures (`EXTPROC`, or the prototype's name) and to programs (`EXTPGM`).
- **Program calls:** `CALL` with `PARM` (factor 2 moved in first; error indicator in 73-74) and `CALLP` through an `EXTPGM` prototype. The called RPG program comes from the library list (CRTBNDRPG) or `AS4TAINR_PGM_PATH`; its parameters are passed by reference (`*ENTRY PLIST`, or a procedure interface outside procedures). Its DSPLY and printed output go to the caller's job.
- **`ctl-opt main(name)`:** the program runs that procedure (no cycle); its interface is the program's parameter list.
- The tests: `native_rpg_engine_runs_a_program` (rla_test.rs) calls programs with CALL and with prototypes, runs procedures with every kind of parameter, STATIC and automatic locals, recursion and ON-EXIT.

**R3 (Oct 19, 2026).** The native engine prints: CUSTPRT matches its golden output, and with `AS4TAINR_RPG_ENGINE=native` the whole 5250 end-to-end test (128 checks) and the Phase 3 gate pass (three clerks entering orders while batch jobs print). CI runs both on the native engine.

What R3 adds:
- **Printer files** (program-described, `PRINTER`) and **O specs**: heading, detail, total and exception records; conditioning indicators with AND/OR lines; space and skip before and after; field end positions and `+n` placement; constants; edit codes; `PAGE`, `UDATE`, `*DATE`; blank after; `OFLIND` (the overflow indicator comes on when line 60 of 66 is printed on or passed). `EXCEPT` with and without a name. Lines go to the job's spooled file (`WRKSPLF`); without a job (`as4tainr call`) they are printed to standard output.
- **The cycle:** first-cycle output with `1P`, a primary file (`IP`) read once per cycle with LR at end of file, total output, detail output after the detail calculations; the overflow indicator goes off at the start of the next cycle.
- **Edit code Y** for six and eight digits with the leftmost zero suppressed; codes that do not blank zero show `0` for a zero integer.

**R2 (Oct 19, 2026).** Interactive programs run on the native engine: CUSTINQ, WRKCUST (load-all subfile, READC, SFLNXTCHG), CUSTMNT (record locks: a second job waits WAITRCD, then gets the error indicator), ORDENT (input subfile, CHAIN/UPDATE of subfile records, data area ORDNXT with `IN *LOCK`/`OUT`) and CONTLST pass the 5250 end-to-end test with `AS4TAINR_RPG_ENGINE=native` (CI runs it). The three-clerk order entry of the Phase 3 gate works too; the gate's batch jobs print, which is R3.

What R2 adds:
- **Display files** (externally described WORKSTN): the record formats' fields become program fields (numbers zoned); EXFMT, WRITE, READC; subfiles with `SFILE(format:rrn)`: WRITE, CHAIN and UPDATE by relative record number; `INDDS`; response indicators; `*INKA`-`*INKY` from the function key pressed. The display work itself (overlay, paging, validation) is the same code Jariko used (`workstn.rs`, `dspf.rs`).
- **Externally described data structures:** EXTNAME with PREFIX (`PREFIX(N_)`, `PREFIX(X:2)` in place of the first two characters), as trigger programs use for the record images; an entry parameter is the caller's storage, so a pointer from %ADDR reaches past its declared length (the trigger buffer). OPEN and CLOSE of USROPN files. Trigger programs for a record change run in the program's thread, with their record changes in the database session of the change (docs/journals.md).
- **Data areas:** `*DTAARA DEFINE`, the `DTAARA` keyword (`DTAARA(*VAR : field)`: the name the field holds when the program runs), `IN` (with `*LOCK`), `OUT`, `IN *DTAARA`. `INZ(*USER)`: the job's user profile.
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
4. **R4, the rest of the job.** Done, with commitment control (see fidelity-gaps.md).
   - Commitment control (record locks and data areas came with R2; `*LDA` with the runtime gaps).
   - CALL and CALLP between programs (RPG and CL), parameters by reference.
   - Procedures, prototypes, `RETURN` values, `EXPORT`, `ctl-opt main`.
   - Gate: programs that call each other with parameters, and procedure-based programs from the corpus (HELLOADV). (The Phase 3 gate, first planned for R4, already passes on the native engine since R3.)
5. **R5, real code.**
   - The corpus (`tools/corpus.sh`): free-form programs, data structures (`LIKEDS`, `QUALIFIED`, `DIM`, `OVERLAY`), dates, `MONITOR`, `%BIFs`, `/IF DEFINED`.
   - Gate: the default switches to native, then Jariko and the JVM are removed (done).
6. **R6, beyond.**
   - Embedded SQL (SQLRPGLE) on `sql.rs` (done, see Status).
   - Service programs and modules: CRTRPGMOD, CRTSRVPGM, CRTPGM, binding directories (done, see Status).
   - RPG/400 (RPG III) source: CRTRPGPGM, CVTRPGSRC (done, see Status).

Each milestone ends with its gate passing in CI, and with the gaps found written down in fidelity-gaps.md.
