# Spooled files

Printed output on IBM i does not go straight to a printer. It becomes a **spooled file** that waits on an output queue, where the user can display it, print it or delete it. as4tainr keeps spooled files the same way. They are written from IBM's public documentation of printer files and spooled files.

## Where spooled files come from

| Source | Spooled file | User data |
| --- | --- | --- |
| An RPG program printing through a program-described printer file (O specs, `EXCEPT`) | the printer file's name, e.g. `QSYSPRT` | the program name |
| A display command (`DSPPFM`, `DSPLIB`, `WRKOBJ`, ...) run in a batch job | `QSYSPRT` | the command name |
| `DSPJOBLOG OUTPUT(*PRINT)`, `SIGNOFF LOG(*LIST)` | `QPJOBLOG` | the job name |
| A batch job that ends abnormally | `QPJOBLOG` | the job name |

A batch job's job log is kept only when the job ends abnormally. That is what the shipped job description `QDFTJOBD` does with `LOG(4 0 *NOLIST)`.

Each spooled file belongs to the job that made it and to that job's user. It is numbered within the job (1, 2, ...), goes to output queue `QPRINT`, and has status `RDY` once it is complete.

## Commands

| Command | What it does |
| --- | --- |
| `WRKSPLF` | Your spooled files: file, user data, status, pages, job, number. Option 4 deletes, option 5 displays, and F5 refreshes |
| `DSPSPLF FILE(QSYSPRT) JOB(number/user/name) SPLNBR(n)` | Shows the pages as printed. 132-column lines scroll with F19/F20 |
| `DLTSPLF FILE(...) JOB(...) SPLNBR(...)` | Deletes a spooled file |

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
- **Pages:** 66 lines. Printing on or spacing past line 60 turns on the overflow indicator, and skipping to a line above the current one starts a new page.

## How it works

Jariko parses O specs but does nothing with them, and `EXCEPT` is not implemented. When a program is loaded, the runner (`Printers.java`) reads its PRINTER F specs and their O specs, and replaces each `EXCEPT` with a signal (the same mechanism as record locks, see [record-locks.md](record-locks.md)). On the signal, it lays out the lines from the program's variables and sends them to as4tainr core (bridge operations `splopen`, `splwrite`, `splclose`, protocol version 5). Core keeps them by page and line (`as4tainr.spooled_files`, `as4tainr.spool_lines`). The spooled file is closed when the program ends.

## Not yet

- **Program cycle output:** `H`, `D` and `T` records (heading, detail and total output of the program cycle), and fetch overflow (`F`).
- **Formats and editing:** externally described printer files (DDS `*PRTF` with `WRITE`), edit words, data formats `P`/`B`/`L`/`R`, and the overflow indicators `OA`-`OG`.
- **Printer settings:** `PRTCTL`, form length and overflow line from `OVRPRTF`/`CRTPRTF` (always 66/60).
- **Output queues:** `WRKOUTQ`, `CHGSPLFA`, `HLDSPLF`/`RLSSPLF`, `CPYSPLF`, and printing to a real printer or to PDF.
- **Unedited negative numbers** print with a trailing `-`. IBM i shows the sign in the last digit's zone.
