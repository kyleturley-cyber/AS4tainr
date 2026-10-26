# REXX procedures

as4tainr runs REXX procedures as IBM i's REXX/400 does (`core/src/rexx/`): from a source member with `STRREXPRC`, or as the command processing program of a command made with `CRTCMD ... PGM(*REXX) REXSRCFILE(...) REXSRCMBR(...)`.

```
STRREXPRC SRCMBR(MYPROC) SRCFILE(MYLIB/QREXSRC) PARM('one two')
```

## The language

Written from IBM's REXX/400 Reference and Programmer's Guide and the REXX language definition:

- Clauses end at a semicolon or the end of a line; a comma at the end of a line continues the clause; comments `/* */` nest; labels are a symbol and a colon.
- Instructions: assignment (simple and compound variables, stems: `s.i = x`, `s. = 0`), `SAY`, `IF ... THEN ... ELSE`, `DO` (simple groups, `DO n`, `DO i = a TO b BY c FOR n`, `FOREVER`, `WHILE`, `UNTIL`), `LEAVE`, `ITERATE`, `SELECT/WHEN/OTHERWISE`, `CALL` and function calls, internal routines with `PROCEDURE [EXPOSE ...]`, `RETURN`, `EXIT`, `PARSE [UPPER] ARG|PULL|VAR|VALUE ... WITH|SOURCE|VERSION`, `ARG`, `PULL`, `SIGNAL label`, `SIGNAL VALUE`, `SIGNAL ON|OFF` and `CALL ON|OFF` for ERROR, FAILURE, NOVALUE, SYNTAX, HALT, `INTERPRET`, `DROP`, `NUMERIC DIGITS`, `ADDRESS`, `PUSH`, `QUEUE`, `NOP`; `TRACE` and `OPTIONS` are accepted and do nothing.
- Parsing templates: words (each target but the last gets a word; the last gets the rest less one separating blank), literal and variable patterns, absolute and relative positions, `.` placeholders, several templates separated by commas.
- Arithmetic is decimal, to `NUMERIC DIGITS` significant digits (9 by default, at most 28 here); division drops trailing zeros (`1/3` is `0.333333333`, `6/2` is `3`); `%` integer division, `//` remainder, `**` whole powers.
- Comparisons: `=`, `<>`, `>`... compare numbers as numbers and other strings without leading and trailing blanks, the shorter padded with blanks; `==`, `>>`... compare exactly. Character order is EBCDIC (CCSID 37).
- Built-in functions: ABBREV, ABS, ADDRESS, ARG, B2X, BITAND, BITOR, BITXOR, C2D, C2X, CENTER/CENTRE, COMPARE, CONDITION, COPIES, D2C, D2X, DATATYPE, DATE, DELSTR, DELWORD, DIGITS, ERRORTEXT, FORM, FORMAT, FUZZ, INDEX, INSERT, LASTPOS, LEFT, LENGTH, MAX, MIN, OVERLAY, POS, QUEUED, RANDOM, REVERSE, RIGHT, SIGN, SOURCELINE, SPACE, STRIP, SUBSTR, SUBWORD, SYMBOL, TIME, TRANSLATE, TRUNC, USERID, VALUE, VERIFY, WORD, WORDINDEX, WORDLENGTH, WORDPOS, WORDS, X2B, X2C, X2D. Byte functions (C2X, X2C, C2D, D2C, the BIT functions) and hexadecimal strings (`'C1'x`) work on the CCSID 37 bytes of the text.

## Commands

A clause that is only an expression is a command for the command environment, `*COMMAND` by default: a CL command. In it, `&name` is a REXX variable: its value goes into the command, and a retrieve command's return value (`RTVJOBA USER(&user)`, `RTVOBJD ... SRCDATE(&d)`, `CVTDAT ... TOVAR(&v)`, a `CALL` parameter) comes back into the variable. The CL variable is as long as the REXX variable's value (so a procedure sets it to blanks of the length it wants first, as iRPGUnit does); a variable with no value gets the returned value without its trailing blanks.

After a command, RC is 0, or the identifier of the escape message it ended with (`CPF9801`); a command that fails raises the ERROR condition (SIGNAL ON ERROR jumps to the label `ERROR`, or the one NAME gives). `SNDPGMMSG MSGTYPE(*ESCAPE)` to the caller ends the procedure, and STRREXPRC with it.

`SAY` writes to the job's standard output (`as4tainr system` shows it). `PULL` reads the external data queue (`PUSH`, `QUEUE`); there is no terminal input, so an empty queue gives an empty line. `EXIT n` with n not 0 ends STRREXPRC with CPF7CFF.

## Commands with a REXX command processing program

`CRTCMD CMD(LIB/CMD) PGM(*REXX) REXSRCFILE(LIB/QREXSRC) REXSRCMBR(MBR)`: running the command checks its parameters against the command definition, then the procedure gets one argument: every parameter in PARM order, in keyword form, `KWD(value)`, with the value given or its default and a qualified name as `LIB/NAME` (`MODULE(MYLIB/X) SRCFILE(*LIBL/QSRC) SRCMBR(*MODULE) ...`, which `PARSE ARG 'MODULE('lib'/'obj') ...'` takes apart).

## Not here

ADDRESS environments other than *COMMAND (`*EXECSQL`, programs), exit programs (EXITPGM, REXEXITPGM), external routines (another procedure called as a function), interactive input for PULL, TRACE, the elapsed-time clock of TIME('E')/TIME('R'), exponential notation of very large or small numbers, NUMERIC FUZZ and FORM. REXX's own errors are messages REX0nnn with the REXX error number (to be checked against a real system), and SYNTAX traps them.
