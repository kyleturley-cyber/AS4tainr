# SSH, Code for IBM i and Mapepire

as4tainr can serve SSH, so that the tools that reach IBM i over SSH reach
it too: **Code for IBM i** (the VS Code extension), `ssh`, `scp` and `sftp`.
Code for IBM i runs its SQL through **Mapepire**, which it starts over the
same SSH connection; as4tainr answers that with its own Mapepire server.

## Turning it on

SSH is off unless an address is given:

```yaml
# docker-compose.yml, service as4tainr
environment:
  AS4TAINR_SSH_LISTEN: 0.0.0.0:22
  AS4TAINR_QSH: "1"          # shell commands run in the container
cap_add: [SYS_ADMIN]         # so commands see the IFS at its paths
security_opt: ["apparmor:unconfined"]
ports:
  - "2222:22"
```

or `as4tainr tn5250 --ssh-listen 0.0.0.0:22`. The host key is made the
first time and kept in the database (`as4tainr.ssh_host_keys`), so it stays
the same across restarts of the container.

**Security.** An SSH command runs a shell in the container, as QSH does
(docs/qshell.md): it is not limited by as4tainr's object authorities, and
it can read the container's environment. Give SSH access only to users you
would give the container to.

## Signing on

Use a user profile and its password, as on the 5250 sign-on. A profile
whose password has expired cannot sign on over SSH: change the password on
a 5250 session first. Only passwords are taken (no keys yet).

## What a session gets

- `HOME` is the profile's home directory in the IFS (`/home/USER`; make it
  with `MKDIR DIR('/home/USER')` and `CHGOWN`), the shell is bash
  (`/QOpenSys/pkgs/bin/bash`), and the IFS is at its paths (`/home`,
  `/tmp`, ...).
- PASE's programs at their paths: `/QOpenSys/usr/bin/system` (runs a CL
  command as the user), `qsh` (with Qshell's `liblist`), `attr` and
  `setccsid` (an IFS file's CCSID), `uname` (answers as IBM i 7.5's PASE:
  `uname -rv` is `5 7`), and the usual tools in `/QOpenSys/usr/bin` and
  `/QOpenSys/pkgs/bin`.
- `/QSYS.LIB`: libraries, and the objects and members of the libraries
  a command names (docs/qshell.md, "The IFS"); source members read and
  written as text, so Code for IBM i's member search (`grep` in a source
  file's directory) and object lookup (`ls /QSYS.LIB/LIB.LIB/OBJ.*`) work.
- `sftp` (and `scp -s`) reads and writes the IFS, with the owner's and
  the public's permission bits checked (*ALLOBJ passes).
- Mapepire: the Java command at
  `/QOpenSys/QIBM/ProdData/JavaVM/jdk80/64bit/bin/java` (and jdk11, 17,
  21) starts as4tainr's Mapepire server whatever JAR it is given, and a
  `java ... -jar mapepire-server-X.Y.Z.jar --single` sent over SSH is
  answered by it directly. `as4tainr mapepire` serves it on standard input
  and output too.

`ssh host` without a command (and Code for IBM i's terminal) gets an
interactive session: each line typed runs as a command, as above, in the
directory the one before ended in (`cd` carries over); `exit` or Ctrl-D
ends it. With a terminal the session echoes, edits (backspace, Ctrl-C)
and shows a prompt. A command does not read the terminal (its standard
input is empty: no editors or `read`), its output comes when it ends,
and variables set by one line are gone for the next.

## Code for IBM i

Create a connection with the host, the SSH port you mapped, a user
profile and its password. Leave "Mapepire: use server" off (the default):
the extension uploads its Mapepire JAR to `~/.vscode` and starts it over
SSH, and as4tainr answers. The SQL job is a database server job
(`nnnnnn-QUSER-QP0ZSPWT`, as Mapepire 2.3 names it), with the connection's library list.

What works: browsing libraries, objects and source members; opening,
editing and saving members (CPYTOSTMF and CPYFRMSTMF through the IFS);
the IFS browser; running actions in the ILE environment (their messages
from the job log, their printed output from the job's spooled files) and
in Qshell and PASE; the SQL statements Code for IBM i and the Db2 for i
extension run.

as4tainr is tested against Code for IBM i's own API test suite
(`src/api/tests` in the extension's repository, run against IBM i by its
authors): see "Testing with Code for IBM i" below.

## IBM ACS

ACS's 5250 emulator connects to as4tainr's TN5250 server (port 23 in the
container). ACS's other tools (Run SQL Scripts, the IFS and spooled file
views) need the IBM i host servers, which as4tainr does not have yet.

## Testing with Code for IBM i

Clone vscode-ibmi at test time (never vendored), install only what its API
tests need (`vitest`, `@ibm/mapepire-js`, `node-ssh`, `tmp`,
`escape-string-regexp`, `@ibm/ibmi-eventf-parser`, with `--ignore-scripts`),
and run them against as4tainr:

```sh
VITE_SERVER=127.0.0.1 VITE_DB_PORT=2222 VITE_DB_USER=ME VITE_DB_PASS=... \
VITE_TEMP_LIB=ILEDITOR npx vitest run --config vitest.config.ts connection
```

`AS4TAINR_MAPEPIRE_TRACE=/path/file` makes the Mapepire server add each
request and reply to that file. Tests that need objects IBM ships
(QSYSINC's include members, QIWS.QCUSTCDT's sample rows) cannot pass:
as4tainr has no IBM code or data.
