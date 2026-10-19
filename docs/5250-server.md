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

Command entry, after CRTLIB and a CALL of an RPG program:

```
                                Command Entry                            AS4TAINR
                                                                        QSECOFR
 Signed on to AS4TAINR as QSECOFR.
 > crtlib salesx text('Sales applications')
   Library SALESX created.
 > call ordtotal parm('1002')
   DSPLY  Order 1002: 3 lines, total 24.00











 Type command, press Enter.
 ===>

 F3=Exit   F4=Prompt   F9=Retrieve   F12=Cancel

 DSPLY  Order 1002: 3 lines, total 24.00
```

An error keeps the command on the command line so it can be corrected:

```
                                Command Entry                            AS4TAINR
                                                                        QSECOFR
 Signed on to AS4TAINR as QSECOFR.
 > crtlib salesx text('Sales applications')
   Library SALESX created.
 > call ordtotal parm('1002')
   DSPLY  Order 1002: 3 lines, total 24.00
 > crtlib salesx
   CPF2111: Library SALESX already exists.









 Type command, press Enter.
 ===> crtlib salesx

 F3=Exit   F4=Prompt   F9=Retrieve   F12=Cancel

 CPF2111: Library SALESX already exists.
```

DSPLIB:

```
                               Display Library

 Library . . . . . . . . .:   QSYS
 Number of objects . . . .:   6
 Text  . . . . . . . . . .:   System library

 Object      Type        Attribute   Text
 MYLIB       *LIB                    My first library
 QGPL        *LIB                    System library
 QSYS        *LIB                    System library
 QTEMP       *LIB                    System library
 SALES       *LIB                    Sales applications
 SALESX      *LIB                    Sales applications






                                                                       Bottom


 F3=Exit   F12=Cancel
```

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
| `RUNSQL SQL('...')` | Runs one INSERT, UPDATE or DELETE (not SELECT) and reports the rows changed, or a data definition statement such as CREATE TABLE. |
| `RUNSQLSTM SRCFILE(LIB/QSQLSRC) SRCMBR(NAME)` | Runs the SQL statements in a source member ([sql.md](sql.md)). |
| `DLTF FILE(LIB/NAME)` | Deletes a database or display file (not a physical file with logical files over it: CPF3203). |
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

**Command menu (F4 on an empty line).** A numbered menu of command groups (Library, Object, File, User profile, Program, Session). Type a number and press Enter to open a group's menu of commands, then a number to prompt that command. The "Selection or command" line also takes a command name: Enter runs it, F4 prompts it. F12 goes back one menu, F3 returns to the command line. The groups are `cl::GROUPS`; a unit test checks that every command is in exactly one.

**Prompting (F4).** The prompt has one line per parameter with its current value or default and the allowed values; required parameters are highlighted, qualified names get a separate Library line. Enter builds the command (parameters left at their default are left out), checks it, and runs it; the command as built appears in the log, so F9 retrieves it. F5 resets the values, F12 returns to the command line with what was typed, F3 returns to an empty command line. The prompt comes from the same command table (`cl::COMMANDS`) as parsing.

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
