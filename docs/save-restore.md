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
| `RSTLIB SAVLIB(lib) DEV(*SAVF) SAVF(...) [RSTLIB(lib)]` | Everything saved from the library, into it or into another (made if it does not exist) |
| `RSTOBJ OBJ(...) SAVLIB(lib) DEV(*SAVF) [OBJTYPE(...)] SAVF(...) [RSTLIB(lib)]` | The objects named; the library restored into must exist (CPF2110) |
| `DLTLIB LIB(lib)`, `CLRLIB LIB(lib)` | Delete a library and everything in it, or everything in it only. System libraries (QSYS, QSYS2, QGPL, QTEMP, QUSRSYS, QHLPSYS) cannot be deleted or cleared |
| `DLTF FILE(lib/savf)` | Deletes a save file |

A save file that has data is only saved into again with `CLEAR(*ALL)` or `CLEAR(*REPLACE)` (IBM i would ask; as4tainr ends with CPF9898). Journals, journal receivers and save files are not saved: each is named in a message.

## What is saved

For each object, its description (type, attribute, text, owner, *PUBLIC and private authorities) and everything as4tainr knows about it: file descriptions, fields, keys, logical files, display and printer files, source members, programs, modules, service programs, binding directories, commands, menus, data areas, message descriptions, constraints, trigger programs (ADDPFTRG), SQL triggers, journaling. With it go the PostgreSQL objects that hold it: tables with their records (at the same relative record numbers), identity columns and their next values, sequences and their last values, indexes, constraints, views, SQL triggers and their functions. A save is one repeatable-read transaction, so files that change while it runs are saved as they were when it started.

With `UPDHST(*YES)` (the default) each object's last save date is kept, and a journaled file gets an F MS entry in its journal.

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
