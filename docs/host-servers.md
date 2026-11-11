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
| Database (as-database) | 8471 | Exchange of seeds, start server with a password, server attributes (the JDBC driver connects, `Connection.isValid` answers true), SQL statements without parameter markers: queries with their columns and rows, updates with their counts, errors with their SQLCODE and message |

The data streams are the ones JTOpen's client side builds and reads; the
server side is as4tainr's own (`core/src/hostsrv.rs`, `core/src/hostdb.rs`).

## SQL

The database server runs a statement on the connection's job (the
user, current library and library list of the profile; the client's
default library as the default schema) with the same statement engine
as the Mapepire server (`core/src/mapepire.rs`). Requests done: create,
change, reset and delete an RPB, prepare, prepare and describe,
execute, execute immediate, open and describe, open, describe and
fetch, fetch, close, and the last error's message ID, text and SQLCA.

A query runs when it is prepared; its rows are kept for the open and
sent 2048 rows a block. Columns are described as:

| Column | SQL type | Bytes |
| --- | --- | --- |
| SMALLINT, INTEGER, BIGINT | 500, 496, 492 | 2, 4, 8 |
| REAL, DOUBLE | 480 | 8 |
| DECIMAL(p,s) | 484 (packed) | p/2+1 |
| NUMERIC(p,s) | 488 (zoned) | p |
| CHAR(n) | 452, CCSID 1208 | n |
| BINARY, VARBINARY | 912, 908 | n, n+2 |
| Others (VARCHAR, dates, times, timestamps) | 448, CCSID 1208 | the declared or longest length + 2 |

All columns are described as nullable (type + 1). Dates, times and
timestamps come as text in Db2's form (`2026-11-10`, `12.34.56`,
`2026-11-10-12.34.56.123456`).

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
