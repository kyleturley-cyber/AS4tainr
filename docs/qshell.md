# Qshell from CL (QSH, STRQSH) and environment variables

`QSH CMD('...')` (and `STRQSH CMD(...)`) runs a shell command for a CL program or a command line, as IBM i's Qshell does from CL ("Running Qshell commands from CL" in IBM's Qshell documentation):

- The job's environment variables (ADDENVVAR, CHGENVVAR, RMVENVVAR; WRKENVVAR lists them) are the command's environment, and only they: as4tainr's own settings (the database URL) are not passed.
- `QIBM_QSH_CMD_OUTPUT` sends the output to `STDOUT` (the default: what `as4tainr system` prints), `NONE`, `FILE=path` (a new stream file) or `FILEAPPEND=path`.
- The command ends with QSH0005 and its exit status (the first 4 bytes of the message data, a binary number), or QSH0006 and the signal number when a signal ended it: completion messages, or escape messages with `QIBM_QSH_CMD_ESCAPE_MSG=Y` (QSH0005 only for an exit status above 0).
- The command starts in the user's home directory (`/home/USER`) when there is one, else in `/`.

## Turning it on

QSH runs commands with `/bin/sh` as the operating system user as4tainr runs as. That user is not limited by as4tainr's object authorities (and can reach the database), so QSH is off until the system is started with `AS4TAINR_QSH=1`; without it QSH ends with CPF9899. Give that setting only where everyone who can sign on may have a shell.

## The IFS

The command works on the integrated file system at its paths (`/home/...`, `/tmp/...`): before it runs, as4tainr copies the IFS to a directory on disk (only what changed since the last command), binds its top-level directories at their paths in a mount namespace that only the command sees, and afterwards writes back the files the command changed or made and removes what it removed. A file that was there keeps its CCSID; a new file is tagged CCSID 1208 (UTF-8). The container's own files are not changed (an empty directory is made for a top-level IFS directory the container does not have), and the container's system directories (`/usr`, `/bin`, `/etc`, `/var`...) stay the container's, so the command uses the container's programs.

Mount namespaces need a privilege containers do not have by default. With Docker, add `cap_add: [SYS_ADMIN]` to the as4tainr service (and `security_opt: [apparmor:unconfined]` on hosts whose AppArmor profile forbids mounts). Without it QSH still runs, on the container's own files, and as4tainr says so once on standard error. `AS4TAINR_QSH_IFS=0` turns the IFS copy off; `AS4TAINR_QSH_MIRROR` names the directory for it (default: one per database in the temporary directory).

One QSH command at a time uses the copy of the IFS. The command can read every stream file, whatever the user's authority to it. See docs/fidelity-gaps.md for the rest.
