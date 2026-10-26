# CL programs

CL programs (CLLE) are the glue of most IBM i applications: they set up the library list, check that objects exist, monitor for errors and call the RPG programs. as4tainr checks a CL program when it is created and runs it in the job that calls it.

```sh
as4tainr crtbndcl QGPL/CUSTRPT --src samples/qclsrc/CUSTRPT.clle --text 'Customer report driver'
as4tainr call CUSTRPT L          # or, on the 5250 command line: CALL CUSTRPT PARM(L)
```

`crtbndcl` reports errors with their line numbers and does not create a program that has any. It replaces an existing program of the same name, as `REPLACE(*YES)` does. `CALL` finds a CL program through the library list; a name that is not a CL program runs on the RPG engine.

## Supported

Written from IBM's public CL reference.

| Area | Supported |
| --- | --- |
| Source | `PGM PARM(...)` / `ENDPGM`, labels `NAME:`, comments `/* */`, continuation lines ending in `+` (next line's leading blanks dropped) or `-` (kept), keyword and positional parameters |
| Variables | `DCL` with `TYPE(*CHAR \| *DEC \| *LGL \| *INT \| *PTR)`, `LEN`, `VALUE`; defaults *CHAR 32, *DEC (15 5). *CHAR values are padded or truncated to their length; *DEC results are truncated to the variable's decimal positions and must fit its digits (`MCH1210` otherwise). Decimal arithmetic is exact. `STG(*DEFINED) DEFVAR(&var start)`: a variable over part of a *CHAR variable's storage (a *DEC one packed, *INT binary); `STG(*BASED) BASPTR(&ptr)`: a variable where a pointer points; `ADDRESS(&var offset)`, `%ADDRESS`, `%OFFSET` (also as a CHGVAR target) set and move pointers. |
| Expressions | `+ - * /`, `*CAT *BCAT *TCAT` (`\|\| \|> \|<`), `*EQ *NE *GT *LT *GE *LE *NG *NL` (and `= ¬= <> > < >= <=`), `*AND *OR *NOT` (and `& \| ¬`), with IBM's precedence; character comparisons pad the shorter value with blanks. `*ON`, `*OFF`, `*BLANKS`, other special values as character constants (`&OPT *EQ *YES`), hexadecimal constants `X'25'`. |
| Built-ins | `%SST`/`%SUBSTRING` (also as a CHGVAR target), `%TRIM`, `%TRIML`, `%TRIMR` (with characters to trim), `%CHAR`, `%DEC`, `%INT`, `%SCAN`, `%CHECK`, `%CHECKR`, `%UPPER`, `%LOWER`, `%LEN`, `%SIZE`, `%PARMS`, `%BIN`/`%BINARY` (also as a CHGVAR target), `%ADDRESS`, `%OFFSET` |
| Files | `DCLF FILE RCDFMT OPNID` (up to five files; a database file, physical or logical, or a display file): a CL variable for each field, `&OPNID_FIELD` with an open identifier, numbers as *DEC; a display file also gives `&IN01`-`&IN99` (*LGL). `RCVF` reads the next record of a database file in its access path order, `CPF0864` at the end of the file (the variables keep the last record). `SNDF`, `SNDRCVF` and `RCVF` show a display file's record formats with the variables and take back what the operator typed and the response indicators. `CLOSE` lets the next RCVF start again at the first record. The file must exist when the program is created. |
| Overrides | `OVRDBF FILE TOFILE LVLCHK OVRSCOPE`, `OVRPRTF FILE TOFILE PAGESIZE OVRFLW`, `DLTOVR FILE LVL`: an override belongs to the call level that made it and ends when that program returns (OVRSCOPE(*JOB): when the job ends or DLTOVR LVL(*JOB)); the highest call level's override of a file counts. RPG programs called after it open the overriding file; a CL file is opened at its first RCVF, so an override made before then counts. A file of another record format: `CPF4131` unless LVLCHK(*NO). |
| Control | `IF COND THEN` / `ELSE CMD` (positionally too: `IF (&A = 1) GOTO X`, the rest of the statement being the command), `DO` / `ENDDO`, `DOWHILE`, `DOUNTIL`, `DOFOR VAR FROM TO BY`, `LEAVE`, `ITERATE`, `SELECT` / `WHEN` / `OTHERWISE` / `ENDSELECT`, `GOTO CMDLBL`, `RETURN` |
| Errors | `MONMSG MSGID EXEC` after a command, or program-level right after the declarations (tried after the command-level ones; when its EXEC is not a GOTO or RETURN, the program goes on after the command that failed). Generic IDs: `CPF0000` all CPF messages, `CPF9800` all CPF98xx. An unmonitored error ends the program and names the line. |
| Messages | Every CL program has a program message queue. `SNDPGMMSG MSG(...)` or `MSGID(...) MSGF(...) MSGDTA(...)`, `MSGTYPE(*INFO \| *COMP \| *DIAG \| *NOTIFY \| *STATUS \| *ESCAPE)`, `TOPGMQ(*PRV)` (the caller's queue, the default), `TOPGMQ(*SAME)` (the program's own), `TOPGMQ(*EXT)`, `TOMSGQ` (a user's message queue), `KEYVAR`. A predefined message's text comes from its message description with the message data substituted ([message files](#message-files)). *ESCAPE ends the program; the caller sees it as an exception it can monitor for (CPF9898 when MSG is given). `RCVMSG PGMQ(*SAME \| *PRV) MSGQ MSGTYPE(*ANY \| *COMP \| *DIAG \| *INFO \| *EXCP \| *FIRST \| *LAST \| *NEXT \| *PRV) MSGKEY RMV WAIT` returns `MSG MSGLEN SECLVL SECLVLLEN MSGDTA MSGDTALEN MSGID SEV RTNTYPE MSGF MSGFLIB SNDMSGFLIB KEYVAR` from the program's own queue, its caller's (`PGMQ(*PRV)`) or a message queue (a user profile's, or QSYSOPR; `WAIT` seconds or *MAX for a message to arrive): *ANY and the types take new messages first in first out, *EXCP the last exception (a monitored error is on the queue as one); RMV(*NO) keeps the message as an old one; with no message the variables are blank or zero. The messages of the commands a program runs go on its queue as completion messages. `RTVMSG MSGID MSGF MSGDTA` returns a description's text. Messages to *PRV and *EXT are also shown (the command entry screen, or standard output for `as4tainr call`). |
| Parameters | Passed by reference: a variable passed on `CALL` (to a CL or RPG program, or from an RPG program to a CL program) gets back the value the called program left in its parameter; a character parameter shorter than the variable changes only its first characters. Constants are passed as values. |
| Subroutines | `SUBR SUBR(name)` ... `ENDSUBR [RTNVAL(value)]` after the program's other statements, `CALLSUBR SUBR(name) [RTNVAL(&int)]`, `RTNSUBR [RTNVAL(value)]`; at most 99 levels deep |
| Commands | `CALL PGM PARM` (CL or RPG), `CHKOBJ OBJ OBJTYPE` (`CPF9801` when missing), `RTVJOBA JOB USER NBR CURLIB TYPE`, `RTVSYSVAL SYSVAL RTNVAR` (the values IBM ships: QDATFMT MDY, QDATSEP `/`, QCCSID 65535, ...; QDATE, QTIME, QDAYOFWEEK and the like from the clock, in UTC; `CPF1028` for one not known), `RTVOBJD` (RTNLIB, OBJATR, TEXT, OWNER, CRTDATE and CHGDATE as CYYMMDDHHMMSS, CRTUSER; the other values blank or zero), `RTVMBRD` (RTNLIB, RTNMBR, FILEATR, FILETYPE, SRCTYPE, TEXT, CRTDATE, CHGDATE, NBRCURRCD), `CLRPFM`, `DLYJOB DLY RSMTIME`, `STRCMTCTL`, `ENDCMTCTL`, `COMMIT`, `ROLLBACK` (see [sql.md](sql.md#commitment-control)), `SBMJOB` (see [work-management.md](work-management.md)), `RTVDTAARA DTAARA RTNVAR` (see [data-areas.md](data-areas.md)), and every other command the job knows (CRTLIB, ADDLIBLE, CHGCURLIB, RUNSQL, CRTUSRPRF, ..., and commands made with CRTCMD, see [commands.md](commands.md)) with `&variables` replaced by their values |

A program that runs more than ten million steps is ended, so a loop that never ends cannot hang a job.

## Message files

`CRTMSGF`, `DLTMSGF`, `ADDMSGD MSGID MSGF MSG SECLVL SEV FMT`, `CHGMSGD` (*SAME keeps a value), `RMVMSGD`, `DSPMSGD RANGE MSGF`. A message description's text has substitution variables `&1`-`&99`, filled from the message data by the FMT fields: `*CHAR` without its trailing blanks, `*QTDCHAR` in apostrophes, `*DEC` packed with its decimal point and without leading zeros, `*BIN`/`*UBIN` as numbers, `*HEX` as `X'..'`, `*VARY` fields with their length first. `ADDMSGD` of an identifier that is there fails with `CPF2412`; a message file that is not found is `CPF2407`.

The system's message files (QCPFMSG, QRNXMSG, QSQLMSG) have no descriptions here, since IBM's texts cannot be copied: a message sent from one has its message data as its text (CPF9898 with a period), or, when resent with the identifier RCVMSG returned, the text it had when the job last saw it.

## Example

`samples/qclsrc/CUSTRPT.clle` checks its files (one CHKOBJ is expected to fail and is monitored), builds values in a DOFOR loop, picks a word with SELECT, calls the RPG program OWING and reports with SNDPGMMSG. `as4tainr call CUSTRPT L` prints `samples/expected/CUSTRPT_L.txt`; the demo checks it.

`samples/qclsrc/ORDENTC.clle` starts the order entry application ([order-entry.md](order-entry.md)): it checks the files, creates a data area the first time with `MONMSG CPF9801 EXEC(CRTDTAARA ...)`, calls the RPG program and reads the data area back with `RTVDTAARA`.

## Not supported yet

- `RCVMSG PGMQ(*EXT)`, `SENDER` (blank); inquiry messages and replies.
- `CHGVAR` of a whole `DCLF` record (`RCDFMT` buffers), `OVRDSPF`, message subfiles.
- Display commands (DSPLIB, WRKOBJ, ...) inside a CL program.
- Pointers passed to programs and procedures, RTNVAL of commands made with CRTCMD.
- CRTBNDCL from a source physical file member (only from a file, with `as4tainr crtbndcl`).

See [fidelity-gaps.md](fidelity-gaps.md).
