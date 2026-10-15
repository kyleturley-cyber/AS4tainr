# 5250 display server

`ibmish tn5250` serves 5250 green-screen sessions over telnet (TN5250, [RFC 1205](https://www.rfc-editor.org/rfc/rfc1205), [RFC 2877](https://www.rfc-editor.org/rfc/rfc2877)). Any 5250 emulator can connect: tn5250, tn5250j, or IBM ACS.

```sh
ibmish tn5250 --listen 0.0.0.0:23 --system-name IBMISH
```

In Docker, `docker compose up` publishes it on `localhost:2323`.

## Signing on

The first `ibmish init` creates the security officer profile QSECOFR:

- If `IBMISH_QSECOFR_PASSWORD` is set, that is its password.
- Otherwise the password is `QSECOFR`, and it has expired: the first sign-on goes to the Change Password screen.

Passwords are stored as Argon2id hashes. A wrong user and a wrong password give the same message (`CPF1107`), and take the same time, so the sign-on screen does not reveal which profiles exist. After 3 failed attempts the connection is closed.

## Screens

The screens below are captured from the real `tn5250` client connected to ibmish (by `tools/e2e_5250.py`'s harness).

Sign-on:

```
                                   Sign On
                                               System  . . . . .:   IBMISH
                                               Subsystem . . . .:   QINTER
                                               Display . . . . .:   QPADEV0003

                User  . . . . . . . . . . . . . .
                Password  . . . . . . . . . . . .
                Current library . . . . . . . . .













              ibmish - an ibm-ish experience for open source devs
```

Command entry, after CRTLIB and a CALL of an RPG program on Jariko:

```
                                Command Entry                            IBMISH
                                                                        QSECOFR
 Signed on to IBMISH as QSECOFR.
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
                                Command Entry                            IBMISH
                                                                        QSECOFR
 Signed on to IBMISH as QSECOFR.
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
| `DSPLIBL` | Shows the job's library list. |
| `ADDLIBLE LIB(name) POSITION(*FIRST\|*LAST)` | Adds a library to the job's library list. |
| `CALL PGM(name) PARM('a' 'b')` | Runs an RPG program on Jariko with the job's library list. DSPLY output appears in the log. |
| `SIGNOFF` | Ends the session and returns to the sign-on screen. |

Keyword and positional forms both work (`CRTLIB MYLIB` = `CRTLIB LIB(MYLIB)`). Unquoted values are folded to upper case; quoted values keep their case, with `''` for an apostrophe.

Keys on the command entry screen:

| Key | Action |
| --- | --- |
| Enter | Run the command |
| F3 | Sign off |
| F9 | Retrieve the previous command (repeat to go further back) |
| F12 | Clear the command line |
| F4 | Prompting is not implemented yet |

On list screens, Page Down / Page Up (Roll Up / Roll Down) page through the list, and F3 or F12 return.

## How it is built

| Module | Role |
| --- | --- |
| `core/src/tn5250/telnet.rs` | Telnet negotiation (TERMINAL-TYPE, END-OF-RECORD, BINARY) and 5250 record framing (10-byte header, FF doubling, IAC EOR) |
| `core/src/tn5250/ds.rs` | 5250 data stream: Clear Unit, Write To Display, SBA/SF/IC orders, Read MDT Fields; parsing the input record (cursor, AID, modified fields) |
| `core/src/tn5250/ebcdic.rs` | CCSID 37 conversion tables, generated from the standard code page 037 mapping |
| `core/src/tn5250/session.rs` | Sign-on, Change Password, Command Entry and list screens |
| `core/src/cl.rs` | CL parser and the command definition table |
| `core/src/commands.rs` | Command implementations, shared with future batch jobs |
| `core/src/users.rs` | User profiles and password hashing |

## Testing

`tools/e2e_5250.py` drives the real tn5250 client in a pseudo-terminal and reads its screen with [pyte](https://github.com/selectel/pyte). It signs on (wrong password, forced password change, new password), runs CRTLIB/DSPLIB/CALL, checks the error messages, F9 and SIGNOFF. The `tn5250` CI job runs it on every push. To run it locally:

```sh
git clone https://github.com/tn5250/tn5250 && cmake -S tn5250 -B tn5250/build && cmake --build tn5250/build
pip install pyte
TN5250=tn5250/build/curses/tn5250 python3 tools/e2e_5250.py 127.0.0.1:2323
```

The harness sets `TERM=xterm-r6`, because ncurses uses the "repeat character" escape under `xterm` and pyte does not emulate it.

## Not yet

These are tracked in [fidelity-gaps.md](fidelity-gaps.md): TLS (port 992), automatic sign-on and device names from NEW-ENVIRON ([RFC 4777](https://www.rfc-editor.org/rfc/rfc4777)), 27x132 screens, F4 prompting, display files (DDS `*DSPF`) for RPG programs, disabling profiles after repeated failures, and job logs.
