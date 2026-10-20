# Data areas

A data area (`*DTAARA`) is a small named value kept in a library, which programs share: the next order number, a company name, a switch. as4tainr implements data areas from IBM's public command and RPG documentation.

## Commands

| Command | What it does |
| --- | --- |
| `CRTDTAARA DTAARA(lib/name) TYPE(*DEC\|*CHAR\|*LGL) LEN(...) VALUE(...) TEXT(...)` | Creates a data area. The library defaults to the current library. `LEN(*DFT)` is 32 for `*CHAR`, `(15 5)` for `*DEC` and 1 for `*LGL`; `VALUE(*DFT)` is blanks, zero or `'0'` |
| `CHGDTAARA DTAARA(name) VALUE(...)` | Changes the whole value |
| `CHGDTAARA DTAARA(name (start length)) VALUE(...)` | Changes part of a `*CHAR` data area; the value is padded with blanks to the length |
| `DSPDTAARA DTAARA(name)` | Shows the type, length, text and value |
| `DLTDTAARA DTAARA(name)` | Deletes it |
| `RTVDTAARA DTAARA(name [(start length)]) RTNVAR(&var)` | In CL programs: copies the value (or part of a `*CHAR` value) into a variable. A `*DEC` data area needs a `*DEC` variable |

Limits: `*CHAR` up to 2000 characters, `*DEC` up to 24 digits with up to 9 decimal positions. A `*DEC` value that does not fit the length, or has more decimal positions than the data area, is refused rather than rounded.

Messages: `CPF1015` (data area not found), `CPF1023` (data area already exists), `CPF2110` (library not found).

`WRKOBJ OBJTYPE(*DTAARA)` and `CHKOBJ OBJTYPE(*DTAARA)` see data areas like any other object.

## RPG

The usual fixed-form pattern:

```
     D NXTORD          S              7  0
     C     *DTAARA       DEFINE    ORDNXT        NXTORD
     C     *LOCK         IN        NXTORD
     C                   EVAL      NXTORD = NXTORD + 1
     C                   OUT       NXTORD
```

`IN` reads the data area named in `DEFINE` through the program's library list. A numeric field gets a `*DEC` value as a number, and a character field or data structure gets the text. `OUT` writes the field back. A value that does not fit the data area fails (or turns on the error indicator in positions 73-74).

## Locks

`IN *LOCK` locks the data area for the program until `OUT`, or until the program ends. While it is locked, `CHGDTAARA` from another job, or `IN *LOCK` in another program, waits. A counter read under `*LOCK` and written with `OUT` is never changed in between, so two order-entry sessions never get the same order number.

Under the covers, the lock is a PostgreSQL advisory lock on the program's database session. It is released when the session ends, which is how IBM i releases locks at the end of a job.

## The local data area

Each job has a local data area, `*LDA`: 1024 characters, blanks when an interactive job starts; SBMJOB gives the new job a copy of the submitting job's. `CHGDTAARA DTAARA(*LDA (1 10)) VALUE(...)`, `RTVDTAARA DTAARA(*LDA (1 10)) RTNVAR(&X)` and `DSPDTAARA *LDA` work on it, and RPG programs read and write it with a data area data structure (U in position 23 without a name, or `DTAARA(*LDA)`) or `IN`/`OUT`. It is kept as bytes (`as4tainr.local_data_areas`, by job number), so packed and binary subfields keep their exact contents.

## Storage

Each data area is an object row (`*DTAARA`) plus a row in `as4tainr.data_areas` (type, length, decimals, value). `*DEC` values are stored as exact decimal text, never as floating point.

## Not yet

- The group data area `*GDA` and the program initialization parameter data area `*PDA`: as4tainr has no group jobs or prestart jobs.
- `UNLOCK *DTAARA`. (`DEFINE` and the `DTAARA` keyword on definitions both work.)
- `RTVDTAARA` into a `*CHAR` variable from a `*DEC` data area. IBM i allows it in some cases; as4tainr refuses it with a clear message.
- How long `CHGDTAARA` waits for a lock: it waits until the lock is released, with no time limit.
