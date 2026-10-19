# SQL

as4tainr runs SQL in the DB2 for i dialect against the same files RPG programs use: a physical or logical file created from DDS is a table (or view) to SQL, and a change made one way is seen the other way at once.

| Where | How |
| --- | --- |
| 5250 session | `STRSQL`: type a statement, press Enter. A SELECT shows its rows on a Display Data screen (Page Up/Down, F19/F20 to shift wide results); other statements report how many rows they changed. F9 retrieves the previous statement, F3 ends. |
| Command line | `RUNSQL SQL('UPDATE CUSTMAST SET CUSBAL = 0 WHERE CUSNO = 3')`: one statement that does not return rows (as on IBM i, RUNSQL does not run SELECT). |
| Scripts | `RUNSQLSTM SRCFILE(LIB/QSQLSRC) SRCMBR(NAME)`: the statements of a source member, separated by semicolons (`--` and `/* */` comments). A statement that fails stops the run with SQL9010 unless `ERRLVL(30)` (SQL errors have severity 30), which runs every statement and reports the ones that failed. `DFTRDBCOL(LIB)` is the default schema. SELECT is not valid in a script. |
| Shell | `as4tainr sql "SELECT * FROM QGPL/CUSTMAST"`, with the library list from `AS4TAINR_LIBL`. In Docker: `docker compose exec as4tainr as4tainr sql "..."`. |

## The dialect

Written from IBM's public DB2 for i SQL reference.

| DB2 for i | Supported |
| --- | --- |
| Statements | SELECT (with joins, GROUP BY, ORDER BY, `FETCH FIRST n ROWS ONLY`, ...), WITH, VALUES, INSERT, UPDATE, DELETE, and the data definition below. One statement at a time. |
| Naming | System naming `LIB/FILE` and SQL naming `LIB.FILE` are both accepted. Unqualified names are found through the job's library list. Unquoted names are not case sensitive; quoted names are read as upper case. |
| Special registers | `CURRENT DATE`, `CURRENT TIME`, `CURRENT TIMESTAMP` |
| Operators and functions | `CONCAT` (and `\|\|`), `UCASE`, `LCASE`, `RRN(t)`, plus the functions DB2 and PostgreSQL share (`SUBSTR`, `TRIM`, `UPPER`, `COALESCE`, `COUNT`, `SUM`, ...) |
| Tables | Physical and logical files; `SYSIBM.SYSDUMMY1` |
| Clauses accepted and ignored | `WITH NC\|UR\|CS\|RS\|RR`, `OPTIMIZE FOR n ROWS` |
| `VALUES 1, 2` | One row per expression |
| `INSERT INTO file VALUES (...)` | Without a column list, values go to the file's fields in order |
| Record number | Not a column: `SELECT *` does not show it. `RRN(t)` returns it. |
| Unnamed result columns | Named by their position (1, 2, ...) |
| Errors | `SQL0204` not found, `SQL0206` unknown column, `SQL0104` statement not valid, `SQL0803` duplicate key, `SQL0551` not authorized, `SQL0084` statement not supported, `SQL0302` value does not fit, `SQL0901` other |

Each statement is committed on its own (`COMMIT(*NONE)`). Timestamps are shown as Db2 does (`2026-10-19-14.05.30.000000`); dates as `YYYY-MM-DD` and times as `HH:MM:SS`.

## Embedded SQL in RPG

RPG programs with `EXEC SQL` (SQLRPGLE, created with CRTSQLRPGI) run their statements on this layer, as the same restricted login, with the program's library list as the search path. Each program has its own connection for the call. Host variables are put into the statement as values (character values quoted and escaped, numbers exact); identifiers never come from host variables. Cursors read all their rows when they are opened. The SQLCA (SQLCODE, SQLSTATE, SQLERRMC, SQLERRD) is set after each statement. See docs/rpg-engine.md for the statements supported.

## Data definition

Written from the Db2 for i SQL Reference. A table is a physical file and an index a keyed logical file, in the same catalog as files created from DDS: RPG programs use them like any other file, and DSPPFM, WRKOBJ and the library list see them.

| Statement | Supported |
| --- | --- |
| `CREATE [OR REPLACE] TABLE` | Columns of type CHAR, VARCHAR, SMALLINT, INTEGER, BIGINT, DECIMAL, NUMERIC, DATE, TIME, TIMESTAMP; `NOT NULL` (other columns are null-capable); `DEFAULT value`, `WITH DEFAULT` (blanks, zero, or the current date, time or timestamp); `CCSID n` (accepted); `PRIMARY KEY` and `UNIQUE` (a duplicate is SQL0803, or CPF5026 for a WRITE); `CHECK` comparing the table's columns with constants (SQL0545, or CPF502F for a WRITE); `CONSTRAINT name`; `RCDFMT name` (the record format; the table name otherwise). OR REPLACE drops the old table and the indexes over it. |
| `CREATE [UNIQUE] INDEX name ON table (col [ASC\|DESC], ...) [RCDFMT name]` | A keyed logical file over the table, with all its fields |
| `DROP TABLE\|INDEX\|VIEW [IF EXISTS] name` | DROP TABLE drops the indexes over it too |
| `LABEL ON TABLE\|INDEX\|VIEW name IS 'text'` | The object's text |
| `LABEL ON COLUMN t.c TEXT IS 'text'`, `LABEL ON [COLUMN] t (c TEXT IS 'text', ...)` | The field's text. Column headings (`IS` without `TEXT`) are accepted and not kept. |
| `COMMENT ON ...` | Accepted, not kept |
| `SET SCHEMA name` | The default schema for unqualified names, for the rest of the STRSQL session or script |

New objects go to the library named, else the default schema, else the current library, else QGPL (system naming). Names are system names: 10 characters at most. Tables get the owner and authorities CRTPF gives a file; DROP needs *ALL authority to the file (SQL0551).

The statements are parsed by as4tainr and the PostgreSQL objects built from the parsed description; no part of the statement text is passed on. Not supported yet: names longer than 10 characters, ALTER TABLE, CREATE VIEW, identity and other generated columns, FOREIGN KEY, the other data types (GRAPHIC, BINARY, CLOB, BLOB, FLOAT, DECFLOAT, XML, BOOLEAN, ...), TIMESTAMP precisions other than 6, procedures, functions, triggers, sequences and compound statements (BEGIN ... END). See fidelity-gaps.md.

## Security

User SQL never runs as the database user as4tainr itself uses. `as4tainr init` creates a PostgreSQL login role, `as4tainr_sql`, that is not a superuser, has a random password (kept in the catalog) and may only use the library schemas: it cannot read the catalog (user profiles and password hashes), create or drop objects, switch to another role, or call server functions. Every statement opens a connection that logs in as that role. Statements that name the catalog or PostgreSQL's own schemas are also refused before they run, with a clearer message.

`as4tainr init` needs a database user who may create roles (the Docker and CI setups use one). Without one, as4tainr still runs, and SQL statements are refused with a message saying so.

## Not supported yet

The data definition listed above as not supported yet, commitment control (COMMIT, ROLLBACK, `COMMIT(*CHG)`), the QSYS2 catalog views, `SELECT *` in `INSERT ... SELECT` (it includes the record number; list the columns), DB2-only scalar functions beyond the ones above, and special registers other than the dates and times. Object authority is not checked: anyone who can sign on can read and change every file with SQL, as with record-level access. See [fidelity-gaps.md](fidelity-gaps.md).
