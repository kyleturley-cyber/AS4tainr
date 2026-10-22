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

A list has one `name|CL command` per line (run with `system`, standard output and error together) or `name|!shell line` (standard error lines prefixed `2> `). One SSH connection runs the whole list; the script retries when the run is cut short. Results go to `target\ibmi\golden\<name>.txt`. They hold IBM's message texts and are never committed: tests take values from them (message IDs, report layouts, exit statuses) and say so in a comment.

The lists:

- `tools/golden/basics.txt`: display commands and their printed reports (DSPLIBL, DSPSYSVAL, DSPOBJD, DSPMSGD, DSPFD), CHKOBJ, CRTPF RCDLEN, DLTF.
- `tools/golden/system-utility.txt`: the `system` utility itself (options, streams, exit statuses).

## Comparing

```sh
AS4TAINR_BIN=target/debug/as4tainr python3 tools/golden_compare.py \
    tools/golden/basics.txt target/ibmi/golden --setup "CRTLIB AS4TRAINR1"
```

It runs each line through `as4tainr system`, writes `<name>.ours.txt` next to the golden file and lists the lines that differ. Before comparing it masks report header dates, times, product and system name, object creation stamps and PUB400's own page footer (`-=* https://pub400.com *=-`), and it reduces messages to their IDs, because our message texts are written fresh.

## What the PASE `system` utility does (PUB400, 2026-10-21)

| Case | Standard output | Standard error | Exit status |
| --- | --- | --- | --- |
| Completion messages | `MSGID: text`, one per message | | 0 |
| A display command | its printed report | | 0 |
| An escape message | | `MSGID: text` | 255 |
| Command not found, a parameter not valid | | the diagnostic (CPD0030, CPD0084), then CPF0006 | 255 |
| `-n` | messages without IDs | messages without IDs | |
| `-v` | `Command is > command  <` first | | |
| `-s` | printed output left out | | |
| No command | | `Usage: system [-beEhiIkKnOpqsv] command` | 2 |

## Differences that come from PUB400, not from IBM i

PUB400 is a configured system, not a shipped one. Seen so far: QCCSID 273 (shipped 65535), QDATFMT YMD with `-` (shipped MDY with `/`), its own libraries in the library list (PUB400SYS, GAMES400) and QSHELL as a product library, the user's current library, WAITFILE 30 on a new physical file (CRTPF's shipped default is *IMMED), and its page footer. as4tainr follows the shipped values.
