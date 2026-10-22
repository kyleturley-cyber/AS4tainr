# 5250 display server

`as4tainr tn5250` serves 5250 green-screen sessions over telnet (TN5250, [RFC 1205](https://www.rfc-editor.org/rfc/rfc1205), [RFC 2877](https://www.rfc-editor.org/rfc/rfc2877)). Any 5250 emulator can connect: tn5250, tn5250j, or IBM ACS.

```sh
as4tainr tn5250 --listen 0.0.0.0:23 --system-name AS4TAINR
```

In Docker, `docker compose up` publishes it on `localhost:2323`. The same server also has a terminal in the browser, at `http://localhost:5250` in Docker: see [web-terminal.md](web-terminal.md).

## Signing on

The first `as4tainr init` creates the security officer profile QSECOFR:

- If `AS4TAINR_QSECOFR_PASSWORD` is set, that is its password.
- Otherwise the password is `QSECOFR`, and it has expired: the first sign-on goes to the Change Password screen.

Passwords are stored as Argon2id hashes. A wrong user and a wrong password give the same message (`CPF1107`), and take the same time, so the sign-on screen does not reveal which profiles exist. After 3 failed attempts the connection is closed.

Passwords are not case sensitive by default, like IBM i as shipped (system value QPWDLVL 0); `AS4TAINR_QPWDLVL=2` makes them case sensitive. Passwords set before as4tainr followed QPWDLVL still work as typed and are converted at the next sign-on.

**Forgot the QSECOFR password?** `as4tainr rstpwd` (or `as4tainr rstpwd USER`) sets it back to the profile name, expired, so the next sign-on asks for a new one. In Docker: `docker compose exec as4tainr as4tainr rstpwd`.

## Screens

The screens below are captured from the real `tn5250` client connected to as4tainr (by `tools/e2e_5250.py`'s harness).

Sign-on:

```
                                   Sign On
                                               System  . . . . .:   AS4TAINR
                                               Subsystem . . . .:   QINTER
                                               Display . . . . .:   QPADEV0003

                User  . . . . . . . . . . . . . .
                Password  . . . . . . . . . . . .
                Current library . . . . . . . . .













              as4tainr - legacy RPG and 5250 applications in a container
```

Command entry, after CRTLIB and a CALL of an RPG program. The layout follows IBM i 7.5's Command Entry as captured on a real system (docs/golden-outputs.md): the request level on line 2, the previous commands from column 5 with their messages under them as texts (no message IDs), "Bottom", and a command line of four rows. A command's messages are not repeated on line 24.

```
                                 Command Entry                         AS4TAINR
                                                           Request level:   2
 Previous commands and messages:
    > crtlib salesx text('Sales applications')
      Library SALESX was created.
    > call ordtotal parm('1002')
      DSPLY  Order 1002: 3 lines, total 24.00








                                                                         Bottom
 Type command, press Enter.
 ===>



 F3=Exit   F4=Prompt   F9=Retrieve   F12=Cancel

```

An error keeps the command on the command line so it can be corrected:

```
                                 Command Entry                         AS4TAINR
                                                           Request level:   2
 Previous commands and messages:
    > crtlib salesx text('Sales applications')
      Library SALESX was created.
    > call ordtotal parm('1002')
      DSPLY  Order 1002: 3 lines, total 24.00
    > crtlib salesx
      Library SALESX already exists.






                                                                         Bottom
 Type command, press Enter.
 ===> crtlib salesx



 F3=Exit   F4=Prompt   F9=Retrieve   F12=Cancel

```

A command typed without a required parameter is not run: its prompt is shown, with the missing parameter's message on line 24 and the cursor on it, as IBM i does in an interactive job. F3 or F12 on a prompt puts the command into the log with "Command prompting was ended by F3 or F12." (CPF6801) and leaves the command line empty.

DSPLIB, laid out as IBM i 7.5 draws it (the library's attributes in two columns, option 5, the objects with their size and text, F17/F18 for the top and bottom):

```
                                Display Library

 Library  . . . . . . :   QGPL            Number of objects  . :   13
 Type . . . . . . . . :   PROD            Library ASP number . :   1
 Create authority . . :   *SYSVAL         Library ASP device . :   *SYSBAS
                                          Library ASP group  . :   *SYSBAS

 Type options, press Enter.
   5=Display description

 Opt  Object      Type      Attribute          Size  Text
      CUSTINQD    *FILE     DSPF                  0
      CUSTMAST    *FILE     PF                90112  Customer master
      CUSTMNTD    *FILE     DSPF                  0
      CUSTNAME    *FILE     LF                    0  Customers by name
      CUSTOWE     *FILE     LF                    0  Customers who owe
      CUSTRPT     *PGM      CLLE                  0  Customer report driver
      CUSTWRKD    *FILE     DSPF                  0
      ITEMMAST    *FILE     PF                57344  Item master
      ORDENTC     *PGM      CLLE                  0  Order entry
      ORDENTD     *FILE     DSPF                  0
                                                                        More...
 F3=Exit   F12=Cancel   F17=Top   F18=Bottom
```

DSPLIBL, DSPDTAARA, DSPOBJD, WRKOBJ, WRKSPLF (F11 shows each file's job), DSPJOBLOG and DSPMSG follow IBM i's layouts the same way (`commands::IbmLayout`); the other list displays keep a plainer layout. On Command Entry, F10 shows every request level's commands and messages, each command with its level.

## Commands

| Command | What it does |
| --- | --- |
| `CRTLIB LIB(name) TYPE(*PROD) TEXT('...')` | Creates a library (a PostgreSQL schema). `CPF2111` if it exists. |
| `DSPLIB LIB(name\|*LIBL\|*CURLIB)` | Lists the objects in a library. `DSPLIB QSYS` lists the libraries. |
| `DSPLIBL` | Shows the job's library list: the system part (QSYS, QSYS2, QHLPSYS, QUSRSYS, type `SYS`), the current library if there is one (`CUR`), and the user part (`USR`, QGPL and QTEMP for a new job). Option 5 shows a library's objects. |
| `ADDLIBLE LIB(name) POSITION(*FIRST\|*LAST)` | Adds a library to the user part of the library list. `CPF2103` if it is already in the list. |
| `RMVLIBLE LIB(name)` | Removes a library from the user part of the library list. |
| `CHGCURLIB CURLIB(name\|*CRTDFT)` | Sets the current library; `*CRTDFT` means none (objects are then created in QGPL). |
| `CHGLIBL LIBL(names\|*SAME\|*NONE) CURLIB(*SAME\|*CRTDFT\|name)` | Replaces the user part of the library list, and optionally the current library. |
| `CALL PGM(name) PARM('a' 'b')` | Runs a CL or RPG program with the job's library list. DSPLY output appears in the log; DSPLY with a response asks on the screen. |
| `SIGNOFF` | Ends the session and returns to the sign-on screen. |
| `CRTUSRPRF USRPRF(name) PASSWORD(*USRPRF\|*NONE\|pw) PWDEXP STATUS USRCLS CURLIB TEXT SPCAUT` | Creates a user profile. Needs `*SECADM`. `SPCAUT(*USRCLS)` gives the class's special authorities (`*SECOFR`: all eight; `*SECADM`: `*SECADM`; `*SYSOPR`: `*JOBCTL *SAVSYS`; `*PGMR`, `*USER`: none). |
| `DSPUSRPRF USRPRF(name)` | Shows a profile: class, status, special authorities, previous sign-on. Your own, or any with `*SECADM`. `CPF2204` if it does not exist. |
| `DLTUSRPRF USRPRF(name)` | Deletes a profile. Needs `*SECADM`; not QSECOFR or yourself. |
| `CHGPWD` | Shows the Change Password screen. |
| `WRKOBJ OBJ(lib/name) OBJTYPE(*ALL\|*LIB\|*FILE)` | Lists objects. The name can be generic (`CUST*`) or `*ALL`; the library `*LIBL`, `*CURLIB`, `*USRLIBL`, `*ALL`, `*ALLUSR` or a name. List only for now: no options. |
| `STRSQL` | Interactive SQL: type a statement, see the rows (see [sql.md](sql.md)). |
| `RUNSQL SQL('...')` | Runs one INSERT, UPDATE or DELETE (not SELECT), or a data definition statement such as CREATE TABLE. No completion message; SQL9010 when it fails. |
| `RUNSQLSTM SRCFILE(LIB/QSQLSRC) SRCMBR(NAME)` or `SRCSTMF(path)` | Runs the SQL statements in a source member, or in a stream file (converted from its CCSID; a byte order mark is skipped) ([sql.md](sql.md)). |
| `CHGPFM FILE(LIB/FILE) MBR(name) SRCTYPE(type) TEXT('...')` | Changes a source member's type and text (CPF3141 if the member does not exist). |
| `CRTJRNRCV`, `CRTJRN`, `CHGJRN`, `DLTJRN`, `DLTJRNRCV`, `STRJRNPF`, `ENDJRNPF`, `DSPJRN` | Journals: see [journals.md](journals.md). |
| `CRTSAVF`, `CLRSAVF`, `DSPSAVF`, `SAVOBJ`, `SAVLIB`, `RSTOBJ`, `RSTLIB`, `DLTLIB`, `CLRLIB` | Save and restore: see [save-restore.md](save-restore.md). |
| `ADDPFTRG`, `RMVPFTRG` | Trigger programs on physical files ([journals.md](journals.md)). |
| `DLTF FILE(LIB/NAME)` | Deletes a database, display or save file (not a physical file with logical files over it: CPF3219). |
| `CRTRPGPGM PGM(LIB/NAME)` | Creates a program from RPG/400 (RPG III) source in QRPGSRC: converted to RPG IV as CVTRPGSRC does, then compiled. |
| `CVTRPGSRC FROMFILE(LIB/QRPGSRC) FROMMBR(name\|generic*\|*ALL) TOFILE(LIB/QRPGLESRC)` | Converts RPG III members to RPG IV members (type RPGLE) and lists the conversion messages. |
| `CVTRPGFREE FROMFILE(LIB/QRPGLESRC) FROMMBR(name) TOMBR(name)` | as4tainr's own command: converts a fixed-form RPGLE member to free form (what has no free form stays fixed). |
| `CRTRPGMOD MODULE(LIB/NAME)` | Creates a module from QRPGLESRC (checked like CRTBNDRPG; `NOMAIN` modules have only procedures). The compile options that change nothing here are accepted on the create commands: `DBGVIEW`, `OPTION`, `OUTPUT`, `TGTRLS`, CRTSQLRPGI's `RPGPPOPT` and `CLOSQLCSR`, CRTDSPF's `RSTDSP`, `DFRWRT`. CRTDSPF `REPLACE(*YES)` (the default) replaces a display file. |
| `CRTSRVPGM SRVPGM(LIB/NAME) MODULE(...) EXPORT(*SRCFILE\|*ALL) SRCFILE SRCMBR BNDSRVPGM BNDDIR` | Creates a service program from modules (`BNDSRVPGM((LIB/NAME *IMMED))` element lists too); its exports come from binder language (`STRPGMEXP`/`EXPORT`/`ENDPGMEXP` in QSRVSRC) or all exported procedures. |
| `CRTPGM PGM(LIB/NAME) MODULE(...) ENTMOD BNDSRVPGM BNDDIR` | Binds modules and service programs into a program. |
| `CRTBNDDIR`, `ADDBNDDIRE`, `RMVBNDDIRE`, `DSPBNDDIR` | Binding directories: lists of modules and service programs searched for imports by CRTPGM, CRTSRVPGM and CRTBNDRPG (`BNDDIR`, or `ctl-opt bnddir`). |
| `DLTPGM`, `DLTMOD`, `DLTSRVPGM`, `DLTBNDDIR` | Delete programs and ILE objects (CPC2191; CPF2105 when not found). |
| `RCLACTGRP ACTGRP(*ELIGIBLE\|name)` | Ends an activation group of the job (*ELIGIBLE: all but the default one): its service programs and the programs active in it start over at their next call. |
| `RCLRSC` | Ends the programs of the default activation group that are still active (they ended without LR). |
| `DSPPGM PGM(LIB/NAME)`, `DSPSRVPGM SRVPGM(LIB/NAME)` | Show a program's or service program's modules, bound service programs, signatures and activation group. |
| `UPDPGM PGM(LIB/NAME) MODULE(names\|*NONE)`, `UPDSRVPGM` | Replace modules and bind again (taking the service programs' current signatures). |
| `CRTCLMOD MODULE(LIB/NAME)` | Creates a CL module (QCLSRC); bound by CRTPGM, it calls RPG procedures with CALLPRC. |
| `CHKOBJ OBJ(lib/name) OBJTYPE(type) [MBR] [AUT]` | No message when the object is there; CPF9801 (object), CPF9810 (library), CPF9802 (authority), CPF9815 (member). |
| `DSPOBJD OBJ(lib/name) OBJTYPE(types)` | The objects with attribute, size and text (DETAIL(*BASIC)). |
| `DSPSYSVAL SYSVAL(name)` | A system value's current and shipped values. |
| `DSPFD FILE(lib/name) [TYPE(*ALL\|*ATR\|*MBRLIST\|*ACCPTH)]` | A file's attributes, members and access path. |
| `DSPFFD FILE(lib/name)` | A file's fields: type, length, bytes and position in the record. |
| `CPYF FROMFILE TOFILE(file\|*PRINT) [MBROPT] [CRTFILE] [FMTOPT]` | Copies records (CPC2955), into an empty member unless MBROPT(*ADD) or *REPLACE; CRTFILE(*YES) creates the to-file; FMTOPT(*MAP *DROP) or *NOCHK when the fields differ (else CPF2817). |
| `DSPPFM FILE(lib/name)` | Shows a physical file's records in arrival order, one line each with the field names above. F19/F20 shift left/right on wide files. |

Keyword and positional forms both work (`CRTLIB MYLIB` = `CRTLIB LIB(MYLIB)`). Unquoted values are folded to upper case; quoted values keep their case, with `''` for an apostrophe.

After sign-on the user's initial program runs (if the profile has one) and the initial menu is shown, MAIN for new profiles: see [menus.md](menus.md). Option 7 on MAIN, or `CALL QCMD`, opens Command Entry.

Keys on the command entry screen:

| Key | Action |
| --- | --- |
| Enter | Run the command |
| F3 | Back to the menu (sign off when Command Entry is the first display, e.g. the initial menu was not found) |
| F9 | Retrieve the previous command (repeat to go further back) |
| F12 | Clear the command line |
| F4 | Prompt the command typed (what was typed so far fills the prompt); on an empty line, show the command menu |

**Command menu (F4 on an empty line).** A numbered menu of command groups (Library, Object, File, User profile, Program, Session). Type a number and press Enter to open a group's menu of commands, then a number to prompt that command. The "Selection or command" line also takes a command name: Enter runs it, F4 prompts it. F12 goes back one menu, F3 returns to the command line. A menu longer than one screen shows More... and pages with Page Down and Page Up. The groups are `cl::GROUPS`; a unit test checks that every command is in exactly one.

**Prompting (F4).** The prompt is laid out like IBM i's with keywords shown: one line per parameter with its prompt text and dots to column 33, the keyword in column 35, a `>` in column 48 before a value that was typed, and the field in column 50 (a qualified name's Library line below it, its field in column 52); "More..." or "Bottom" on line 21, the keys on line 22. Required parameters are highlighted. Enter builds the command (parameters left at their default are left out), checks it, and runs it; the command as built appears in the log, so F9 retrieves it. F5 resets the values, F3 and F12 return to an empty command line, the command and CPF6801 in the log. The prompt comes from the same command table (`cl::COMMANDS`) as parsing.

On list screens, Page Down / Page Up (Roll Up / Roll Down) page through the list. Lists with options (DSPLIBL) have an Opt column: type an option number next to one or more entries and press Enter to run it on each, then you come back to the list. F12 goes back one screen, F3 goes back to the command line.

**Library list.** A job's library list is searched in order: the system part, the current library, the user part. The system part is fixed. The user part of a new job comes from `AS4TAINR_LIBL` (default `QGPL,QTEMP`); the current library comes from the sign-on screen, else the user profile (`*CRTDFT` = none, as QSECOFR is shipped).

## How it is built

| Module | Role |
| --- | --- |
| `core/src/tn5250/telnet.rs` | Telnet negotiation (TERMINAL-TYPE, END-OF-RECORD, BINARY) and 5250 record framing (10-byte header, FF doubling, IAC EOR) |
| `core/src/tn5250/ds.rs` | 5250 data stream: Clear Unit, Write To Display, SBA/SF/IC orders, Read MDT Fields; parsing the input record (cursor, AID, modified fields) |
| `core/src/tn5250/ebcdic.rs` | CCSID 37 conversion tables, generated from the standard code page 037 mapping |
| `core/src/tn5250/session.rs` | Sign-on, Change Password, initial program and menu, menus, Command Entry and list screens |
| `core/src/menus.rs` | GO, CRTMNU, the system menus (MAIN, USER, JOBS, FILES, PROGRAM, SECURITY) |
| `core/src/cl.rs` | CL parser and the command definition table |
| `core/src/commands.rs` | Command implementations, shared with future batch jobs |
| `core/src/users.rs` | User profiles and password hashing |

## Testing

`tools/e2e_5250.py` drives the real tn5250 client in a pseudo-terminal and reads its screen with [pyte](https://github.com/selectel/pyte). It signs on (wrong password, forced password change, new password), runs CRTLIB/DSPLIB/CALL, prompts with F4, creates a user profile and signs on with it, runs WRKOBJ and DSPPFM, and checks the error messages, F9 and SIGNOFF. The `tn5250` CI job runs it on every push. To run it locally:

```sh
git clone https://github.com/tn5250/tn5250 && cmake -S tn5250 -B tn5250/build && cmake --build tn5250/build
pip install pyte
TN5250=tn5250/build/curses/tn5250 python3 tools/e2e_5250.py 127.0.0.1:2323
```

The harness sets `TERM=xterm-r6`, because ncurses uses the "repeat character" escape under `xterm` and pyte does not emulate it.

## Not yet

These are tracked in [fidelity-gaps.md](fidelity-gaps.md): TLS (port 992), automatic sign-on and device names from NEW-ENVIRON ([RFC 4777](https://www.rfc-editor.org/rfc/rfc4777)), 27x132 screens, prompt help (F1) and list-parameter prompting, WRKOBJ options, disabling profiles after repeated failures, and job logs.
