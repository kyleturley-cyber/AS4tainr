# Journals and trigger programs

Written from IBM's CL command reference (CRTJRNRCV, CRTJRN, CHGJRN, DLTJRN, DLTJRNRCV, STRJRNPF, ENDJRNPF, DSPJRN, APYJRNCHG, RMVJRNCHG, ADDPFTRG, RMVPFTRG), the journal management topics ("All journal entries by code and type", "Actions of applying or removing journaled changes by journal code"), the Db2 for i services reference (QSYS2.DISPLAY_JOURNAL) and the database programming topics on trigger programs and commitment control.

## Journals

```
CRTJRNRCV JRNRCV(MYLIB/RCV0001)
CRTJRN    JRN(MYLIB/MYJRN) JRNRCV(MYLIB/RCV0001)
STRJRNPF  FILE(MYLIB/ORDHDR MYLIB/ORDDTL) JRN(MYLIB/MYJRN) IMAGES(*BOTH)
```

From then on every change to a journaled file is a journal entry, whatever made it: an RPG WRITE, UPDATE or DELETE, an SQL statement, a CL command. Each entry has a sequence number (per journal), a code and type, the time, the job, user and program that made the change, the commit cycle and the receiver it is in.

| Code, type | Entry |
| --- | --- |
| R PT | A record added |
| R UB, R UP | A record changed: its image before (with `IMAGES(*BOTH)`) and after |
| R DL | A record deleted |
| R DR, R BR, R UR | A ROLLBACK undid a change: a record added was deleted (DR); a record changed was put back (BR its image before the rollback, with `IMAGES(*BOTH)`; UR the image put back); a record deleted was put back (UR) |
| C SC, C CM, C RB | Under commitment control: a commit cycle's start (written with its first change), COMMIT, ROLLBACK. The changes carry the cycle's number (the sequence number of its start); outside commitment control it is 0 |
| F JM, F EJ | Journaling started, ended (ENDJRNPF, or the file deleted) |
| F CR | The member cleared (CLRPFM) |
| F SA, F AY | APYJRNCHG started; changes applied to a member |
| F SR, F RC | RMVJRNCHG started; changes removed from a member |
| J PR | CHGJRN: a receiver was detached |

A change is journaled when it is made. When a ROLLBACK (or an SQL statement that fails under commitment control) takes changes back, their entries stay in the journal and the rollback's own entries (DR, BR, UR, newest first) follow them, then C RB.

An entry's data is the record image in the file's record format: character fields in CCSID 37, packed and zoned decimals, binary integers, dates and times as characters. COUNT_OR_RRN is the record's relative record number, NULL_VALUE_INDICATORS a `0` or `1` per field.

`CHGJRN JRN(...) JRNRCV(*GEN)` attaches a new receiver (the digits at the end of the name counted up: RCV0001, RCV0002) or the one named; `SEQOPT(*RESET)` starts the numbers at 1 again. `DLTJRNRCV` deletes a receiver that is no longer attached, with its entries. `ENDJRNPF FILE(...)` (or `FILE(*ALL) JRN(...)`) ends journaling; `DLTJRN` deletes a journal no file is journaled to.

### Applying and removing journaled changes

```
CLRPFM    FILE(MYLIB/ORDHDR)
APYJRNCHG JRN(MYLIB/MYJRN) FILE(MYLIB/ORDHDR) RCVRNG(*CURCHAIN) FROMENT(*FIRST) TOENT(1234)
RMVJRNCHG JRN(MYLIB/MYJRN) FILE(MYLIB/ORDHDR) RCVRNG(*CURRENT) FROMENT(*LAST) TOENT(1200)
```

APYJRNCHG goes through a file's entries from FROMENT (`*FIRST` or a sequence number) to TOENT (`*LAST` or a sequence number), oldest first: PT and PX add the record at its relative record number, UP and UR change it to the after-image, DL and DR delete it, F CR clears the member; UB and BR are ignored. RMVJRNCHG goes from FROMENT (`*LAST` or a sequence number) back to TOENT (`*FIRST` or a sequence number), newest first: PT and PX delete the record, UB and BR change it to the before-image, DL and DR put it back; UP and UR are ignored; it needs a file journaled with `IMAGES(*BOTH)`, and an F CR in the range cannot be removed (CPF9898; nothing is removed). FROMENTLRG and TOENTLRG are taken as well. RCVRNG is `*CURRENT` (the attached receiver's entries) or `*CURCHAIN` (all). With `CMTBDY(*YES)` (the default) a change made under commitment control is applied or removed only when its commit cycle was committed within the range. Trigger programs are not called, and the changes are not journaled one by one: the journal gets F SA (SR) when the command starts and F AY (RC) for each file. The whole command is one transaction: an entry that cannot be applied (a record that is already there) undoes it.

The IBM defaults work with saves (docs/save-restore.md): SAVOBJ and SAVLIB write an F MS entry for each journaled file they save (with `UPDHST(*YES)`, the default), and RSTOBJ and RSTLIB journal a restored file again and write F MR. `FROMENT(*LASTSAVE)` starts with the entry after the file's last F MS; a file with no save in the journal gets CPF9898. `RCVRNG(*LASTSAVE)` takes the receivers from the save on, which in as4tainr is the whole chain (`*CURCHAIN`). `TOENT(*LASTRST)` is taken as `*LAST`. So the usual recovery is:

```
RSTOBJ    OBJ(ORDHDR) SAVLIB(MYLIB) DEV(*SAVF) SAVF(BACKUP/NIGHTLY)
APYJRNCHG JRN(JRNLIB/MYJRN) FILE(MYLIB/ORDHDR)
```

Without a save, clear the file and apply its entries from the first (`FROMENT(*FIRST) RCVRNG(*CURCHAIN)`).

### Reading the entries

`DSPJRN JRN(MYLIB/MYJRN) [FILE(...)] [RCVRNG(*CURCHAIN)] [JRNCDE(...)] [ENTTYP(...)]` lists them. In SQL, `QSYS2.DISPLAY_JOURNAL` has IBM's 23 parameters and 54 result columns:

```sql
SELECT ENTRY_TIMESTAMP, JOURNAL_ENTRY_TYPE, SUBSTR("OBJECT", 1, 10) AS FILE,
       INTERPRET(SUBSTR(ENTRY_DATA, 1, 4) AS DECIMAL(7, 0)) AS ORDNO
  FROM TABLE(QSYS2.DISPLAY_JOURNAL('MYLIB', 'MYJRN',
             STARTING_RECEIVER_NAME => '*CURCHAIN', JOURNAL_CODES => 'R')) J
```

STARTING_RECEIVER_NAME is `*CURRENT` (the default: the receiver attached now), `*CURCHAIN` (and `*CURAVLCHN`, `*CURSEQCHN`: every receiver), or a receiver; ENDING_RECEIVER_NAME likewise. The other filters: STARTING_ and ENDING_SEQUENCE and _TIMESTAMP, JOURNAL_CODES (`*ALL`, `*CTL`, or a list), JOURNAL_ENTRY_TYPES (`*ALL`, `*RCD`, or a list), OBJECT_LIBRARY and OBJECT_NAME (up to 300 names), USER, JOB, PROGRAM and COMMIT_CYCLE. `INTERPRET(bytes AS type)` reads an image's bytes as DECIMAL (packed), NUMERIC (zoned), SMALLINT, INTEGER, BIGINT, CHAR, VARCHAR or BINARY.

## Trigger programs

```
ADDPFTRG FILE(MYLIB/ORDHDR) TRGTIME(*AFTER) TRGEVENT(*INSERT) PGM(MYLIB/ORDTRG)
```

The program is called for each record added (`*INSERT`), changed (`*UPDATE`; with `TRGUPDCND(*CHANGE)` only when a value changed) or deleted (`*DELETE`), `*BEFORE` or `*AFTER` the change, whether RPG, SQL or CL made it. It gets two parameters: the trigger buffer and its length. The buffer has the file, library and member names, the event (`1` insert, `2` delete, `3` update), the time (`1` after, `2` before), the commit lock level (`0` without commitment control, `1` *CHG), the CCSID (37), the relative record number, and the offsets and lengths of the original record, its null byte map, the new record and its null byte map, which follow the first 96 bytes. A program reads an image with a data structure over a pointer:

```
dcl-ds NewRec extname('ORDHDR') prefix(N_) based(pNew) end-ds;
pNew = %addr(pBuf) + pBuf.NewOff;
```

The program runs in the job, in the database session of the change: it sees the record the change wrote before that is committed, and its own record changes (to files it does not open under commitment control) are part of the caller's transaction, committed or rolled back with it. A `*BEFORE` program runs before the change is made; with `ALWREPCHG(*YES)` it may change the new record in the buffer, and the record is written as it left it (later `*BEFORE` programs and the `*AFTER` programs see it so). The program's record changes are journaled with its name.

If the program ends in error (an escape message, QMHSNDPM `*ESCAPE`), the operation fails: an RPG operation with CPF502B (file status 01023 before, 01024 after), an SQL statement with SQL0443. What happens to the files follows IBM's commitment control rules for trigger programs. Without commitment control, nothing is rolled back: a `*BEFORE` program's error means the change is not made, an `*AFTER` program's means it stays, and the program's own changes up to the error stay. Under commitment control the change and the program's changes are taken back (and the rollback is journaled).

`RPLTRG(*YES)` replaces a trigger of the same name, or of the same time and event; `TRG` names it (default: file, time and event). `RMVPFTRG FILE(...) [TRGTIME] [TRGEVENT] [TRG]` removes triggers (all by default).

The COM order management application (`tools/legacy.sh`) uses both: its ORDTRG trigger program writes an audit row for every order change, and its ORDAUDJV view shows the same audit from the journal.

See [fidelity-gaps.md](fidelity-gaps.md) for what is not done.

## SQL tables and QSQJRN

CREATE SCHEMA makes the journal QSQJRN and its receiver QSQJRN0001 in the schema (Db2 for i SQL Reference, CREATE SCHEMA). A table made by SQL (CREATE TABLE, also AS and LIKE) in a library that has a journal named QSQJRN is journaled to it right away, with IMAGES(*BOTH) and OMTJRNE(*OPNCLO), as PUB400 showed (2026-10-27, tools/golden/verify4.txt y_to_jrn); RUNSQLSTM's listing then gives SQL7950 instead of SQL7905. UPDATE and DELETE of a system-period temporal table need the table and its history table journaled (SQL7008).
