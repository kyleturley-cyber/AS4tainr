# IBM i host servers

IBM i Access Client Solutions (ACS) and the JDBC driver of the IBM
Toolbox for Java (JTOpen, `jt400.jar`) connect to an IBM i through its
host servers. as4tainr serves them when it is started with
`--hostserver-listen IP` (or `AS4TAINR_HOSTSERVER_LISTEN`), on their
well-known ports:

| Server | Port | Done |
| --- | --- | --- |
| Port mapper (as-svrmap) | 449 | Answers the ports of as-central 8470, as-database 8471, as-dtaq 8472, as-file 8473, as-netprt 8474, as-rmtcmd 8475, as-signon 8476 |
| Sign-on (as-signon) | 8476 | Exchange of attributes (V7R5M0, server level 10, password level 2), sign-on with a password |
| Database (as-database) | 8471 | Exchange of seeds, start server with a password, server attributes, SQL statements, parameter markers, CALL, commitment control, the SYSIBM metadata procedures (below) |
| Remote command (as-rmtcmd) | 8475 | Exchange of seeds, start server, exchange attributes (data stream level 10), CL commands run with their messages (Toolbox `CommandCall`), programs called with their parameters (`ProgramCall`: RPG and CL programs, the data queue, user space and retrieve APIs done here) |

The data streams are the ones JTOpen's client side builds and reads; the
server side is as4tainr's own (`core/src/hostsrv.rs`, `core/src/hostdb.rs`).

## SQL

The database server runs a statement on the connection's job (the
user, current library and library list of the profile; the client's
default library as the default schema) with the same statement engine
as the Mapepire server (`core/src/mapepire.rs`). Requests done: create,
change, reset and delete an RPB, prepare, prepare and describe,
execute (a batch: a run for each row of values), execute immediate,
open and describe, open, describe and fetch, fetch, close, change
descriptor, and the last error's message ID, text and SQLCA. Requests
compressed by the client (run-length) are expanded; replies are not
compressed.

Formats are the extended ones (data format CP 0x380C, parameter marker
format 0x380D, result data 0x380E), which the JDBC driver uses with a
server of V5R4 or later. A query without parameter markers runs when
it is prepared; its rows are kept for the open and sent 2048 rows (at
most about 1 MB) a block. Columns are described as:

| Column | SQL type | Bytes |
| --- | --- | --- |
| SMALLINT, INTEGER, BIGINT | 500, 496, 492 | 2, 4, 8 |
| REAL, DOUBLE | 480 | 8 |
| DECIMAL(p,s) | 484 (packed) | p/2+1 |
| A decimal expression | 484, DECIMAL(31, s), s the most decimals of its values | 16 |
| NUMERIC(p,s) | 488 (zoned) | p |
| CHAR(n) | 452, CCSID 1208 | n |
| DATE, TIME, TIMESTAMP | 384, 388, 392, CCSID 1208 | in the connection's date and time formats: 6, 8 or 10; 8; 26 |
| BINARY, VARBINARY | 912, 908 | n, n+2 |
| Others (VARCHAR...) | 448, CCSID 1208 | the declared length + 2, else the longest value's |

All columns are described as nullable (type + 1).

### Parameter markers

A marker is described as the column it stands for: the column of an
INSERT's VALUES (listed, or the table's in that position), the column
it is compared with (`col = ?`, `? < col`, `LIKE`, `BETWEEN`, `IN`,
`SET col = ?`), its type from a query of the statement's table or FROM
clause that returns no rows. Any other marker is VARCHAR(32739). A
query with markers is described when it is prepared by running it
with nulls for its markers: a varying-length column of no declared
length is described as VARCHAR(32739), a decimal expression as
DECIMAL(31, 10). Values come as the client lays them out (dates and
times in ISO form) and are put into the statement as literals.

### CALL

A CALL's markers are described as the procedure's parameters (their
types; parameter type 0xF0 input, 0xF1 output, 0xF2 input and output),
so that `CallableStatement.registerOutParameter` and `getXxx` work: the
execute reply carries the OUT and INOUT values in a row of result data
(IN parameters null). The number of result sets is in SQLERRD(2); the
client opens the first with open and describe and each next one after
closing the one before (`getMoreResults`). The columns of an SQL
procedure's result sets have their queries' types.

### Commitment control

With autocommit off (`Connection.setAutoCommit(false)`: CP 0x3824
0xD5 in set attributes) and a commit level (CP 0x380E: 1 *CHG, 2 *CS,
3 *ALL, 4 *RR from the transaction isolation), the connection's
statements run in a unit of work of their own (a commitment
definition of the connection, as RUNSQLSTM's with a commit level) that
commit (function 0x1807), rollback (0x1808) and the COMMIT and
ROLLBACK statements end. Turning autocommit back on commits what is
pending; a connection that ends rolls it back. SQL DDL statements run
committed, outside the unit of work.

### Database metadata

At V7R5 the JDBC driver answers `DatabaseMetaData` by calling the
catalog procedures of SYSIBM (`CALL SYSIBM.SQLTABLES(?, ?, ?, ?, ?)`
and so on). as4tainr answers them (`core/src/sqlmeta.rs`, from any SQL
client: the host servers, Mapepire, `db2`) with a query of the QSYS2
catalog views that returns the result set the JDBC API defines:

| Procedure | DatabaseMetaData | From |
| --- | --- | --- |
| SQLTABLES | getTables, getSchemas, getCatalogs, getTableTypes | SYSTABLES, SYSSCHEMAS |
| SQLCOLUMNS | getColumns | SYSCOLUMNS |
| SQLPRIMARYKEYS | getPrimaryKeys | SYSCST, SYSKEYCST |
| SQLFOREIGNKEYS | getImportedKeys, getExportedKeys, getCrossReference | SYSREFCST, SYSKEYCST |
| SQLSTATISTICS | getIndexInfo | SYSINDEXES, SYSKEYS, primary key and unique constraints |
| SQLSPECIALCOLUMNS | getBestRowIdentifier (the primary key), getVersionColumns (none) | SYSKEYCST, SYSCOLUMNS |
| SQLPROCEDURES, SQLPROCEDURECOLS, SQLFUNCTIONS | getProcedures, getProcedureColumns, getFunctions | SYSROUTINES, SYSPARMS |
| SQLGETTYPEINFO | getTypeInfo | the types as4tainr's SQL has |
| SQLTABLEPRIVILEGES, SQLCOLPRIVILEGES, SQLUDTS, SQLFUNCTIONCOLS, SQLPSEUDOCOLUMNS | | no rows |

A CALL's result set is reported in the SQLCA (SQLERRD(2), the number
of result sets) and the client opens it with open and describe.

## Remote commands

A connection's commands run in a job of its own (the user, its current
library and the user library list), one after another, as IBM i's
QZRCSRVS server job runs a client's. The reply's return code is 0 when
the command ran and 0x0400 when it failed; the messages are the ones
the command sent (completions, diagnostics, the escape message last),
each with its ID, type, message file (QCPFMSG for CPx, QSQLMSG for
SQL) and as4tainr's text. Severities are known only for the messages a
capture showed (others are 0).

A program call passes its parameters by reference: an RPG program gets
their bytes; a CL program its variables from them (*CHAR and *LGL in
EBCDIC, *DEC packed, *INT binary) and back; the APIs done in as4tainr
(`sysapi.rs`) the storage, the packed decimal parameters of the data
queue APIs typed as for PASE's `_PGMCALL`. The output and input-output
parameters come back. A program or library that is not there is
answered as JTOpen expects it (MCH3401 with the object's type and name
in the substitution data: `ObjectDoesNotExistException`). The server
says its job is number/QUSER/QZRCSRVS, so `ProgramCall.getServerJob`
needs no API call.

## Passwords

The servers announce password level 2: the client sends a SHA-1 password
substitute made from a token of the user ID and the password. as4tainr
keeps that token for a profile when its password is set (CRTUSRPRF,
CHGPWD, `as4tainr rstpwd`) and when it signs on with it (5250, SSH):
a profile made before this version signs on to the host servers after it
has signed on once another way or its password has been changed. At
QPWDLVL 0 and 1, where passwords are not case-sensitive, the tokens of
the password in upper and in lower case are kept too.

## Trying it

```sh
as4tainr tn5250 --listen 0.0.0.0:23 --hostserver-listen 0.0.0.0
```

With the Toolbox's jar, `jdbc:as400://HOST;prompt=false` and a user
profile connect. The port mapper needs port 449 (root in the
container); without it, set the client's ports to the defaults
(`AS400.setServicePortsToDefault()`).
