# The integrated file system

as4tainr has the "root" (`/`) file system of IBM i's integrated file system (IFS): directories and stream files. Each object is a row of `as4tainr.ifs` (`core/src/ifs.rs`) with its path, its bytes, the CCSID it is tagged with, its permission bits and its owner. Names keep their case and are found ignoring case, as in the root file system. As on IBM i, `/home` and `/tmp` are there from the start (`/tmp` with mode 1777; the restricted rename and unlink bit is kept but not enforced). Include files that `as4tainr import` brings in are stream files under `/<library>/` (UTF-8, CCSID 1208), and RPG `/INCLUDE` and CL `INCLUDE SRCSTMF` read them from here.

## From programs

RPG programs call the UNIX-type APIs through prototypes, bound from QC2LE (`ctl-opt bnddir('QC2LE')`), as on IBM i (`core/src/rpg/crt.rs`):

| Function | What it does |
| --- | --- |
| `open(path: oflag [: mode [: ccsid [: open ccsid]]])` | A file descriptor, or -1 with errno. O_RDONLY, O_WRONLY, O_RDWR; O_CREAT (with the mode bits) and O_EXCL; O_TRUNC; O_APPEND; O_CCSID or O_CODEPAGE (the CCSID a new file is tagged with; without it, the job's, 37); O_TEXTDATA (data converted between the file's CCSID and the open CCSID: the 4th argument for an existing file, else the job's); O_TEXT_CREAT (a new file tagged with the 4th argument, data converted from the 5th, 0 meaning the job's). The flag values are IBM's (QSYSINC/H FCNTL) |
| `read`, `write`, `lseek`, `close` | On the descriptor. Text conversion reads whole characters only |
| `unlink`, `mkdir`, `rmdir`, `access`, `rename` | unlink of a directory is EPERM; rmdir of a directory that is not empty is ENOTEMPTY; rename keeps an existing target (EEXIST) |
| `__errno`, `strerror`, `perror` | errno is an `int(10)` the returned pointer addresses; the values are IBM's (ENOENT 3025, EEXIST 3457, EACCES 3401...); the texts are as4tainr's |
| `system(command)` | Runs a CL command: 0 when it ran, 1 when it failed, -1 for a null pointer |
| `chdir(path)`, `getcwd(buf: size)` | The job's current directory (below); getcwd gives NULL with ERANGE when the path and its null do not fit |
| `printf(format: ...)`, `puts(string)` | The job's standard output, which `as4tainr system` writes out (see docs/fidelity-gaps.md) |

File descriptors belong to the job: a descriptor one program opened can be used by another program of the job until it is closed. A file's bytes are read when it is opened and stored when it is closed, when the program that wrote ends, and when the job ends.

## The current directory

Each job has a current directory, where relative path names start (`SRCSTMF('qrpglesrc/x.rpgle')`, `open('data.csv')`). A job starts in its user's home directory, /home/USER, when it exists, else in /; `as4tainr system` and `as4tainr call` start in the directory they were started in when that path is a directory in the IFS, as PASE `system` runs in its shell's directory. `CHGCURDIR` (`CD`) and `chdir()` change it, for the rest of the job; Qshell commands start in it.

## CL commands

| Command | |
| --- | --- |
| `MKDIR` (`MD`, `CRTDIR`) `DIR(path)` | CPFA0A0 if it exists, CPFA0A9 if the directory above it does not |
| `RMVDIR` (`RMDIR`, `RD`) `DIR(path) [SUBTREE(*ALL)]` | An empty directory, or with SUBTREE(*ALL) everything in it |
| `RMVLNK` (`DEL`, `ERASE`) `OBJLNK(path)` | A stream file, or the files a generic name (`/dir/*.txt` is not supported; `/dir/abc*` is) matches; CPFA0A9 when nothing is found |
| `RNM OBJ(path) NEWOBJ(name)` | |
| `CHGCURDIR` (`CD`) `DIR(path)` | The job's current directory; CPFA0A9 when it does not exist |
| `WRKLNK`, `DSPLNK` `OBJ(path)` | The objects in a directory, with options 4 (remove) and 5 (display) |
| `CPYTOSTMF FROMMBR('/QSYS.LIB/lib.LIB/file.FILE/mbr.MBR') TOSTMF(path)` | A source member's records as lines: trailing blanks removed and ENDLINFMT's line end added (*CRLF by default; *FIXED keeps the records whole); STMFOPT(*NONE, *ADD, *REPLACE); STMFCCSID (a number, *STMF: the existing file's CCSID or 37 for a new file, *PCASCII 1252, *STDASCII 819). CPFA097 when the file exists with STMFOPT(*NONE) |
| `CPYFRMSTMF FROMSTMF(path) TOMBR('/QSYS.LIB/...')` | Lines into a source member: any combination of CR and LF ends a line, tabs become blanks to the next stop of 8 (TABEXPN(*YES)); MBROPT(*NONE, *ADD, *REPLACE); CPFA095 when it cannot copy |
| `CPYTOSTMF FROMMBR('/QSYS.LIB/lib.LIB/savf.FILE') TOSTMF(path)`, `CPYFRMSTMF FROMSTMF(path) TOMBR('/QSYS.LIB/lib.LIB/savf.FILE')` | A save file's contents as they are (CCSID 65535), to move a save to another system (docs/save-restore.md). CPYFRMSTMF needs the save file to exist, and MBROPT(*REPLACE) when it has data |

## From outside

`as4tainr put LOCAL PATH [--ccsid N]` copies a file, or a folder and everything in it, from the machine as4tainr runs on into the IFS (as copying files to an IBM i with FTP or a mapped drive would), tagged with CCSID 1208 unless `--ccsid` says otherwise. `tools/legacy.sh` uses it to install applications the way their READMEs say. `as4tainr get PATH LOCAL` copies a stream file out, its bytes as they are.

## From SQL

`QSYS2.IFS_READ`, `IFS_READ_UTF8` and `IFS_READ_BINARY` read a stream file as lines, and `QSYS2.IFS_WRITE`, `IFS_WRITE_UTF8` and `IFS_WRITE_BINARY` write lines (docs/sql.md, IBM i services).

## CCSIDs

Text is converted between CCSID 37 (EBCDIC), 1208 (UTF-8), 819 and 367 (ISO 8859-1 and ASCII), 1252 (taken as ISO 8859-1), and 1200 and 13488 (UTF-16). An open with O_TEXTDATA between other CCSIDs fails with ECONVERT.

See [fidelity-gaps.md](fidelity-gaps.md) for what is not there yet.
