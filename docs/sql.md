# SQL

as4tainer runs SQL in the DB2 for i dialect against the same files RPG programs use: a physical or logical file created from DDS is a table (or view) to SQL, and a change made one way is seen the other way at once.

| Where | How |
| --- | --- |
| 5250 session | `STRSQL`: type a statement, press Enter. A SELECT shows its rows on a Display Data screen (Page Up/Down, F19/F20 to shift wide results); other statements report how many rows they changed. F9 retrieves the previous statement, F3 ends. |
| Command line | `RUNSQL SQL('UPDATE CUSTMAST SET CUSBAL = 0 WHERE CUSNO = 3')`: one statement that does not return rows (as on IBM i, RUNSQL does not run SELECT). |
| Shell | `as4tainer sql "SELECT * FROM QGPL/CUSTMAST"`, with the library list from `AS4TAINER_LIBL`. In Docker: `docker compose exec as4tainer as4tainer sql "..."`. |

## The dialect

Written from IBM's public DB2 for i SQL reference.

| DB2 for i | Supported |
| --- | --- |
| Statements | SELECT (with joins, GROUP BY, ORDER BY, `FETCH FIRST n ROWS ONLY`, ...), WITH, VALUES, INSERT, UPDATE, DELETE. One statement at a time. |
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

Each statement is committed on its own (`COMMIT(*NONE)`).

## Security

User SQL never runs as the database user as4tainer itself uses. `as4tainer init` creates a PostgreSQL login role, `as4tainer_sql`, that is not a superuser, has a random password (kept in the catalog) and may only use the library schemas: it cannot read the catalog (user profiles and password hashes), create or drop objects, switch to another role, or call server functions. Every statement opens a connection that logs in as that role. Statements that name the catalog or PostgreSQL's own schemas are also refused before they run, with a clearer message.

`as4tainer init` needs a database user who may create roles (the Docker and CI setups use one). Without one, as4tainer still runs, and SQL statements are refused with a message saying so.

## Not supported yet

CREATE/ALTER/DROP and other DDL (create files with CRTPF/CRTLF), commitment control (COMMIT, ROLLBACK, `COMMIT(*CHG)`), embedded SQL in RPG (SQLRPGLE), the QSYS2 catalog views, `SELECT *` in `INSERT ... SELECT` (it includes the record number; list the columns), DB2-only scalar functions beyond the ones above, and special registers other than the dates and times. Object authority is not checked: anyone who can sign on can read and change every file with SQL, as with record-level access. See [fidelity-gaps.md](fidelity-gaps.md).
