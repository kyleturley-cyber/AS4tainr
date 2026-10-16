# Object authority

Who may use which object. It is written from IBM's public security reference.

## The rules

- **Owner:** every object has one, the user who created it. Objects created with the `ibmish` CLI (the sample files, `CRTBNDCL`) and system objects belong to QSECOFR.
- ***PUBLIC:** every object has a *PUBLIC authority. New objects get `*CHANGE`, which is the shipped value of system value `QCRTAUT`.
- **Private authorities:** named users can have their own authority to an object.
- **Levels:** `*ALL`, `*CHANGE`, `*USE` and `*EXCLUDE`.

A user's authority is found in this order, and the first one that applies wins:

1. Special authority `*ALLOBJ` (QSECOFR, user class `*SECOFR`): everything.
2. The owner: `*ALL`.
3. A private authority, including `*EXCLUDE`, which keeps the user out even when *PUBLIC may use the object.
4. *PUBLIC.

| To | Needs |
| --- | --- |
| Read a file, open a display file, call a CL program, read a data area (`RTVDTAARA`, `DSPDTAARA`, RPG `IN`) | `*USE` |
| Open a file for update, `WRITE`/`UPDATE`/`DELETE` records, change a data area (`CHGDTAARA`, RPG `OUT`) | `*CHANGE` |
| Delete an object (`DLTDTAARA`), change its authorities or owner | `*ALL` (or being the owner) |

Without the authority needed, the operation fails with `CPF9802`. In an RPG program the program ends with that error; a CL program can monitor for it.

## Commands

| Command | What it does |
| --- | --- |
| `GRTOBJAUT OBJ(lib/obj) OBJTYPE(*FILE) USER(name *PUBLIC ...) AUT(*USE)` | Gives users, or *PUBLIC, an authority (default `*CHANGE`) |
| `RVKOBJAUT OBJ(...) OBJTYPE(...) USER(name)` | Removes a user's private authority (they fall back to *PUBLIC). For `*PUBLIC`, the public authority becomes `*EXCLUDE` |
| `DSPOBJAUT OBJ(...) OBJTYPE(...)` | Shows the owner, private authorities and *PUBLIC |
| `CHGOBJOWN OBJ(...) OBJTYPE(...) NEWOWN(name)` | Gives the object to another user |

Object types are `*FILE`, `*PGM`, `*DTAARA`, `*MENU` and `*LIB`. These commands are on the F4 menu under "Security commands".

## Example

```
GRTOBJAUT OBJ(CUSTMAST) OBJTYPE(*FILE) USER(*PUBLIC) AUT(*USE)
GRTOBJAUT OBJ(CUSTMAST) OBJTYPE(*FILE) USER(BOB) AUT(*CHANGE)
DSPOBJAUT OBJ(CUSTMAST) OBJTYPE(*FILE)
```

Now everyone can read customers, but only BOB (and QSECOFR, with `*ALLOBJ`) can change them. Anyone else's `CALL CUSTMNT` ends with `CPF9802` when it opens CUSTMAST for update.

## Not yet

- **Authority settings:** authorization lists (`*AUTL`), group profiles, specific authorities (`*OBJOPR`, `*READ`, `*ADD`, ...), and `CRTAUT` per library.
- **Libraries:** a library's own authority is not checked when its objects are used or created.
- **RPG programs** are found as source and are not objects yet, so `CALL` of an RPG program is not checked. CL programs are.
- **SQL** (`STRSQL`, `RUNSQL`) runs with the restricted SQL login's rights, not the user's object authorities.
- **`RVKOBJAUT`** removes the whole private authority instead of only the authority named.
