# Known fidelity gaps

These are places where ibmish knowingly behaves differently from IBM i. Each one should eventually turn into a fix with a test, or into a documented decision.

| Area | IBM i behavior | ibmish today | Impact |
| --- | --- | --- | --- |
| Key collating sequence | Character keys are ordered by EBCDIC: lowercase before uppercase, and letters before digits | Byte order of UTF-8 text (`COLLATE "C"`): digits before uppercase before lowercase | Mixed letter/digit or mixed-case keys read in a different order. Planned fix: an EBCDIC sort key per character key field |
| Record locking | Input/update files lock the record read for update until UPDATE, the next read, or UNLOCK | No locks | Concurrent update jobs can overwrite each other. Planned: `SELECT ... FOR UPDATE` within the job's commitment scope |
| Commitment control | `COMMIT`/`ROLBK` under journaling | Each operation autocommits | No rollback of partial work |
| Field types | A, P, S, B, I, U, F, L, T, Z, G, O, H... | A, P, S only | Files using other types are rejected at CRTPF |
| Logical files | Keyed/selected views over physical files, joins | Not yet | Programs that open LFs fail with CPF9812 |
| CHAIN not found | No current record; next READ position is not well defined in all cases | The next READ starts at the searched key | Programs that rely on this undefined behavior may differ |
| Object names | National characters allowed in some CCSIDs | ASCII A–Z, 0–9, `$#@_.` only | |
| Null-capable fields | `ALWNULL` | Not supported | |
| Character data | EBCDIC CCSIDs (37, 500, ...) | UTF-8 in PostgreSQL | Hex constants and byte-level tricks (`%BITAND` on text) differ |
| Jariko DSPLY output | DSPLY writes to the job's external message queue | Jariko prints the message on standard output | Fine for tests; operator messages come later |
| 5250 sessions | Many session and device options | 24x80 only; no TLS (992), no NEW-ENVIRON auto sign-on or device naming | Use only on trusted networks for now; telnet sends passwords in clear text |
| Sign-on security | QMAXSIGN / QMAXSGNACN can disable the profile or the device | The connection closes after 3 failures; profiles are never disabled | Repeated guessing is slowed but not blocked across connections |
| Command entry | F4 prompting, help, job log, message queues | Commands typed in full; messages shown in the session log only | |
| Display files | Full DDS for display files | Common subset (see display-files.md): no subfiles (`SFL`/`SFLCTL`), `OVERLAY`/`PUTOVR` (every EXFMT clears the screen), `WINDOW`, `REF`/`REFFLD`, `VALUES`/`RANGE`/`COMP`, `EDTWRD`, help, `INDARA` | Programs using subfiles or windows cannot run yet; validity checking keywords are not enforced |
| Display I/O operations | EXFMT, WRITE + READ, READC on subfiles | EXFMT only (through Jariko's callback) | Separate WRITE/READ to a display file are not supported yet |
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
