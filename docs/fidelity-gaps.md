# Known fidelity gaps

These are places where ibmish knowingly behaves differently from IBM i. Each one should eventually turn into a fix with a test, or into a documented decision.

| Area | IBM i behavior | ibmish today | Impact |
| --- | --- | --- | --- |
| Key collating sequence | Character keys are ordered by EBCDIC: lowercase before uppercase, and letters before digits | Byte order of UTF-8 text (`COLLATE "C"`): digits before uppercase before lowercase | Mixed letter/digit or mixed-case keys read in a different order. Planned fix: an EBCDIC sort key per character key field |
| Record locking | Input/update files lock the record read for update until UPDATE, the next read, or UNLOCK | No locks | Concurrent update jobs can overwrite each other. Planned: `SELECT ... FOR UPDATE` within the job's commitment scope |
| Commitment control | `COMMIT`/`ROLBK` under journaling | Each operation autocommits | No rollback of partial work |
| Field types | A, P, S, B, I, U, F, L, T, Z, G, O, H... | A, P, S only | Files using other types are rejected at CRTPF |
| CL programs | The full CL language and command set, parameters passed by reference | The subset in cl-programs.md; parameters by value; no files in CL (DCLF/RCVF), OVRDBF, RCVMSG, RTVSYSVAL; *DEC to *CHAR in CHGVAR gives digits with leading zeros (to be checked against a real system) | Programs using unsupported statements fail at CRTBNDCL with the line number |
| SQL | DB2 for i SQL: DDL, commitment control, embedded SQL (SQLRPGLE), QSYS2 catalog views, all scalar functions and special registers | SELECT/VALUES/INSERT/UPDATE/DELETE, one at a time, autocommitted, with the translations in sql.md; PostgreSQL semantics otherwise (e.g. character comparison and sort order) | Statements outside the subset fail with SQL0084 or SQL0104 |
| Logical files | Keyed and selected views over physical files, join logical files, several record formats, RENAME/CONCAT/SST, DYNSLT | One physical file per logical file, one record format, a subset of its fields, keys (DESCEND), select/omit with COMP/VALUES/RANGE/ALL, UNIQUE (see logical-files.md) | Join and multi-format logical files are rejected at CRTLF |
| CHAIN not found | No current record; next READ position is not well defined in all cases | The next READ starts at the searched key | Programs that rely on this undefined behavior may differ |
| Object names | National characters allowed in some CCSIDs | ASCII A–Z, 0–9, `$#@_.` only | |
| Null-capable fields | `ALWNULL` | Not supported | |
| Character data | EBCDIC CCSIDs (37, 500, ...) | UTF-8 in PostgreSQL | Hex constants and byte-level tricks (`%BITAND` on text) differ |
| Jariko DSPLY output | DSPLY writes to the job's external message queue | Jariko prints the message on standard output | Fine for tests; operator messages come later |
| 5250 sessions | Many session and device options | 24x80 only; no TLS (992), no NEW-ENVIRON auto sign-on or device naming | Use only on trusted networks for now; telnet sends passwords in clear text |
| Sign-on security | QMAXSIGN / QMAXSGNACN can disable the profile or the device | The connection closes after 3 failures; profiles are never disabled | Repeated guessing is slowed but not blocked across connections |
| Library list | System values QSYSLIBL and QUSRLIBL; product libraries (`PRD`); QTEMP is private to each job and empty at sign-on; DSPLIBL shows the ASP device | The system part is fixed (QSYS, QSYS2, QHLPSYS, QUSRSYS); the user part comes from `IBMISH_LIBL`; no product libraries; QTEMP is one shared library; no ASP column; EDTLIBL is missing | Objects put in QTEMP are visible to other jobs and are not deleted at sign-off |
| Command entry | F4 prompting with help, F9 (all parameters), F10 (additional parameters), "+ for more values" on list parameters, job log, message queues | F4 prompts every parameter on one screen; list parameters are one text field; no help; messages shown in the session log only | Prompts look plainer; long lists are typed as `'a' 'b'` |
| CRTUSRPRF | About 60 parameters; authority checks on every object | USRPRF, PASSWORD, PWDEXP, STATUS, USRCLS, CURLIB, TEXT, SPCAUT. Only `*SECADM` is checked (for CRTUSRPRF, DLTUSRPRF and other users' DSPUSRPRF); object authority is not modelled yet | Every profile can use every object |
| WRKOBJ | A list with options (2=Edit authority, 4=Delete, 5=Display, ...) | The list only; object types `*LIB` and `*FILE` | |
| DSPPFM | Shows the raw record image (zoned digits, packed as hex), F10/F11 hex view, position to line | Fields separated by a blank, numbers as decimal text, at most 9999 records; F19/F20 shift | Easier to read than the real one, but not identical |
| Password level (QPWDLVL) | Levels 0-3 change case rules, length (10 at 0/1, 128 at 2/3) and allowed characters | `IBMISH_QPWDLVL` sets case: 0/1 (default) not case sensitive, 2/3 case sensitive. Length is at most 28 at every level; allowed characters are not checked | A password valid on ibmish may be too long or use characters IBM i rejects at level 0/1 |
| Display files | Full DDS for display files | Common subset (see display-files.md): no `PUTOVR`/`OVRDTA`, `WINDOW`, `REF`/`REFFLD`, `VALUES`/`RANGE`/`COMP`, `EDTWRD`, help, `INDARA` | Programs using windows cannot run yet; validity checking keywords are not enforced |
| Subfiles | All subfile keywords | `SFL`, `SFLCTL`, `SFLSIZ`, `SFLPAG`, `SFLDSP`, `SFLDSPCTL`, `SFLCLR`, `SFLDLT`, `SFLEND(*MORE/*PLUS)`, `SFLNXTCHG`, `SFLMSG`, `PAGEUP`/`PAGEDOWN`. Not yet: `SFLRCDNBR` (the first page is always shown), `SFLINZ`, `SFLFOLD`/`SFLDROP`, `SFLLIN` (side-by-side records), `SFLCSRRRN`, message subfiles (`SFLMSGRCD`), `SFLEND(*SCRBAR)` shows as `*MORE` | Load-all and page-at-a-time subfiles work; position-to and folded lists do not |
| Display I/O operations | EXFMT, WRITE, READ, READC, CHAIN/UPDATE of subfile records | EXFMT, WRITE, READC, CHAIN and UPDATE of subfile records. Not yet: WRITE followed by a separate READ of a display file, `READ` of a WORKSTN file name, free-form `DCL-F` with `SFILE` | A WRITE is shown with the next EXFMT, not immediately; a program that writes a record and then waits (no input) shows nothing |
| Records on the screen | Input fields of every record on the screen can be typed into | Only the record being read (and its subfile) takes input; other records are output only | Rare in practice |
| Numeric display fields | Unedited output shows the field's digits; input positions follow the data type | Unedited output shows digits, a decimal point and a sign position | Screen layouts with unedited numeric fields can be wider by one or two positions |
| DATE / TIME constants | Job date in the job's date format; local time | MM/DD/YY and HH:MM:SS in UTC | |

## Gaps in Jariko (the RPG runtime)

These are RPG features Jariko does not support yet. They are worth contributing upstream to [smeup/jariko](https://github.com/smeup/jariko).

| Feature | Jariko v2.2.0 | Workaround |
| --- | --- | --- |
| `%FOUND(filename)` | Fails with "%FOUND expression with file names is not implemented yet" | Use `%FOUND` with no argument right after the file operation |
| `ITER` inside `DOW` | `DOW` catches `LEAVE` but not `ITER` (`DO`/`DOU` handle both); the program ends with IterException | Restructure with `IF`/`ELSE`; a one-line upstream fix |
| Same field in two files with different numeric types | "Incongruous definitions" when a name is packed in one file and zoned in another | ibmish declares display-file numbers as packed, like most database files |
| Startup output | Prints a version banner and feature flags on standard output | The ibmish launcher redirects it to standard error |
| WORKSTN operations other than EXFMT | `WRITE`/`UPDATE` of a display record look for a database file; `READC` parses but does nothing; `SFILE` is not used | Before parsing, the ibmish runner declares each display record format as a DISK file served through the bridge, and rewrites `READC sfl` as `READ sfl`. Error messages from Jariko then show line numbers a few lines higher than the source (the added F specs). See display-files.md |
