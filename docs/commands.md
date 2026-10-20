# Commands you make (CRTCMD)

Written from IBM's public CL documentation ("Defining commands", the command definition statements CMD, PARM, ELEM, QUAL, DEP and PMTCTL, "Data passed to the command processing program", and the CRTCMD, DLTCMD and DSPCMD commands). The code is `core/src/cmd.rs`.

## Making a command

Command definition source is a member of type CMD, usually in QCMDSRC:

```
             CMD        PROMPT('Say hello')
             PARM       KWD(NAME) TYPE(*CHAR) LEN(10) MIN(1) CASE(*MIXED) +
                          PROMPT('Name')
             PARM       KWD(TIMES) TYPE(*DEC) LEN(3 0) DFT(1) RANGE(1 9)
             PARM       KWD(OBJ) TYPE(QO)
QO:          QUAL       TYPE(*NAME) LEN(10) DFT(X)
             QUAL       TYPE(*NAME) LEN(10) DFT(*LIBL) SPCVAL((*LIBL))
```

`CRTCMD CMD(MYLIB/HELLO) PGM(MYLIB/HELLOCPP) SRCFILE(MYLIB/QCMDSRC)` makes the command. `PGM` is the command processing program (CPP), a CL or RPG program; it need not exist yet. `VLDCKR` names a validity checking program, called first with the same parameters (an escape message from it stops the command). `DSPCMD` shows a command; `DLTCMD` deletes it.

## Running it

A command made with CRTCMD runs wherever a system command does: on a command line, in a CL program (with `&variables`), with QCMDEXC, and with SBMJOB. It is found through the library list, or by its library (`MYLIB/HELLO`); a system command of the same name wins unless the name is qualified. F4 prompts it from its definition, page by page.

What was typed is checked against the definition: required parameters (`MIN`), the number of values (`MAX`), restricted values (`RSTD` with `VALUES` and `SPCVAL`), `RANGE` and `REL`, lengths and types (names, numbers, logical values, dates, times, hexadecimal). An error is `CPF0006` with the reason. Then the CPP is called with one parameter per PARM, in PARM order:

| Parameter | What the CPP gets |
| --- | --- |
| `*CHAR`, `*NAME`, `*SNAME`, `*CNAME`, `*GENERIC`, `*PNAME`, `*CMDSTR` | The characters, padded with blanks to LEN; `VARY(*YES)`: a 2-byte binary length first |
| `*DEC`, `*INT2`, `*INT4`, `*UINT2`, `*UINT4` | The number (the CPP declares it as packed decimal or an integer) |
| `*LGL` | '1' or '0' |
| `*DATE`, `*TIME` | CYYMMDD, HHMMSS |
| `*HEX` | The bytes, right-adjusted |
| A qualified name (QUAL) | Its parts one after another, the object first: `QGPL/PRT01` is `PRT01     QGPL      ` |
| A list (MAX greater than 1) | A 2-byte binary count, then the values |
| A mixed list (ELEM) | A 2-byte count of the elements, then the elements |
| A list of mixed lists | The count, a 2-byte offset of each mixed list from the start of the parameter, then the mixed lists |

A special value (`SPCVAL`) is replaced by its "to" value; a value not given takes its default (`DFT`), or blanks, zero or '0'; `CONSTANT` values are passed and cannot be typed. Quoted text is folded to upper case unless `CASE(*MIXED)`.

The test `user_commands_crtcmd` makes the command above with a CL CPP and runs it from the job and from a CL program; `cmd::tests` checks the CPP forms byte by byte.

## Not yet

See [fidelity-gaps.md](fidelity-gaps.md): DEP, PMTCTL, RTNVAL, PASSATR, PASSVAL, CHGCMD, CHGCMDDFT, message-file prompt text, REXX.
