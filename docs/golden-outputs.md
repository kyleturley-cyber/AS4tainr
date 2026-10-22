# Golden outputs from a real IBM i

as4tainr's expected values come from IBM's documentation or from a real system (CLAUDE.md, rule 3). The real system is [PUB400](https://pub400.com), a free public IBM i 7.5. Commands run there over SSH through the PASE `system` utility, and their output is compared with `as4tainr system`.

## Setup (once)

1. Sign up at PUB400. Sign on once with a 5250 client (port 23) to set your password; SSH refuses an expired one. PUB400's character set is CCSID 273 (German), so use only letters and digits in the password.
2. Make an SSH key and add it to your profile. A key line is too long for EDTF (about 70 columns), and starting QSH can take minutes on PUB400, so write the key with SQL from STRSQL NAMING(*SQL), one short line per row (`||` comes out as a different character in CCSID 273; use CONCAT):

   ```sql
   CALL QSYS2.IFS_WRITE(
   PATH_NAME => '/home/USER/.ssh/authorized_keys',
   LINE => 'ssh-ed25519 AAAA...first half'
   CONCAT 'second half...',
   FILE_CCSID => 819, OVERWRITE => 'REPLACE',
   END_OF_LINE => 'LF')
   ```

   Then `CHGAUT OBJ('/home/USER/.ssh') USER(*PUBLIC) DTAAUT(*EXCLUDE) OBJAUT(*NONE)`, and the same for `authorized_keys`.
3. On Windows, restrict the private key file (`icacls KEY /inheritance:r`, remove BUILTIN\Users, grant yourself) and add a host alias to `~/.ssh/config`:

   ```
   Host pub400
     HostName pub400.com
     Port 2222
     User USER
     IdentityFile C:\path\to\id_ed25519
   ```

   `ssh pub400 "system DSPLIBL"` then prints the library list without asking for a password. PUB400's "Enter your password" text is only its login banner. Its SSH server sometimes closes a new connection at once ("Connection closed by 185.113.5.134 port 2222"); try again.

## Capturing

```
powershell -ExecutionPolicy Bypass -File tools\golden\golden.ps1 -List tools\golden\basics.txt
```

A list has one `name|CL command` per line (run with `system`, standard output and error together) or `name|!shell line` (standard error lines prefixed `2> `). The script is copied to the IBM i with `scp` and run there over one SSH connection (piping a long script into `ssh` deadlocks in Windows PowerShell); the script retries when the run is cut short. It prints each command's name as it starts and writes what comes back to `target\ibmi\attempt<N>.out` as it arrives. A command still running after `-Timeout` seconds (default 120) is killed, and its output ends with `TIMEOUT after N s`. A command that sends an inquiry message waits for a reply forever in this job. Results go to `target\ibmi\golden\<name>.txt`. They hold IBM's message texts and are never committed: tests take values from them (message IDs, report layouts, exit statuses) and say so in a comment.

The lists:

- `tools/golden/basics.txt`: display commands and their printed reports (DSPLIBL, DSPSYSVAL, DSPOBJD, DSPMSGD, DSPFD), CHKOBJ, CRTPF RCDLEN, DLTF.
- `tools/golden/system-utility.txt`: the `system` utility itself (options, streams, exit statuses).
- `tools/golden/messages.txt`: completion and escape message IDs and exit statuses of most commands as4tainr has, the command analyzer's diagnostics, shipped system values.
- `tools/golden/files-and-programs.txt`: CPYF, DSPFFD, DSPFD, DSPOBJD, save files, CL and RPG programs from stream files, MONMSG of a failed CALL.

Rounds so far (2026-10): basics, system utility, messages (130 commands), files and programs (43 commands; 40 match, the other 3 are a stream file PUB400 could not open and the CALL that depends on it).

### 5250 screens

```
powershell -ExecutionPolicy Bypass -File tools\golden\screens.ps1 [-List tools\golden\screens.txt]
```

`tools/golden/Tn5250Capture.cs` is a small TN5250 client (C# 5, loaded by `screens.ps1` with `Add-Type`, so nothing has to be installed on Windows). The script opens an SSH tunnel to PUB400's telnet port (`ssh -N -L 50023:localhost:23 pub400`), asks for the password with `Read-Host -AsSecureString` (it goes only to PUB400, is never written anywhere and is zeroed after use), then plays the steps of the list. A step is `name|keys`: text to type and `{ENTER}`, `{F1}`-`{F24}`, `{PAGEUP}`, `{PAGEDOWN}`, `{TAB}`, `{FIELDEXIT}`, `{ERASEEOF}`, `{WAIT n}`, `{USER}`, `{PASSWORD}`. After each step the screen is written to `target\ibmi\screens\<name>.txt` (24 lines, then the cursor, the input fields and the error line); non-display fields are blanked. `session.log` lists what the client received. The client asks for a US keyboard with code page 37 through the telnet NEW-ENVIRON variables KBDTYPE, CODEPAGE and CHARSET (RFC 4777), since a password with characters that differ between code pages is otherwise not the password typed (PUB400's default is CCSID 273). The password is asked twice, and the run stops when the sign-on is not accepted, when a later screen asks for a password, or when the keyboard stays locked: each wrong sign-on counts toward QMAXSIGN, after which IBM i varies off the device or disables the profile. The same client runs against as4tainr (mono, or `Add-Type` on Windows), so screens can be compared line by line.

To compare, run the same list against as4tainr (a library AS4TRAINR1 and a profile with it as current library first), for example with mono on Linux: `mcs -langversion:5 Tn5250Capture.cs runner.cs` where the runner calls `Tn5250Capture.RunList`, then diff the two folders. The first capture (2026-10-22) set Command Entry's layout (request level, previous commands from column 5 and their messages from column 7 as texts without IDs, "Bottom", a 313-character command line on rows 18-21, nothing on line 24 after a command), the command prompt's layout with keywords shown, prompting for a missing required parameter, CPF6801 on F12, and how titles are centered. The second capture (`screens2.txt`, run with `-Out target\ibmi\screens2`) set the layouts of DSPLIB, DSPLIBL, DSPDTAARA, DSPOBJD, WRKOBJ, WRKSPLF, DSPJOBLOG, DSPMSG and the MAIN menu's header, Command Entry's F10, and CRTDTAARA's prompt control (LEN, VALUE and TEXT appear once TYPE has a value; Enter shows them before the command runs; '>' marks changed values; *CURLIB as a create command's library). The third (`screens3.txt`) set element lists on prompts (CRTDTAARA LEN, CRTPRTF DEV), CRTPRTF's first screen and its additional parameters, a *DEC data area's DSPDTAARA, the layouts of WRKACTJOB, WRKSBMJOB and DSPUSRPRF, and F16 on the system menus. What is still different is in docs/fidelity-gaps.md.

Object names in a list are its own (GOLD*, GLD*), so a run that is stopped halfway (an inquiry message, a dropped connection) leaves objects that only that list uses. A job stopped that way can keep holding locks on them until it is ended (`WRKUSRJOB USER(...) STATUS(*ACTIVE)`, then `ENDJOB`).

## Comparing

```sh
AS4TAINR_BIN=target/debug/as4tainr python3 tools/golden_compare.py \
    tools/golden/basics.txt target/ibmi/golden --setup "CRTLIB AS4TRAINR1"
```

It runs each line through `as4tainr system`, writes `<name>.ours.txt` next to the golden file and lists the lines that differ. Before comparing it masks report header dates, times, product and system name, object creation stamps and PUB400's own page footer (`-=* https://pub400.com *=-`), and it reduces messages to their IDs, because our message texts are written fresh. Use it for printed report layouts.

`tools/golden.py` checks behavior across whole lists:

```sh
AS4TAINR_DATABASE_URL=postgres://... python3 tools/golden.py target/ibmi/golden tools/golden/*.txt
```

It works in a fresh library named like the capture's (`--lib`, default AS4TRAINR1) and a fresh `/home` directory, with `system` being `as4tainr system` (a shell line's files under `/home` are copied into the IFS before each command). For each command it compares the exit status, the message IDs on standard output and on standard error in order, and whether a listing was printed. It never matches message texts; it reports the opposite, a message whose text is the same as IBM's (clean room). `python3 tools/golden.py target/ibmi/golden --audit core/src` looks for IBM's texts in the source: each captured message becomes a pattern with names and numbers taken out, and so does each `"ID: text"` string literal; a match is a text to write again.

Tests built from captures compare IDs, exit codes, values and layouts (core/tests/system_cli.rs, `pase_system_utility_and_printed_reports`) and say "observed on IBM i 7.5" or name the capture in a comment.

### RPG and SQL values

```
powershell -ExecutionPolicy Bypass -File tools\golden\golden.ps1 -List tools\golden\rpg-sql.txt -Files tools\golden\files
```

`tools/golden/files/gldcalc.rpgle` (as4tainr's own test program) writes about 100 results to a table: decimal truncation and half adjust, %DEC, %DECH, %REM, %DIV, %INT(H), %CHAR of decimals, integers and zoned values, every edit code on a negative value and zero, *CURSYM, *ASTFILL and floating currency, edit words, dates and times in each format, %DIFF, %SUBDT, month and year arithmetic. The list adds 25 SQL expressions with RUNSQL and prints the table with CPYF. `-Files` copies the source to `/home/AS4TRAINR/gld` in the same scp as the script; `tools/golden.py --files tools/golden/files` puts it in the sandbox, and `--save DIR` keeps our output of each command for comparing values. First run (2026-10-22): 121 of 122 values the same. PUB400's decimal comma (QDECFMT J) shows in SQL's CHAR of decimals (`-12,50`) and makes `DEC(10,7,2)` a syntax error there (`7,2` reads as one number); written `DEC(10, 7, 2)` it works.

## What the PASE `system` utility does (PUB400, 2026-10-21)

| Case | Standard output | Standard error | Exit status |
| --- | --- | --- | --- |
| Completion messages | `MSGID: text`, one per message | | 0 |
| A display command | its printed report | | 0 |
| An escape message | | `MSGID: text` | 255 |
| Command not found, a parameter not valid | | the diagnostic (CPD0013, CPD0030, CPD0031, CPD0043, CPD0065 for each extra value, CPD0071, CPD0074, CPD0075, CPD0084, CPD0170), then CPF0006 | 255 |
| RUNSQL that fails | | SQL9010 only | 255 |
| `-n` | messages without IDs | messages without IDs | |
| `-v` | the command first, in a line of IBM's own words (as4tainr: `Command: ...`) | | |
| `-s` | printed output left out | | |
| `-q` | no messages | no messages | |
| No command | | a usage line (as4tainr's own text) | 2 |

## Differences that come from PUB400, not from IBM i

PUB400 is a configured system, not a shipped one. Seen so far: QCCSID 273 (shipped 65535), QDATFMT YMD with `-` (shipped MDY with `/`), its own libraries in the library list (PUB400SYS, GAMES400) and QSHELL as a product library, the user's current library, WAITFILE 30 on a new physical file (CRTPF's shipped default is *IMMED), and its page footer. as4tainr follows the shipped values.
