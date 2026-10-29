# Importing an application

`as4tainr import` brings an application's source and data into a library and builds its objects, the way an operator would with the IBM i create commands:

```sh
as4tainr import path/to/app --lib MYAPP
```

In Docker: `docker compose exec as4tainr as4tainr import /path/in/container --lib MYAPP`. Mount or copy the directory into the container first.

## The directory

The layout is the one used to keep IBM i source in git:

```
app/
  QDDSSRC/CUSTMAST.PF            one folder per source file,
  QDDSSRC/CUSTINQ.DSPF           one stream file per member: MEMBER.TYPE
  QRPGLESRC/CUSTINQ.RPGLE
  QCLSRC/START.CLLE
  includes/header.rpgleinc       include files (any folder)
  data/CUSTMAST.csv              initial records (header row = field names)
```

- **Source files.** A folder named like a source file (`Q...SRC`) puts its members in that file. Other folders, such as project folders, only group files. Their members go to the usual source file for their type: PF, LF and DSPF to QDDSSRC; RPGLE and SQLRPGLE to QRPGLESRC; CLLE and CLP to QCLSRC; SQL scripts (`.sql`, `.table`, `.view`, `.index`) to QSQLSRC; binder language (`.bnd`) to QSRVSRC; binding directory scripts (`.bnddir`) to QBNDDIRSRC.
- **Member names.** Plain names (`CUSTINQ.RPGLE`, any case) and the TOBi convention both work. TOBi is IBM's open-source build tool. Its names look like `CUSTINQ-Customer_inquiry.pgm.rpgle`: the object name, a description that becomes the member text (underscores become blanks), the object type, then the source type.
- **Include files.** `*.rpgleinc` and `*.clleinc` files become stream files under `/<library>/` with their relative path. RPG `/INCLUDE 'header.rpgleinc'` and `/COPY 'x'` find them by name. CL `INCLUDE SRCSTMF('header.clleinc')` does too. RPG `/COPY QRPGLESRC,MEMBER` finds members.
- **Data.** `NAME.csv` loads into the physical file NAME when that file is created by the import, from DDS or by an SQL script. An existing file keeps its records.

## What it does

1. Creates the library if needed (CRTLIB), and each source file (CRTSRCPF). Then it adds every member, replacing a member of the same name. Include files (`.rpgleinc`, `.clleinc`) go to the IFS under `/library/`, and those in a folder named for a source file (`QCPYSRC/NWUTILPR.rpgleinc`) are also members of it, for `/COPY QCPYSRC,NWUTILPR`.
2. Creates the physical files with `CRTPF FILE(LIB/NAME) SRCFILE(LIB/QDDSSRC) SRCMBR(NAME)` and loads their CSV data.
3. Runs the SQL scripts as `RUNSQLSTM ... ERRLVL(30)` would: every statement runs, and the ones that fail are reported. Scripts that create tables run first. Everything goes to the import library: it is the default schema, and `SET SCHEMA` statements are skipped. A script with failing statements counts as an object that could not be created.
4. Creates the logical files (CRTLF), display files (CRTDSPF) and printer files (CRTPRTF). Files that exist are kept.
5. Creates modules, service programs and binding directories (ILE). A source is a module when the repository's `Rules.mk` says so (`NAME.MODULE: source`), when its name says so (`NAME.module.rpgle`), or when it is `NOMAIN`. `NAME.bnd` (binder language, kept in QSRVSRC) makes a service program from the modules its `Rules.mk` rule lists, or from the module of the same name. `NAME.bnddir` lists CL commands (`CRTBNDDIR`, `ADDBNDDIRE`, with `&O` for the library and `&N` for the name, as in IBM's Bob build tool); a command starting with `!` may fail.
6. Creates the programs: CRTBNDCL (CLLE), CRTCLPGM (CLP), CRTBNDRPG (RPGLE), CRTSQLRPGI (SQLRPGLE) and CRTRPGPGM (RPG, that is RPG/400). These are replaced, since they are built from the source just imported. CL programs are checked when they are created. RPG programs are compiled (checked) when they are created, as CRTBNDRPG does.
7. Creates the commands: `CRTCMD CMD(LIB/NAME) PGM(LIB/NAME)`, the command processing program being the program of the same name, as build tools for IBM i name it by default.
8. Reports each object, and the members it cannot build yet (COBOL, for one). Their source is kept.

The import exits with status 1 if any object could not be created. The report says why.

The same commands work on a 5250 command line once the source is in a library, for example `CRTBNDRPG PGM(MYAPP/CUSTINQ)`. `DSPFD MYAPP/QRPGLESRC` lists the members of a source file, and `DSPPFM MYAPP/QRPGLESRC MBR(CUSTINQ)` shows one.

## Running it

RPG programs created with CRTBNDRPG are program objects: CALL finds them through the library list like any other program, and their source comes from the library. `samples/apps/contacts` is a small example that `scripts/samples.sh` imports into CONTACTS:

```
ADDLIBLE CONTACTS
CALL CONTACTS
```

## Testing with real-world source

`tools/corpus.sh` clones a public repository of IBM i source and imports it. The default is Nick Litten's public IBM i examples (github.com/NickLitten/nick.litten.public). That repository has no license file, so it is cloned at test time and never added to this repository. What it showed on Oct 16, 2026:

- **Runs unmodified:** fixed-form AS/400-style subfile programs (SIMPLESFL1, NOOBSFL1) and the CONTLST-style READ loop.
- **Fixed by as4tainr along the way:**
  - comment lines in DDS without the form type;
  - `//` comments in columns 1-5;
  - quoted `/INCLUDE` files;
  - CL `INCLUDE`, DCLPRCOPT and COPYRIGHT;
  - the end-of-file and not-found indicators on READ and CHAIN;
  - `*INKC` and the other function key indicators;
  - a field named CTID (a PostgreSQL system column name);
  - zoned and packed fields with the same name.
- **Not yet:**
  - **Fully free-form RPG (`**FREE`) and free-form declarations.** Jariko did not support them; this is why as4tainr got its own RPG engine, which does (docs/rpg-engine.md).
  - CL pointers and defined storage (`TYPE(*PTR)`, `STG(*DEFINED)`).
  - DDS date and time fields.
  - 27x132 displays.
  - System APIs (QDCXLATE, IFS APIs) and commands such as QSH, CPYTOIMPF and SNDSMTPEMM.

On Oct 21, 2026 the corpus builds 43 programs (50 objects with its 4 modules and 3 service programs) and 6 commands. Most of what still fails would fail on IBM i too:

- SQL scripts that run SELECT in RUNSQLSTM (SIMPLESHOW, SORTSFLPF, WEBSQLSMP, FATQPFRA, FATQPFRB; these two now run everything else, including CREATE TABLE AS over QSYS2.OBJECT_STATISTICS), drop tables that do not exist (FOODFILE, FILEIN, FILEOUT) or name a schema that is not there (LSTLIB).
- Build files and sources that do not match: BIGFATSRV's Rules.mk names a module that is not in the repository, so LOGMESSAGE is missing for SORTSFLBOB, SORTSFLVSC and UPDIASPSQL; members include files (QTXTSRC, IFS paths) that are not in the repository; STOREPRCR and SFLSNIP2 need files (EMPLO00001, CUSTOMER) the repository does not create; LSTLIBSIMP and UPDIASP declare outfiles that only exist after the program has run once.
- Sources that do not compile: a missing parenthesis (IFSREAD), fixed-form specifications or PLIST inside `**FREE` (FREERPG1-3), a comment in factor 2 (SFLSNIP1), RPG III input specifications in an RPG IV member (LEGACYRPG), a prototype without an implementation (PWDEXPILE), LIKEDS of another procedure's local data structure (JSNIFSSQL2), and WRITEIFS2, whose include `modern/ifsio_h.rpgleinc` has no `**FREE` line, so it is column-limited, yet starts its statements in column 7 (columns 6 and 7 must be blank).

What as4tainr still lacks for the rest: DATALINK, ROWID, arrays and generated expression columns (ALLTABLE). JSNIFSSQL1 builds now that the IFS APIs and JSON_TABLE are there (docs/ifs.md, docs/sql.md).

### IBM's company_system sample

`tools/corpus.sh` also takes IBM's own sample application (github.com/IBM/ibmi-company_system: no license file, so it is cloned at test time only). It is laid out for IBM's build tools: TOBi names, a Rules.mk per folder, `.sqlprc` and `.sqlrpgle` members, a binder source next to its module, iRPGUnit tests. On Oct 22, 2026 it builds everything but two objects: the DEPARTMENT and EMPLOYEE tables (with the referential constraints of IBM's sample database), the POPDEPT and POPEMP procedures, the three display files, the EMPDET module and service program, the APP binding directory, and the DEPTS, EMPLOYEES and NEWEMP programs. What it found and as4tainr now does:

- a source of one name twice in a folder (empdet.sqlrpgle and empdet.bnd): the binder source goes to QSRVSRC, where CRTSRVPGM looks for it;
- a Rules.mk target built from a source of another name (TEMPDET.MODULE from empdet.test.sqlrpgle): the member takes the object's name;
- the `.sqlprc`, `.sqludf`, `.sqltrg` and other SQL source extensions; `RESULT SETS n` without DYNAMIC;
- CRTSQLRPGI COMPILEOPT (`BNDDIR(...)`, `DFTACTGRP(...)`);
- FOREIGN KEY with a constraint name after KEY, REFERENCES without columns (the parent's primary key), and column-level `CONSTRAINT x REFERENCES t (c)`;
- RPG `LIKE` of a data structure (a character item of its length) as a prototype's return value;
- many Db2 for i scalar functions and special registers (RAND, INT, VARCHAR, DATE of a number, HEX of a number, LOCATE, TRANSLATE in Db2's argument order, STRIP with its keyword, LISTAGG, CURRENT USER, CURRENT SERVER... docs/sql.md);
- SQL variables named like columns: in an INSERT column list and the targets of UPDATE ... SET they are the columns.

What fails would fail on IBM i too: TEMPDET needs iRPGUnit's `qinclude,TESTCASE`, and MYPGM calls printf without binding QC2LE. CALL POPDEPT stops with SQL0530: it makes up random ADMRDEPT values, which the table's own ROD constraint (ADMRDEPT references DEPTNO) rejects. POPEMP calls SYSTOOLS.HTTP_GET for randomuser.me.

### OSSILE

The third corpus is OSSILE (github.com/OSSILE/OSSILE, MIT): utilities in RPG, CL, C and SQL, each project built by its own Makefile or `setup` shell script. On Oct 23, 2026 the import creates 54 objects (programs, modules, service programs, commands, display files, SQL objects); 17 cannot be created, and those would not build on IBM i either, or need what as4tainr does not have:

- would fail on IBM i too: CALCWEEK, INTTOHEX and INPUTTEST call procedures but are created with DFTACTGRP(*YES) (no bindable APIs or bound calls then); LLIST's binder source has two PGMLVL(*CURRENT) blocks, so LLIST and LUTIL (bound to it) fail; DLTLIBRCV has a blank line inside a continued CALL; SU declares its display file as `DCLF OSSILE/ FILE(SU_DSPF)`; CURPTFGRP runs SELECT in RUNSQLSTM; the examples ALISTEX01, LLISTEX1 and LUTILEX1 are meant to be bound by hand; ALIST01 and LLIST_UT_1 are unit tests for RPGUnit (below);
- not in as4tainr: C (the INIH module and INTEROPR1's C half; OSSILE's other C projects are not built), the QtocLstNetCnn API (NSTATR).

What it found and as4tainr now does: source in Windows-1252 (not UTF-8); service programs as Makefiles and setup scripts build them (`CRTSRVPGM ... MODULE(...) BNDSRVPGM(...)`); copy members named by `/COPY name.rpgle`, `/COPY MEMBER` or a quoted path are include files, not programs (also when the file name is too long for a member); binder language comments that span lines; the QSYSINC library (QUSEC, QWCRNETA); TCP/IP command names such as FTP for the CL compiler's check; and the RPG engine's procedure pointers, CEE APIs, C runtime functions and compiler fixes (docs/rpg-engine.md). CONVCASE, DYNMEMCEE, TMPNAM, SORTARRAY, DYNMEM, COMPTIMARR and the other examples run.

Its unit tests are the application-level check (`tools/legacy.sh`, Oct 29, 2026): with RPGUnit installed, ALIST01 (arraylist) made with RUCRTTST and LLIST_UT_1 (linked list) made as its Makefile makes it, both run by RUCALLTST in a batch job: 19 test cases with 118 assertions and 7 test cases with 47 assertions, all passing. The tests call procedures their libraries' binder sources do not export (arraylist_sublist is not in arraylist.bnd; llist.bnd does not build), so on IBM i they would not bind either; legacy.sh binds them to service programs made with EXPORT(*ALL) (ALISTALL, LLISTALL). They found: a callback called with more arguments than it has parameters (list_foreach passes its user data); float arithmetic and comparisons (a float operand makes the operation float); and that each bound call looked up the service program in the database (100,001 calls in TEST_BIGLIST took 52 seconds, now 2.6 with a release build).

See [fidelity-gaps.md](fidelity-gaps.md).

### iMoq

iMoq (github.com/danlong005/IBMiMock, MIT) is a mocking framework for IBM i: commands (IMOQPGM, IMOQSRVPGM, IMOQWHEN, IMOQVERIFY...) that generate RPG stubs into QTEMP, compile them and stand them in for real programs and service programs, with an engine in SQLRPGLE that activates answer procedures through procedure pointers (QleActBndPgm, QleGetExp, _RSLVSP2/4). It builds itself with a CL program (BUILD: CRTLIB, source files, the repository copied into members by a Qshell loop, then its installer IMOQINST) and tests itself (IMOQTEST: IMOQENG_T and IMOQCMD_T, 32 test cases; IMOQDEMO; EXAMPLES: 23 examples). as4tainr runs BUILD's steps without the Qshell loop (`scratch/imoq.sh` copies each file with CPYFRMSTMF; Qshell would run the repository's shell code on the host). On Oct 29, 2026 it installs, and IMOQTEST passes 31 of its 32 test cases (ANSWER(program) fails) and EXAMPLES 12 of 23. What it found and as4tainr now does is listed in docs/rpg-engine.md ("Found by iMoq"), docs/work-management.md (CHGJOB, RTVJOBA, %SWITCH) and docs/fidelity-gaps.md (CPYSRCF, RTVBNDSRC, includes kept with an object); also DLTMSGF of a message file that is not there is CPF2105, CRTSQLRPGI takes GENLVL and PRTFILE (not applied), an alias of a source member takes UPDATE and DELETE, a command's DFT(QTEMP) is the running job's QTEMP, and CLRPFM empties a source member.

### rpgle-repl

The fourth corpus is rpgle-repl (github.com/tom-writes-code/rpgle-repl, MIT), a read-evaluate-print loop for ILE RPG laid out as a TOBi (makei) project: `iproj.json`, a Rules.mk per folder, SQLRPGLE modules whose prototypes are `.RPGLEINC` files beside them, nine service programs, five programs, three commands, two display files and three tables. On Oct 25, 2026 every object is created, and its batch wrapper runs a snippet end to end: `REPLWRPR LIB(REPL) FIL(SNIPS) MBR(T1)` reads the snippet member, generates an SQLRPGLE source into QTEMP through an SQL alias over a source member, compiles it with CRTSQLRPGI and CRTPGM, runs it, and records each variable's value (`X = 8`) in its results table, whose session column is an auditing column (GENERATED ALWAYS AS (QSYS2.JOB_NAME)).

What it found and as4tainr now does:

- TOBi projects (`iproj.json`): RPG sources compiled from the IFS (SRCSTMF) as makei does, with its defaults COMMIT(*NONE) and DFTACTGRP(*NO) and the project's includePath as INCDIR; a bare `/include name` finds the `name.rpgleinc` next to the source before a member of that name (the IFS search path, docs/fidelity-gaps.md); X.MODULE and X.SRVPGM (or X.PGM) rules of one name; target variables (`X.PGM: private ACTGRP := *NEW`, TEXT); programs bound from modules (`X.PGM: X.MODULE Y.SRVPGM`); service programs built after those they bind; `.CMDSRC` command source;
- SQL: auditing columns and ROW CHANGE TIMESTAMP, RENAME TABLE, CREATE and DROP ALIAS (also over a source member), QSYS2.JOB_NAME, columns named by their system names, SYSTOOLS.SPOOLED_FILE_DATA, data definition through PREPARE/EXECUTE, OVRDBF overrides used by embedded SQL, SQL comments inside embedded SQL;
- RPG: EVAL-CORR, %LOOKUP(arg : dsarr(*).subfield), OPTIONS(*CONVERT); CL: CRTSRCPF MBR, a qualified command checked in its library.

### IBM-i-RPG-Free-CLP-Code (SJLennon)

Scott Lennon's examples of free-form RPG, CL, SQL and display files (github.com/SJLennon/IBM-i-RPG-Free-CLP-Code), imported into one library with `as4tainr import`. On Oct 25, 2026, 34 of its objects are not created (65 before). What it found and as4tainr now does:

- Import: a source whose includes name relative paths (`/include ../Copy_Mbrs/X.RPGLE`) is compiled from the IFS (SRCSTMF), so the paths resolve from the source's folder; commands are built before programs; `SET SCHEMA SESSION` in a script is kept (it names QTEMP, not a library to put objects in);
- SQL: DECLARE GLOBAL TEMPORARY TABLE, DEFAULT USER, ALTER TABLE with several alterations, strings compared with numbers (`WHERE custid = 3` on a CHAR column, `MOD(custid, 2)`);
- Display files: keyboard shift D, usage P; CL: IF and ELSE with the command written positionally.

What still fails and why (each would need something as4tainr does not have, or fails on IBM i too): service programs and programs whose binding directories the README has the user make by hand (CRTBNDDIR/ADDBNDDIRE are not in the repository); programs over the IBM sample file QIWS/QCUSTCDT; SQL functions over RPG procedures (LANGUAGE RPGLE external functions); DEMO_CL2 and DEMO_CL3, which read DSPFD's outfile through the model file QAFDMBRL; scripts for ACS Run SQL Scripts (a SELECT, which RUNSQLSTM refuses) or naming the author's library LENNONS1; DROP TABLE before the table exists (SQL0204, as on IBM i the first time).

### IBMi-topics (MarcoDeSenas)

Marco De Senas' IBM i tools written on PUB400 (github.com/MarcoDeSenas/IBMi-topics-thanks-to-pub400, GPL-3.0: cloned at test time only), laid out with an `iproj.json` and TOBi-style names (`envsav.pgm.clle`, `usrspc.srvpgm.rpgle`, `usp0101.msgid.sql`). On Oct 26, 2026 the import creates every object but two, and its SPLFDLT command runs end to end in a batch job (tools/legacy.sh): a validity checker that makes sure the command analyzer called it (QWVRCSTK through a service program, _MATPGMNM, a data area in QTEMP), user space commands with RTNVAL parameters, QUSLSPL SPLF0300 into a user space, DLTSPLF of each file, completion and escape messages from its own message file.

What it found and as4tainr now does:

- Import: every source of a TOBi project goes to the IFS at its path (include files named `.rpgle` and `.clle` too), CL programs whose INCLUDE SRCSTMF names a relative path are compiled from there (the path is completed with the source's directory, the IFS root is not case-sensitive), a `NAME.srvpgm.rpgle` without binder source becomes a service program exporting all (its installation notes: "Create RPG Module then Create Service Program with EXPORT(*ALL)");
- Commands: the CMD statement's VLDCKR, ALLOW, MAXPOS and TEXT, PARM RTNVAL(*YES), the validity checker called by QSYS/QCATRS; CL: DCLPRCOPT (DFTACTGRP, ACTGRP, BNDSRVPGM, BNDDIR) binds the program, CALLPRC '_MATPGMNM', CL programs on the call stack, RCVMSG RMV(*YES) takes a message out of the job log lines; CPYTOARCF and CPYFRMARCF are IBM i commands to the CL compiler's check;
- SQL: QSYS2.QCMDEXC as a scalar function in SQL procedures (its message scripts add each message description with ADDMSGD and fall back to CHGMSGD when that returns -1), SYSTOOLS.LPRINTF, QSYS2.ENVIRONMENT_VARIABLE_INFO (ENV_VARS, also as an RPG EXTNAME);
- RPG: a subfield of a qualified data structure LIKE a standalone field that is itself LIKE another; a NOMAIN module's PSDS (*PROC: the module) set when CL calls into it; QWVRCSTK leaves the information status blank when the receiver is only the header; display files: relative positions (+n).

What still fails: CMDGRP's script starts with `set schema = ?` (a parameter marker, for ACS's Run SQL Scripts; RUNSQLSTM cannot run it on IBM i either) and uses a system-period temporal table (GENERATED ALWAYS AS ROW BEGIN/END, PERIOD SYSTEM_TIME, ADD VERSIONING), which as4tainr does not have yet; so JOBLOGLST, which declares that file, is not created.

## Legacy applications, installed their own way

`tools/legacy.sh` installs real IBM i applications the way their READMEs say, on an empty database, and reports what works. The repositories are cloned at test time and never added to as4tainr.

- **Northwind for IBM i** (GPL-3.0; github.com/ZodandSons). The NORTHWIND database comes from its own SQL scripts (`RUNSQLSTM SRCSTMF`: CREATE SCHEMA, tables with foreign keys, views, GRANT, and 3,309 INSERTs). The application is then built by its own CL program, NWBUILD, from the folder copied into the IFS (`as4tainr put`): it copies every member with CPYFRMSTMF, sets the source types with CHGPFM, and builds 13 SQLRPGLE service programs (and its RPG utility service program), 14 display files, 13 RPG programs and its CL menu and test runner. On Oct 21, 2026 NWBUILD ends with "Northwind build completed successfully"; only its iRPGUnit tests are skipped (the RPGUNIT library is not there). The menu (`CALL NWAPP/NWMENU`, or a user profile with it as initial program) lists the 91 customers, the 830 orders with their totals, and an order's lines.
- **COM order management** (no license file: test use only; github.com/ARNAV2684). Imported into COM022LIB with `as4tainr import`, then the README's order: the NXTORDNO data area, CRTCOMMSG (its message file), LOADDATA, the COMBNDDIR binding directory, and the programs bound through it. `CALL COM022LIB/COMSTART` runs the green-screen application: menus, customer, item and order maintenance with message subfiles, and an SQL order inquiry. Its SQL trigger (ITMPRCH) logs price changes.

Then SETUPJRN journals its files and ADDTRG attaches its ORDTRG trigger program: changing an order writes an ORDAUD row through the trigger program, and the ORDAUDJV view (over QSYS2.DISPLAY_JOURNAL) shows the same change from the journal (docs/journals.md). What still fails would fail on IBM i too: SELECT statements in RUNSQLSTM scripts (JRNQRY, PRACTICE1).

- **RPGUnit** (CPL-1.0; github.com/RPGUnit/RPGUnit), the unit testing framework for ILE RPG, installed and tested the way its own scripts do: its members copied into the source files of library RPGUNIT, MKMETADATA (CHGPFM of every member), MKRPGUNIT (the RUTESTCASE and RURUNNER service programs, the RUCMDRUN and RUCRTTST programs, the RUCRTTST and RUCALLTST commands with their UIM help panel group), then its self-test MKRPGUNITT, in a batch job (`as4tainr batch`; its acceptance tests read RPGUnit's report from the spooled file, and PASE `system` keeps none). On Oct 23, 2026 the whole self-test passes: the sanity check (TESTPGM03 must fail and does), every unit test suite MKUT runs (ASSERTT, CALLPROCT, EXTPRCT, EXTTSTT, JOBLOGT, PGMMSGT, RUNT, CMDRUNT, CMDRUNLOGT, RUCRTTSTT), and all 22 acceptance tests (RUACPTST: 185 assertions). The EXTPRCT fixtures SRVPGM1 and SRVPGM2 are not built by RPGUnit's scripts; tools/legacy.sh builds them first.

  What it found and as4tainr now does: the call stack (QWVRCSTK, QCMDEXC as an entry of its own), program messages across procedures (QMHSNDPM *ESCAPE ends the entries up to its target, whose MONITOR takes it; QMHRCVPM RCVM0300 with the sender's statement; QMHRSNEM; an exception message's sender is the procedure where it happened), the job log list API (QGYOLJBL), the spooled file APIs (QGYOLSPL, QGYGTLE, QSPOPNSP, QSPGETSP, QSPCLOSP), OVRPRTF SPLFNAME and USRDTA, printer files that stay open in a module between calls, QWCRTVCA, QLICVTTP, rslvsp, QleActBndPgm and QleGetExp, QBNLSPGM, CRTPNLGRP, OVRPRTF without OUTQ, overrides scoped to an activation group, procedure pointers to a service program's procedures (also those it does not export) and calls back into a running service program, procedures returning LIKEDS DIM, whole arrays passed to array parameters, group subfields, CONST parameters by reference, %INT of blanks failing, SQL timestamps into RPG timestamp host variables.

- **QshOni** (MIT; github.com/richardschoen/QshOni), the Qshell and PASE utility commands for CL (QSHEXEC, QSHBASH, QSHCURL, QSHPYRUN...), built by its own `build.sh`: the clone is put into the IFS at the same path (`as4tainr put`), and the script runs in bash with its `system` calls run by `as4tainr system`. It creates library QSHONI and source file SOURCE (CCSID 37), copies every member with CPYFRMSTMF and CHGPFM, and runs its CL program SRCBLDC, which creates 42 commands, about 80 CL and RPG programs, a service program, a binding directory, panel groups, message file QSHMSG, data areas and directories. On Oct 23, 2026 the build ends with "QShell on IBM i library QSHONI was created and programs compiled", and `QSHEXEC CMDLINE('echo ...')` runs the command through QSH (with AS4TAINR_QSH=1), reads QSH0005's exit status and copies the output into its outfile.
- **Company System** (Apache-2.0; github.com/IBM/ibmi-company_system), IBM's demo application for its build tools: SQL tables, SQL procedures, display files, SQLRPGLE programs, a module bound into a service program with binder source in a stream file, and a binding directory. legacy.sh runs the commands its GNU makefile runs, one by one, from the clone put into the IFS at the same path (relative SRCSTMF paths start in the job's current directory) with the current library and library list its `liblist -c`/`liblist -a` set (`AS4TAINR_CURLIB`, `AS4TAINR_LIBL`). On Oct 24, 2026 every object is created and MYPGM writes its text with printf and DSPLY. POPDEPT fails as it would on IBM i (its random ADMRDEPT values break the DEPARTMENT table's own referential constraint, SQL0530) and POPEMP needs the network (SYSTOOLS.HTTP_GET to randomuser.me); its unit test runs once iRPGUnit is installed (below): on Oct 29, 2026 RUCRTRPG makes EMPDETT from qtestsrc/empdet.test.sqlrpgle with the options of its testing.json, binding it to EMPDET through its ctl-opt BNDDIR('APP') (a module keeps the binding directories it names), and RUCALLTST passes its 4 test cases (9 assertions; the test copies EMPLOYEE and DEPARTMENT into QTEMP with CRTDUPOBJ, overrides them with OVRDBF OVRSCOPE(*JOB) through QSYS2.QCMDEXC, and inserts its own rows).
- **iRPGUnit** (CPL-1.0; github.com/tools-400/irpgunit), the successor of RPGUnit: its members copied into the source files of library IRPGUNIT (RPGUnit has RPGUNIT), then its REXX procedure A_INSTALL builds it (`STRREXPRC SRCMBR(A_INSTALL) SRCFILE(IRPGUNIT/QBUILD) PARM('INSTALL IRPGUNIT')`, as its README says: REXX procedures compiling every module with its own CMPMOD command, the RUTESTCASE and RUMEMMGR service programs, the RUCRTRPG, RUCRTCBL and RUCALLTST commands and programs). As its A_SELFTEST does, legacy.sh then compiles the modules again (A_INSTALL CMPMOD) and makes the self-test suite ASSERTT with RUCRTRPG and runs it with RUCALLTST. On Oct 24, 2026 iRPGUnit installs, and 19 of ASSERTT's 22 test procedures pass (186 assertions); docs/fidelity-gaps.md (iRPGUnit) has the other three.

  What it found and as4tainr now does: environment variables (ADDENVVAR, CHGENVVAR, RMVENVVAR, WRKENVVAR, getenv, putenv), QSH and STRQSH with QIBM_QSH_CMD_OUTPUT and QIBM_QSH_CMD_ESCAPE_MSG (docs/fidelity-gaps.md: the command runs in the container), source physical files read and updated by record (SRCSEQ, SRCDAT, SRCDTA), CPYFRMSTMF into a program-described database file and its old STMFCODPAG keyword, CHGLIB, `LIB/*ALL` in CL source (a qualified name, not a comment), expressions as command parameter values (`TEXT(&LIB |> 'version')`), selective prompting characters, CALL QCMDEXC from CL, the function check CPF9999 for an escape message only MONMSG CPF9999 or CPF0000 can take, SNDPGMMSG MSGTYPE(&var), %LEN of a constant in a named constant, program-described input fields of a file that is not there at compile time, the same DCL twice, a parameter in apostrophes keeping its case under CASE(*MONO), a command processing program's completion message received once by its caller, CRTCLPGM/CRTSRVPGM/CRTPGM/CRTRPGPGM TGTRLS, CRTSRCPF and CRTPF CCSID, CRTBNDCL DFTACTGRP and ACTGRP, ADDBNDDIRE POSITION, QSYS2.NETSTAT_JOB_INFO, and the DSPATR command name. A free-form statement that starts with an operation code and a parenthesis is now that operation (a procedure named like one is called with CALLP), as on IBM i.
