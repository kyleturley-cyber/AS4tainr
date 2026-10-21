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

Outside commitment control each statement is committed on its own (`COMMIT(*NONE)`). Timestamps are shown as Db2 does (`2026-10-19-14.05.30.000000`); dates as `YYYY-MM-DD` and times as `HH:MM:SS`.

## Procedures and functions

Written from the Db2 for i SQL Reference (CREATE PROCEDURE (SQL), CREATE FUNCTION (SQL scalar), CALL, and the SQL control statements).

| Statement | Supported |
| --- | --- |
| `CREATE [OR REPLACE] PROCEDURE name ([IN\|OUT\|INOUT] p type [DEFAULT x], ...) options body` | Options: LANGUAGE SQL, SPECIFIC, [NOT] DETERMINISTIC, MODIFIES/READS SQL DATA, CONTAINS SQL, DYNAMIC RESULT SETS n, COMMIT ON RETURN, SET OPTION, PROGRAM TYPE, and the other options Db2 for i takes (accepted). The body is one statement, usually `BEGIN ... END` |
| `CREATE [OR REPLACE] FUNCTION name (p type, ...) RETURNS type options body` | Scalar functions: `RETURN expression`, or `BEGIN ... END` with DECLARE, SET, IF, CASE, WHILE, REPEAT, LOOP, LEAVE, ITERATE, SELECT INTO, VALUES INTO, SIGNAL and RETURN. Used in any statement like a built-in function |
| `CALL name (arguments)` | From STRSQL, RUNSQL, RUNSQLSTM, `as4tainr sql`, embedded SQL (host variables receive OUT and INOUT parameters) and from other procedures. `?` (or any argument) for an OUT parameter, `DEFAULT` for a parameter's default; parameters left out at the end take their defaults. STRSQL shows a procedure's first result set, or its OUT parameters |
| `CREATE PROCEDURE name (...) LANGUAGE RPGLE\|CL\|... EXTERNAL NAME lib/pgm PARAMETER STYLE GENERAL` | An external procedure: CALL calls the program with the parameters by reference; OUT and INOUT parameters are what the program left in them |
| `DROP PROCEDURE\|FUNCTION [IF EXISTS] name` | |
| `BEGIN ... END` by itself | A dynamic compound statement: runs like a procedure body without parameters, from STRSQL, RUNSQL and RUNSQLSTM |
| Compound statement | `BEGIN [NOT] ATOMIC ... END` with `DECLARE` variables (with DEFAULT; SQLSTATE and SQLCODE variables are kept up to date), conditions (`FOR SQLSTATE`), cursors (`WITH RETURN`: left open, a result set), handlers (`CONTINUE`, `EXIT`, `UNDO` for SQLEXCEPTION, SQLWARNING, NOT FOUND, an SQLSTATE or a condition; the most specific one runs) |
| Control statements | `SET` (also `SET (a, b) = (x, y)`), `IF ... ELSEIF ... ELSE ... END IF`, `CASE` (simple and searched; no WHEN true and no ELSE is SQLSTATE 20000), `WHILE`, `REPEAT ... UNTIL`, `LOOP`, `FOR v AS query DO ... END FOR` (columns as `v.col` or `col`), labels with `LEAVE` and `ITERATE`, `RETURN`, `SIGNAL`/`RESIGNAL` with `MESSAGE_TEXT` (SQLCODE -438 with that SQLSTATE), `GET DIAGNOSTICS` (ROW_COUNT; EXCEPTION 1 MESSAGE_TEXT, RETURNED_SQLSTATE, DB2_RETURNED_SQLCODE), `OPEN`, `FETCH ... INTO`, `CLOSE`, `CALL`, `COMMIT`, `ROLLBACK`, data definition, and every other statement |

A procedure runs in as4tainr, one statement at a time: each SQL statement in it is run like any other, as the restricted SQL login, with the routine's variables and parameters put into it as typed values, on the session of the statement that called it (so a procedure called under commitment control is part of the unit of work). A function is a PostgreSQL function that as4tainr writes from the parsed body, with every expression translated as statements are, and defined by the restricted SQL login (as views are). Routines live in the library named, else the default schema, else the current library. RUNSQLSTM keeps a routine's `BEGIN ... END` together although its statements end with semicolons.

## Commitment control

Written from the CL reference (STRCMTCTL, ENDCMTCTL, COMMIT, ROLLBACK), the ILE RPG Reference (COMMIT keyword, COMMIT and ROLBK) and the Db2 for i SQL Reference (COMMIT, ROLLBACK, the COMMIT precompiler option).

- `STRCMTCTL LCKLVL(*CHG|*CS|*ALL) CMTSCOPE(*ACTGRP|*JOB)` starts a commitment definition for the activation group (CL and the command line run in the default activation group) or for the job; a second one for the same scope is CPF8351. `ENDCMTCTL` ends it, rolling back what is pending (CPF8356 says how many changes). `COMMIT` and `ROLLBACK` (CL) without a definition are CPF8350.
- RPG files with the `COMMIT` keyword, and embedded SQL run with a commit level other than *NONE (CRTSQLRPGI `COMMIT`, *CHG by default, or `SET OPTION COMMIT`), make their changes in the definition of the program's activation group, else the job's. SQL starts one at the activation group when there is none. The changes are pending, and the rows changed stay locked, until COMMIT (CL, RPG `COMMIT`, SQL `COMMIT`) makes them permanent or ROLLBACK (`ROLBK`, SQL `ROLLBACK`) removes them. A statement or file operation that fails takes back only its own changes.
- When the job ends, pending changes are rolled back.

PostgreSQL never shows uncommitted data to other sessions, so other jobs (and the same job's files that are not under commitment control) see the rows as they were before the pending changes; see fidelity-gaps.md.

## Embedded SQL in RPG

RPG programs with `EXEC SQL` (SQLRPGLE, created with CRTSQLRPGI) run their statements on this layer, as the same restricted login, with the program's library list as the search path. Each program has its own connection for the call. Host variables are put into the statement as values (character values quoted and escaped, numbers exact); identifiers never come from host variables. Cursors read all their rows when they are opened. The SQLCA (SQLCODE, SQLSTATE, SQLERRMC, SQLERRD) is set after each statement. See docs/rpg-engine.md for the statements supported.

## Data definition

Written from the Db2 for i SQL Reference. A table is a physical file and an index a keyed logical file, in the same catalog as files created from DDS: RPG programs use them like any other file, and DSPPFM, WRKOBJ and the library list see them.

| Statement | Supported |
| --- | --- |
| `CREATE [OR REPLACE] TABLE` | Columns of type CHAR, VARCHAR, SMALLINT, INTEGER, BIGINT, DECIMAL, NUMERIC, REAL, FLOAT, DOUBLE, DATE, TIME, TIMESTAMP and TIMESTAMP(p) (p 0-12; at most 6 fractional digits are kept), DECFLOAT(16|34), BOOLEAN (shown TRUE/FALSE), XML, BINARY, VARBINARY and FOR BIT DATA (bytes, shown as hex digits; constants `BX'C1C2'` and `X'C1C2'`), GRAPHIC, VARGRAPHIC, NCHAR, NVARCHAR (UCS-2), CLOB, BLOB, DBCLOB (large objects: SQL uses them, an RPG program's record leaves them out); `NOT NULL` (other columns are null-capable); `DEFAULT value`, `WITH DEFAULT` (blanks, zero, or the current date, time or timestamp); `CCSID n` (accepted); `PRIMARY KEY` and `UNIQUE` (a duplicate is SQL0803, or CPF5026 for a WRITE); `CHECK` with the table's columns, constants, comparisons, IN, BETWEEN, LIKE, IS NULL and the functions LENGTH, TRIM, LTRIM, RTRIM, UPPER, LOWER, SUBSTR, ABS, MOD, COALESCE (SQL0545, or CPF502F for a WRITE); `CONSTRAINT name`; `RCDFMT name` (the record format; the system table name otherwise); `FOR SYSTEM NAME name` after the table name, `FOR COLUMN name` after a column name. OR REPLACE drops the old table and the indexes over it. |
| `CREATE [OR REPLACE] TABLE name [(columns)] AS (query) WITH [NO] DATA` | A table whose columns are the query's result columns (renamed by the column list; SQL0117 if the counts differ), with the query's rows for WITH DATA; `RCDFMT`, `NOT LOGGED INITIALLY` and the `INCLUDING`/`EXCLUDING` options are accepted |
| Identity columns | `GENERATED ALWAYS` or `BY DEFAULT AS IDENTITY` with `START WITH`, `INCREMENT BY`, `MINVALUE`, `MAXVALUE`, `CYCLE` (CACHE and ORDER accepted), on integer and decimal columns with no decimal places. A program's WRITE to a GENERATED ALWAYS column gets the next value; the value in the record is not used. |
| `CREATE [UNIQUE] INDEX name ON table (col [ASC\|DESC], ...) [RCDFMT name]` | A keyed logical file over the table, with all its fields |
| `CREATE [OR REPLACE] VIEW name [(columns)] AS query [WITH CHECK OPTION] [RCDFMT name]` | A logical file (not keyed) whose records are the query's rows; programs read it (in the order the query returns rows), SQL reads it and changes the table through it where PostgreSQL can. The query is translated like any query and defined by the restricted SQL login, so it reads only what that login may read. Column types come from the query (expressions without a length are VARCHAR(32740), decimals are packed). |
| `ALTER TABLE name ADD [COLUMN] definition` | The new column comes last; existing rows get its default |
| `ALTER TABLE name ADD [CONSTRAINT c] PRIMARY KEY\|UNIQUE (...)\|CHECK (...)`, `DROP PRIMARY KEY\|CONSTRAINT c\|UNIQUE c\|CHECK c` | Constraints, as in CREATE TABLE |
| `ALTER TABLE name DROP [COLUMN] c [CASCADE\|RESTRICT]` | CASCADE (the default) drops the views and logical files that use the column; RESTRICT refuses (SQL0478) while views, logical files, constraints or indexes use it. Other views over the table are made again. |
| `ALTER TABLE name ALTER [COLUMN] c SET DATA TYPE t\|SET [WITH] DEFAULT v\|DROP DEFAULT\|SET NOT NULL\|DROP NOT NULL` | Values are kept; the views over the table are made again after a type change |
| `DROP TABLE\|INDEX\|VIEW [IF EXISTS] name` | DROP TABLE drops the indexes over it too |
| `LABEL ON TABLE\|INDEX\|VIEW name IS 'text'` | The object's text |
| `LABEL ON COLUMN t.c TEXT IS 'text'`, `LABEL ON [COLUMN] t (c TEXT IS 'text', ...)` | The field's text. Column headings (`IS` without `TEXT`) are accepted and not kept. |
| `COMMENT ON ...` | Accepted, not kept |
| `SET SCHEMA name` | The default schema for unqualified names, for the rest of the STRSQL session or script |

New objects go to the library named, else the default schema, else the current library, else QGPL (system naming). Names can be SQL names of up to 128 characters. An SQL name that is not a valid system name (longer than 10 characters) gets a system name, as Db2 for i gives one: its first five characters and a five-digit number unique in the library (a table COUNTRY_LIST is COUNT00001) or in the table for a column (COUNTRY_CODE is COUNT00001, COUNTRY_NAME COUNT00002), unless FOR SYSTEM NAME or FOR COLUMN names it. CL commands (DSPPFM, DLTF, CRTLF PFILE) and RPG use the system names; SQL statements use the SQL names, and the table's system name too. An RPG program uses the SQL column names with the ALIAS keyword on the file or on an EXTNAME data structure. Tables get the owner and authorities CRTPF gives a file; DROP needs *ALL authority to the file (SQL0551).

The statements are parsed by as4tainr and the PostgreSQL objects built from the parsed description; no part of the statement text is passed on. Procedures and functions are described above. Not supported yet: quoted names with lower case or special characters, several ALTER TABLE actions in one statement, ADD COLUMN ... BEFORE, generated columns other than identity columns, FOREIGN KEY, the other data types (ROWID, DATALINK, arrays), external functions, table functions, triggers and sequences. See fidelity-gaps.md.

## Security

User SQL never runs as the database user as4tainr itself uses. `as4tainr init` creates a PostgreSQL login role, `as4tainr_sql`, that is not a superuser, has a random password (kept in the catalog) and may only use the library schemas: it cannot read the catalog (user profiles and password hashes), create or drop objects, switch to another role, or call server functions. Every statement opens a connection that logs in as that role. Statements that name the catalog or PostgreSQL's own schemas are also refused before they run, with a clearer message.

`as4tainr init` needs a database user who may create roles (the Docker and CI setups use one). Without one, as4tainr still runs, and SQL statements are refused with a message saying so.

## Not supported yet

The data definition listed above as not supported yet, the QSYS2 catalog views, `SELECT *` in `INSERT ... SELECT` (it includes the record number; list the columns), DB2-only scalar functions beyond the ones above, and special registers other than the dates and times. Object authority is not checked: anyone who can sign on can read and change every file with SQL, as with record-level access. See [fidelity-gaps.md](fidelity-gaps.md).
