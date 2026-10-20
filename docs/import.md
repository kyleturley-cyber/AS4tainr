# Importing an application

`as4tainr import` brings an application's source and data into a library and builds its objects, the way an operator would with the IBM i create commands:

```sh
as4tainr import path/to/app --lib MYAPP
```

In Docker: `docker compose exec as4tainr as4tainr import /path/in/container --lib MYAPP`. Mount or copy the directory into the container first.

## The directory

The layout is the one used to keep IBM i source in git:

```
app/
  QDDSSRC/CUSTMAST.PF            one folder per source file,
  QDDSSRC/CUSTINQ.DSPF           one stream file per member: MEMBER.TYPE
  QRPGLESRC/CUSTINQ.RPGLE
  QCLSRC/START.CLLE
  includes/header.rpgleinc       include files (any folder)
  data/CUSTMAST.csv              initial records (header row = field names)
```

- **Source files.** A folder named like a source file (`Q...SRC`) puts its members in that file. Other folders, such as project folders, only group files. Their members go to the usual source file for their type: PF, LF and DSPF to QDDSSRC; RPGLE and SQLRPGLE to QRPGLESRC; CLLE and CLP to QCLSRC; SQL scripts (`.sql`, `.table`, `.view`, `.index`) to QSQLSRC; binder language (`.bnd`) to QSRVSRC; binding directory scripts (`.bnddir`) to QBNDDIRSRC.
- **Member names.** Plain names (`CUSTINQ.RPGLE`, any case) and the TOBi convention both work. TOBi is IBM's open-source build tool. Its names look like `CUSTINQ-Customer_inquiry.pgm.rpgle`: the object name, a description that becomes the member text (underscores become blanks), the object type, then the source type.
- **Include files.** `*.rpgleinc` and `*.clleinc` files become stream files under `/<library>/` with their relative path. RPG `/INCLUDE 'header.rpgleinc'` and `/COPY 'x'` find them by name. CL `INCLUDE SRCSTMF('header.clleinc')` does too. RPG `/COPY QRPGLESRC,MEMBER` finds members.
- **Data.** `NAME.csv` loads into the physical file NAME when that file is created by the import, from DDS or by an SQL script. An existing file keeps its records.

## What it does

1. Creates the library if needed (CRTLIB), and each source file (CRTSRCPF). Then it adds every member, replacing a member of the same name.
2. Creates the physical files with `CRTPF FILE(LIB/NAME) SRCFILE(LIB/QDDSSRC) SRCMBR(NAME)` and loads their CSV data.
3. Runs the SQL scripts as `RUNSQLSTM ... ERRLVL(30)` would: every statement runs, and the ones that fail are reported. Scripts that create tables run first. Everything goes to the import library: it is the default schema, and `SET SCHEMA` statements are skipped. A script with failing statements counts as an object that could not be created.
4. Creates the logical files (CRTLF) and display files (CRTDSPF). Files that exist are kept.
5. Creates modules, service programs and binding directories (ILE). A source is a module when the repository's `Rules.mk` says so (`NAME.MODULE: source`), when its name says so (`NAME.module.rpgle`), or when it is `NOMAIN`. `NAME.bnd` (binder language, kept in QSRVSRC) makes a service program from the modules its `Rules.mk` rule lists, or from the module of the same name. `NAME.bnddir` lists CL commands (`CRTBNDDIR`, `ADDBNDDIRE`, with `&O` for the library and `&N` for the name, as in IBM's Bob build tool); a command starting with `!` may fail.
6. Creates the programs: CRTBNDCL (CLLE), CRTCLPGM (CLP), CRTBNDRPG (RPGLE), CRTSQLRPGI (SQLRPGLE) and CRTRPGPGM (RPG, that is RPG/400). These are replaced, since they are built from the source just imported. CL programs are checked when they are created. RPG programs are compiled (checked) when they are created, as CRTBNDRPG does.
7. Reports each object, and the members it cannot build yet: CMD, PRTF and COBOL. Their source is kept.

The import exits with status 1 if any object could not be created. The report says why.

The same commands work on a 5250 command line once the source is in a library, for example `CRTBNDRPG PGM(MYAPP/CUSTINQ)`. `DSPFD MYAPP/QRPGLESRC` lists the members of a source file, and `DSPPFM MYAPP/QRPGLESRC MBR(CUSTINQ)` shows one.

## Running it

RPG programs created with CRTBNDRPG are program objects: CALL finds them through the library list like any other program, and their source comes from the library. `samples/apps/contacts` is a small example that `scripts/samples.sh` imports into CONTACTS:

```
ADDLIBLE CONTACTS
CALL CONTACTS
```

## Testing with real-world source

`tools/corpus.sh` clones a public repository of IBM i source and imports it. The default is Nick Litten's public IBM i examples (github.com/NickLitten/nick.litten.public). That repository has no license file, so it is cloned at test time and never added to this repository. What it showed on Oct 16, 2026:

- **Runs unmodified:** fixed-form AS/400-style subfile programs (SIMPLESFL1, NOOBSFL1) and the CONTLST-style READ loop.
- **Fixed by as4tainr along the way:**
  - comment lines in DDS without the form type;
  - `//` comments in columns 1-5;
  - quoted `/INCLUDE` files;
  - CL `INCLUDE`, DCLPRCOPT and COPYRIGHT;
  - the end-of-file and not-found indicators on READ and CHAIN;
  - `*INKC` and the other function key indicators;
  - a field named CTID (a PostgreSQL system column name);
  - zoned and packed fields with the same name.
- **Not yet:**
  - **Fully free-form RPG (`**FREE`) and free-form declarations.** Jariko did not support them; this is why as4tainr got its own RPG engine, which does (docs/rpg-engine.md).
  - CL pointers and defined storage (`TYPE(*PTR)`, `STG(*DEFINED)`).
  - DDS date and time fields.
  - 27x132 displays.
  - System APIs (QDCXLATE, IFS APIs) and commands such as QSH, CPYTOIMPF and SNDSMTPEMM.

See [fidelity-gaps.md](fidelity-gaps.md).
