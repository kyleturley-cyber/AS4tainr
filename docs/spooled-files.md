# Spooled files

Printed output on IBM i does not go straight to a printer. It becomes a **spooled file** that waits on an output queue, where the user can display it, print it or delete it. as4tainr keeps spooled files the same way. They are written from IBM's public documentation of printer files and spooled files.

## Where spooled files come from

| Source | Spooled file | User data |
| --- | --- | --- |
| An RPG program printing through a program-described printer file (O specs, `EXCEPT`) | the printer file's name, e.g. `QSYSPRT`, or OVRPRTF `SPLFNAME` | the program name, or OVRPRTF `USRDTA` |
| A display command (`DSPPFM`, `DSPLIB`, `WRKOBJ`, ...) run in a batch job | `QSYSPRT` | the command name |
| `DSPJOBLOG OUTPUT(*PRINT)`, `SIGNOFF LOG(*LIST)` | `QPJOBLOG` | the job name |
| A batch job that ends abnormally | `QPJOBLOG` | the job name |

A batch job's job log is kept only when the job ends abnormally. That is what the shipped job description `QDFTJOBD` does with `LOG(4 0 *NOLIST)`.

Each spooled file belongs to the job that made it and to that job's user. It is numbered within the job (1, 2, ...), goes to the output queue of the user's profile (CRTUSRPRF and CHGUSRPRF `OUTQ`; `*WRKSTN`, the default, is `QGPL/QPRINT`, which is there from the start), and has status `RDY` once it is complete, `HLD` while held.

## Commands

| Command | What it does |
| --- | --- |
| `WRKSPLF` | Your spooled files: file, user, output queue, user data, status, pages. Options 3 hold, 4 delete, 5 display, 6 release and 8 attributes (WRKSPLFA), at IBM i's columns; F11 shows each file's job, F5 refreshes |
| `DSPSPLF FILE(QSYSPRT) JOB(number/user/name) SPLNBR(n)` | Shows the pages as printed in IBM i's viewer: the page and line of the first line shown, a Control field (T top, B bottom, +n/-n lines, Pn page n, Wn/W+n/W-n columns), a Find field (Enter or F16 looks for the next line with the text), 16 lines of 78 columns; F19/F20 move 78 columns |
| `DLTSPLF FILE(...) JOB(...) SPLNBR(...)` | Deletes a spooled file |
| `CPYSPLF FILE(...) TOFILE(...) TOMBR(...) MBROPT(*REPLACE\|*ADD) CTLCHAR(*NONE\|*FCFC)` | Copies a spooled file's lines into a file of one character field or a source member (docs/fidelity-gaps.md, CPYSPLF) |
| `HLDSPLF`, `RLSSPLF` (FILE, JOB, SPLNBR) | Holds a ready file (status HLD) and releases it (RDY again) |
| `CHGSPLFA FILE(...) OUTQ(lib/name) USRDTA(...)` | Moves a spooled file to another output queue, changes its user data (the other attributes are accepted and not kept) |
| `CRTOUTQ OUTQ(lib/name)`, `DLTOUTQ`, `CLROUTQ`, `HLDOUTQ`, `RLSOUTQ` | Output queues: created (SEQ, DSPDTA, OPRCTL, TEXT kept), deleted when empty, cleared of their ready and held files, held (CPF3426 when already held) and released; CPF3357 when a queue is not found |
| `WRKOUTQ OUTQ(*ALL)` or `WRKOUTQ lib/name` | The output queues with their number of files and status (options 3 hold, 4 delete, 5 work with, 6 release), or one queue's spooled files (options 3, 4, 5, 6) |

`JOB` defaults to `*` (this job) and `SPLNBR` to `*LAST`.

## Printing from RPG

`samples/qrpglesrc/CUSTPRT.rpgle` prints a customer balance report:

```
     FQSYSPRT   O    F  132        PRINTER OFLIND(*INOF)
     ...
     C                   EXCEPT    HEADING
     C                   READ      CUSTMAST
     C                   DOW       NOT %EOF
     C                   IF        *INOF
     C                   EXCEPT    HEADING
     C                   EVAL      *INOF = *OFF
     C                   ENDIF
     ...
     OQSYSPRT   E            HEADING        2  1
     O                       UDATE         Y      8
     O                                           50 'Customer Balance Report'
     O                                           78 'Page'
     O                       PAGE          Z     83
     ...
     O          E            DETAIL         1
     O                       CUSNO         Z      7
     O                       CUSNAM              41
     O                       CUSCTY              64
     O                       CUSBAL        J     83
     O               50                          84 '*'
```

`SBMJOB CMD(CALL CUSTPRT)`, then `WRKSPLF` and option 5:

```
10/16/26                   Customer Balance Report                        Page    1

 Number    Name                             City                           Balance
      1    Acme Tooling                     Des Moines                    1,250.50
      2    Midwest Fasteners                Chicago                         310.00
      3    Bluebird Supply                  Omaha                              .00
      4    Prairie Freight                  Lincoln                   9,999,999.99
      5    Lake, Pine & Co                  Duluth                           75.25-*

         Customers:     5                         Total balance:     10,001,485.24
```

`as4tainr call CUSTPRT`, which runs without a job, prints the report to standard output. The demo compares that with `samples/expected/CUSTPRT.txt`. The expected layout was worked out by hand from the O specs, not copied from a run.

What is supported:

- **Program-described printer files:** `F` spec with device `PRINTER` and `F` in position 22, record length 1-132+, and `OFLIND(*INOF)`, also `*INOV` or `*IN01`-`*IN99`.
- **Exception output:** `E` records with an EXCEPT name (or none, for `EXCEPT` without a name), and conditioning indicators with `AND`/`OR` lines.
- **Line movement:** space before and after, and skip before and after. With no space or skip, a record spaces one line after.
- **Fields:** end positions (absolute or `+n`), and constants. Edit codes `1`-`4`, `J`-`M`, `Z` and `Y` (dates). `B` blanks the field after printing.
- **Special fields:** `PAGE` (this file's page number), `UDATE` and `*DATE`.
- **Pages:** the printer file object's page length and overflow line (CRTPRTF PAGESIZE and OVRFLW; 66 and 60 when there is no object, FORMLEN and FORMOFL on the F spec first). Printing on or spacing past the overflow line turns on the overflow indicator, and skipping to a line above the current one starts a new page. In the cycle, the lines conditioned by the overflow indicator print once each time overflow occurs; fetch overflow (F in position 18) prints them before the line that asks.

## Printer files (CRTPRTF)

`CRTPRTF FILE(LIB/NAME) SRCFILE(LIB/QDDSSRC) PAGESIZE(66 132) OVRFLW(60)` creates a printer file from DDS (`core/src/prtf.rs`); without SRCFILE (`*NONE`) it is a program-described printer file that only gives its page size and overflow line. The DDS has record formats, fields (A, S; usage P is not printed), constants (`'text'`, `DATE`, `TIME`, `PAGNBR`), positions (or `+n` after the field before) and line numbers, `SPACEB`/`SPACEA`/`SKIPB`/`SKIPA` on records and fields, `EDTCDE`, `EDTWRD`, option indicators and `INDARA`.

An RPG program writes the record formats of an externally described printer file (`dcl-f rpt printer oflind(*in99);` or E in position 22): `WRITE HEAD`, `WRITE DETAIL`. The formats' fields are program fields (numbers zoned). The test `printer_files_crtprtf_and_rpg` prints a report over two pages of 12 lines.

## How it works

The RPG engine (docs/rpg-engine.md) lays out each output record from the program's fields as the O specs say, and writes the lines to the job's spooled file, kept by page and line (`as4tainr.spooled_files`, `as4tainr.spool_lines`). The spooled file is opened by the first line printed and closed by CLOSE or when the program ends; a module of a service program (or one bound into a program) keeps its printer file open between calls, until CLOSE or the end of its activation group. A call without a job prints to standard output instead: `as4tainr call` as on paper, `as4tainr system` as PASE `system` does on IBM i (its lines one after another without the blank lines spacing and skipping make, a line printed over another merged into it; observed on PUB400). `as4tainr batch "CMD"` runs a command in a batch job now, as SBMJOB and QBATCH would, so its spooled files stay.

## Spooled file APIs

QGYOLSPL (format OSPL0300, filter OSPF0100) lists spooled files and keeps the list open for QGYGTLE and QGYCLST; QSPOPNSP, QSPGETSP (format SPFR0300, into a user space) and QSPCLOSP read one (`core/src/spoolapi.rs`). The print data is SCS: a new line (x'15') for each line the printer moves down from line 1, a carriage return (x'0D') to print over a line, a form feed (x'0C') at each new page. RPGUnit's acceptance tests read its report this way and pass.

## Not yet

- **Formats and editing:** data formats `P`/`B`/`L`/`R` in O specs, the overflow indicators `OA`-`OG`; in DDS, fonts, CPI/LPI, BOX, LINE and barcodes are accepted and not printed.
- **Printer settings:** `PRTCTL`; of OVRPRTF only `SPLFNAME`, `USRDTA`, `PAGESIZE` and `OVRFLW` are used.
- **Output queues and writers:** writers (`STRPRTWTR`; a held output queue holds nothing back here, as there are no writers), remote output queues, the data queue of an output queue, `WRKSPLF`'s options 1 (send), 2 (change), 7 (messages) and 9 (printing status), and printing to a real printer or to PDF.
- **Unedited negative numbers** print with a trailing `-`. IBM i shows the sign in the last digit's zone.

A compiler listing (CRTBNDCL, CRTBNDRPG... in an interactive or batch job) is a spooled file named after the program, with a spooled file page for each listing page: a new page starts at each page heading line (as on IBM i 7.5, where a CL listing of three pages is a spooled file of three pages in WRKSPLF).
