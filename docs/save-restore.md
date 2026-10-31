# Save and restore

as4tainr saves libraries and objects to save files and restores them, with the IBM i commands (`core/src/save.rs`). This gives backups, moving an application from one as4tainr system to another, and the save points that APYJRNCHG's `*LASTSAVE` defaults need (docs/journals.md).

```
CRTSAVF FILE(BACKUP/NIGHTLY)
SAVLIB  LIB(ORDERS) DEV(*SAVF) SAVF(BACKUP/NIGHTLY) CLEAR(*ALL)
DSPSAVF FILE(BACKUP/NIGHTLY)
DLTLIB  LIB(ORDERS)
RSTLIB  SAVLIB(ORDERS) DEV(*SAVF) SAVF(BACKUP/NIGHTLY)
RSTOBJ  OBJ(ORDHDR ORDLIN) SAVLIB(ORDERS) DEV(*SAVF) OBJTYPE(*FILE) SAVF(BACKUP/NIGHTLY) RSTLIB(ORDTEST)
```

## Commands

| Command | |
| --- | --- |
| `CRTSAVF FILE(lib/name) [TEXT]` | A save file: a `*FILE` object with attribute SAVF |
| `CLRSAVF FILE(...)` | Empties it |
| `DSPSAVF FILE(...)` | The save command, when it was saved, and the objects in it (name, library, type, attribute, owner, text) |
| `SAVLIB LIB(lib ...) DEV(*SAVF) SAVF(...) [CLEAR] [UPDHST]` | Every object of the libraries, and the library's SQL procedures and functions |
| `SAVOBJ OBJ(name generic* *ALL ...) LIB(lib ...) DEV(*SAVF) [OBJTYPE(...)] SAVF(...) [CLEAR] [UPDHST]` | The objects named, of the types named; CPF3770 when none are found |
| `SAVCHGOBJ OBJ(...) LIB(lib ...) DEV(*SAVF) [OBJTYPE] [REFDATE] [REFTIME] [OBJJRN] SAVF(...)` | The objects changed since the library's last SAVLIB (`REFDATE(*SAVLIB)`, with UPDHST(*YES); every object when there was none) or since `REFDATE` (the job's date format, MMDDYY) and `REFTIME` (HHMMSS, UTC); OBJJRN(*NO), the default, leaves out files journaled since before then. CPF3770 when nothing changed. RSTOBJ restores what it saved |
| `RSTLIB SAVLIB(lib) DEV(*SAVF) SAVF(...) [RSTLIB(lib)]` | Everything saved from the library, into it or into another (made if it does not exist) |
| `RSTOBJ OBJ(...) SAVLIB(lib) DEV(*SAVF) [OBJTYPE(...)] SAVF(...) [RSTLIB(lib)]` | The objects named; the library restored into must exist (CPF2110) |
| `DLTLIB LIB(lib)`, `CLRLIB LIB(lib)` | Delete a library and everything in it, or everything in it only. System libraries (QSYS, QSYS2, QGPL, QUSRSYS, QHLPSYS) cannot be deleted or cleared; CLRLIB QTEMP clears the job's own QTEMP, which cannot be deleted |
| `DLTF FILE(lib/savf)` | Deletes a save file |
| `OMITOBJ((lib/obj type) ...)`, `SELECT((*INCLUDE\|*OMIT lib/obj type attribute member) ...)` | On the four save and restore commands: OMITOBJ leaves objects out; *INCLUDE entries choose objects (every object when there are none) and *OMIT entries leave them out; names, libraries and attributes can be generic or *ALL, the member must be *ALL (whole files). RSTOBJ FILEMBR((*ALL *ALL)) is taken |

A save file that has data is only saved into again with `CLEAR(*ALL)` or `CLEAR(*REPLACE)` (IBM i would ask; as4tainr ends with CPF9898). Journals and journal receivers are not saved: each is named in a message. A save file is saved with its contents (`SAVFDTA(*NO)`: without them), except the save file being written; a save keeps each object's description (text, owner, authorities, the user-defined attribute QLICOBJD sets, its source). The API QSRLSAVF (format SAVF0200) lists a save file's objects in a user space (docs/fidelity-gaps.md).

## What is saved

For each object, its description (type, attribute, text, owner, *PUBLIC and private authorities) and everything as4tainr knows about it: file descriptions, fields, keys, logical files, display and printer files, source members, programs, modules, service programs, binding directories, commands, menus, data areas, message descriptions, constraints, trigger programs (ADDPFTRG), SQL triggers, journaling. With it go the PostgreSQL objects that hold it: tables with their records (at the same relative record numbers), identity columns and their next values, sequences and their last values, indexes, constraints, views, SQL triggers and their functions. A save is one repeatable-read transaction, so files that change while it runs are saved as they were when it started.

With `UPDHST(*YES)` (the default) each object's last save date is kept, and a journaled file gets an F MS entry in its journal; SAVLIB also keeps the library's save time, where the database's transactions were then, and each file's number of records, which SAVCHGOBJ compares with.

## What counts as changed

An object changes when it is made or restored, when its text, attribute or owner changes, when a data area's value or a user space's contents change, and when a source member is added, changed or removed (the `objects.changed` timestamp, kept by triggers on the catalog tables; RTVOBJD CHGDATE, QUSROBJD and OBJECT_STATISTICS show it). A physical file's records are looked at when SAVCHGOBJ runs: since a SAVLIB, a record written after it (its PostgreSQL `xmin`) or another number of records means changed; since a date, a record committed after it when PostgreSQL keeps commit timestamps (`track_commit_timestamp = on`), else a record written after the last save or catalog initialization before that date (or every file with records, when there was none). Records deleted after a date, with nothing written, are not seen then.

## Restoring over objects that exist

A physical file that exists gets the saved records in place of its own (as `MBROPT(*MATCH)` does); its columns must be the saved ones, else CPF3282. Other files that exist are left as they are; other objects are replaced. A journaled file is journaled again to its journal (when the journal exists) and gets an F MR entry; then `APYJRNCHG JRN(...) FILE(...)` with its defaults applies the changes made since the save.

## Moving a save to another system

A save file is copied to a stream file and back with CPYTOSTMF and CPYFRMSTMF, and in and out of as4tainr with `as4tainr get` and `as4tainr put`:

```sh
as4tainr system "CPYTOSTMF FROMMBR('/QSYS.LIB/BACKUP.LIB/NIGHTLY.FILE') TOSTMF('/tmp/nightly.savf') STMFOPT(*REPLACE)"
as4tainr get /tmp/nightly.savf nightly.savf
# on the other system
as4tainr put nightly.savf /tmp/nightly.savf
as4tainr system "CRTSAVF FILE(QGPL/NIGHTLY)"
as4tainr system "CPYFRMSTMF FROMSTMF('/tmp/nightly.savf') TOMBR('/QSYS.LIB/QGPL.LIB/NIGHTLY.FILE')"
as4tainr system "RSTLIB SAVLIB(ORDERS) DEV(*SAVF) SAVF(QGPL/NIGHTLY)"
```

The save file format is as4tainr's own (gzip-compressed JSON; `"format": "as4tainr save 1"`). It is not the IBM i save format: an IBM i cannot restore it, and as4tainr cannot restore a save file made on an IBM i. To bring an application from an IBM i, use its source (`as4tainr import`, docs/import.md).

What is not there yet is in docs/fidelity-gaps.md (Save and restore).
