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

- **Source files.** A folder named like a source file (`Q...SRC`) puts its members in that file. Other folders, such as project folders, only group files. Their members go to the usual source file for their type: PF, LF and DSPF to QDDSSRC; RPGLE and SQLRPGLE to QRPGLESRC; CLLE and CLP to QCLSRC.
- **Member names.** Plain names (`CUSTINQ.RPGLE`, any case) and the TOBi convention both work. TOBi is IBM's open-source build tool. Its names look like `CUSTINQ-Customer_inquiry.pgm.rpgle`: the object name, a description that becomes the member text (underscores become blanks), the object type, then the source type.
- **Include files.** `*.rpgleinc` and `*.clleinc` files become stream files under `/<library>/` with their relative path. RPG `/INCLUDE 'header.rpgleinc'` and `/COPY 'x'` find them by name. CL `INCLUDE SRCSTMF('header.clleinc')` does too. RPG `/COPY QRPGLESRC,MEMBER` finds members.
- **Data.** `NAME.csv` loads into the physical file NAME when that file is created by the import. An existing file keeps its records.

## What it does

1. Creates the library if needed (CRTLIB), and each source file (CRTSRCPF). Then it adds every member, replacing a member of the same name.
2. Creates the physical files with `CRTPF FILE(LIB/NAME) SRCFILE(LIB/QDDSSRC) SRCMBR(NAME)` and loads their CSV data.
3. Creates the logical files (CRTLF) and display files (CRTDSPF). Files that exist are kept.
4. Creates the programs: CRTBNDCL (CLLE), CRTCLPGM (CLP) and CRTBNDRPG (RPGLE). These are replaced, since they are built from the source just imported. CL programs are checked when they are created. RPG programs are checked by the runner when they are called.
5. Reports each object, and the members it cannot build yet: SQLRPGLE, RPG III, CMD, PRTF, COBOL and SQL scripts. Their source is kept.

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
  - **Fully free-form RPG (`**FREE`) and free-form declarations.** Jariko does not support them, and most modern code uses them.
  - Embedded SQL (SQLRPGLE).
  - CL pointers and defined storage (`TYPE(*PTR)`, `STG(*DEFINED)`).
  - DDS date and time fields.
  - 27x132 displays.
  - System APIs (QDCXLATE, IFS APIs) and commands such as QSH, CPYTOIMPF and SNDSMTPEMM.

See [fidelity-gaps.md](fidelity-gaps.md).
