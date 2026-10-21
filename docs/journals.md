# Journals and trigger programs

Written from IBM's CL command reference (CRTJRNRCV, CRTJRN, CHGJRN, DLTJRN, DLTJRNRCV, STRJRNPF, ENDJRNPF, DSPJRN, ADDPFTRG, RMVPFTRG), the Db2 for i services reference (QSYS2.DISPLAY_JOURNAL) and the database programming topics on trigger programs.

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
| C SC, C CM, C RB | Under commitment control: a commit cycle's start (written with its first change), COMMIT, ROLLBACK. The changes carry the cycle's number (the sequence number of its start); outside commitment control it is 0 |
| F JM, F EJ | Journaling started, ended (ENDJRNPF, or the file deleted) |
| J PR | CHGJRN: a receiver was detached |

An entry's data is the record image in the file's record format: character fields in CCSID 37, packed and zoned decimals, binary integers, dates and times as characters. COUNT_OR_RRN is the record's relative record number, NULL_VALUE_INDICATORS a `0` or `1` per field.

`CHGJRN JRN(...) JRNRCV(*GEN)` attaches a new receiver (the digits at the end of the name counted up: RCV0001, RCV0002) or the one named; `SEQOPT(*RESET)` starts the numbers at 1 again. `DLTJRNRCV` deletes a receiver that is no longer attached, with its entries. `ENDJRNPF FILE(...)` (or `FILE(*ALL) JRN(...)`) ends journaling; `DLTJRN` deletes a journal no file is journaled to.

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

If the program ends in error (an escape message, QMHSNDPM `*ESCAPE`), the change is undone: an RPG operation fails with CPF502B (file status 01023 before, 01024 after), an SQL statement with SQL0443. `RPLTRG(*YES)` replaces a trigger of the same name, or of the same time and event; `TRG` names it (default: file, time and event). `RMVPFTRG FILE(...) [TRGTIME] [TRGEVENT] [TRG]` removes triggers (all by default).

The COM order management application (`tools/legacy.sh`) uses both: its ORDTRG trigger program writes an audit row for every order change, and its ORDAUDJV view shows the same audit from the journal.

See [fidelity-gaps.md](fidelity-gaps.md) for what is not done.
