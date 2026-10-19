# CL programs

CL programs (CLLE) are the glue of most IBM i applications: they set up the library list, check that objects exist, monitor for errors and call the RPG programs. as4tainr checks a CL program when it is created and runs it in the job that calls it.

```sh
as4tainr crtbndcl QGPL/CUSTRPT --src samples/qclsrc/CUSTRPT.clle --text 'Customer report driver'
as4tainr call CUSTRPT L          # or, on the 5250 command line: CALL CUSTRPT PARM(L)
```

`crtbndcl` reports errors with their line numbers and does not create a program that has any. It replaces an existing program of the same name, as `REPLACE(*YES)` does. `CALL` finds a CL program through the library list; a name that is not a CL program goes to the RPG runner.

## Supported

Written from IBM's public CL reference.

| Area | Supported |
| --- | --- |
| Source | `PGM PARM(...)` / `ENDPGM`, labels `NAME:`, comments `/* */`, continuation lines ending in `+` (next line's leading blanks dropped) or `-` (kept), keyword and positional parameters |
| Variables | `DCL` with `TYPE(*CHAR \| *DEC \| *LGL \| *INT)`, `LEN`, `VALUE`; defaults *CHAR 32, *DEC (15 5). *CHAR values are padded or truncated to their length; *DEC results are truncated to the variable's decimal positions and must fit its digits (`MCH1210` otherwise). Decimal arithmetic is exact. |
| Expressions | `+ - * /`, `*CAT *BCAT *TCAT` (`\|\| \|> \|<`), `*EQ *NE *GT *LT *GE *LE *NG *NL` (and `= ¬= <> > < >= <=`), `*AND *OR *NOT` (and `& \| ¬`), with IBM's precedence; character comparisons pad the shorter value with blanks. `*ON`, `*OFF`, `*BLANKS`. |
| Built-ins | `%SST`/`%SUBSTRING`, `%TRIM`, `%TRIML`, `%TRIMR`, `%CHAR`, `%DEC`, `%INT` |
| Control | `IF COND THEN` / `ELSE CMD`, `DO` / `ENDDO`, `DOWHILE`, `DOUNTIL`, `DOFOR VAR FROM TO BY`, `LEAVE`, `ITERATE`, `SELECT` / `WHEN` / `OTHERWISE` / `ENDSELECT`, `GOTO CMDLBL`, `RETURN` |
| Errors | `MONMSG MSGID EXEC` after a command, or program-level right after the declarations (tried after the command-level ones; when its EXEC is not a GOTO or RETURN, the program goes on after the command that failed). Generic IDs: `CPF0000` all CPF messages, `CPF9800` all CPF98xx. An unmonitored error ends the program and names the line. |
| Messages | `SNDPGMMSG MSG(...)` or `MSGID(...) MSGDTA(...)`, `MSGTYPE(*INFO \| *COMP \| *DIAG \| *ESCAPE)`: *ESCAPE ends the program with that message (CPF9898 when MSG is given), which a calling CL program can monitor for. Other messages go to the job log (the command entry screen, or standard output for `as4tainr call`). |
| Commands | `CALL PGM PARM` (CL or RPG), `CHKOBJ OBJ OBJTYPE` (`CPF9801` when missing), `RTVJOBA JOB USER NBR CURLIB TYPE`, `SBMJOB` (see [work-management.md](work-management.md)), `RTVDTAARA DTAARA RTNVAR` (see [data-areas.md](data-areas.md)), and every other command the job knows (CRTLIB, ADDLIBLE, CHGCURLIB, RUNSQL, CRTUSRPRF, ...) with `&variables` replaced by their values |

A program that runs more than ten million steps is ended, so a loop that never ends cannot hang a job.

## Example

`samples/qclsrc/CUSTRPT.clle` checks its files (one CHKOBJ is expected to fail and is monitored), builds values in a DOFOR loop, picks a word with SELECT, calls the RPG program OWING and reports with SNDPGMMSG. `as4tainr call CUSTRPT L` prints `samples/expected/CUSTRPT_L.txt`; the demo checks it.

`samples/qclsrc/ORDENTC.clle` starts the order entry application ([order-entry.md](order-entry.md)): it checks the files, creates a data area the first time with `MONMSG CPF9801 EXEC(CRTDTAARA ...)`, calls the RPG program and reads the data area back with `RTVDTAARA`.

## Not supported yet

- Parameters are passed by value: a called program's changes to them do not come back to the caller.
- Files in CL (`DCLF`, `RCVF`, `SNDRCVF`, `SNDF`), `OVRDBF`, `RCVMSG`, `RTVSYSVAL`, `RTVOBJD`, message files.
- Display commands (DSPLIB, WRKOBJ, ...) inside a CL program.
- `%SST` as the target of `CHGVAR`, `*PTR` variables, `DCLPRCOPT`, subroutines (`SUBR`), `INCLUDE`.
- CRTBNDCL from a source physical file member (only from a file, with `as4tainr crtbndcl`).

See [fidelity-gaps.md](fidelity-gaps.md).
