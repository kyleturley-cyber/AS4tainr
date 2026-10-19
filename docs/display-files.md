# Display files (interactive RPG)

RPG programs that use DDS display files run on as4tainr. When a program does `EXFMT`, as4tainr draws the record format on the user's 5250 session, waits for input, and hands the field values and response indicators back to the program. Subfiles (the scrolling lists of "Work with..." screens) work too; see [Subfiles](#subfiles).

## How it works

```
 5250 session ◄── the job's display ◄── RPG engine (EXFMT)
   (screen)       (DDS drawn, input      sends the record's field values
                   validated)            and *IN01-*IN99, applies the
                                         response
```

1. `as4tainr crtdspf LIB/FILE --src FILE.dspf` checks and stores the DDS source. It is a `*FILE` object with attribute `DSPF`.
2. When an RPG program with the display file is compiled, the record formats' fields become program fields (numbers zoned).
3. On `EXFMT`, the engine sends the record's field values and all 99 indicators (or the `INDDS` data structure) to the job's display. as4tainr draws the record, honoring option indicators, `DSPATR`, `COLOR`, `EDTCDE` and `ERRMSG`. It then validates the input: numbers must fit, and only the function keys defined with `CA`/`CF` are allowed. It returns the new field values and the response indicators.
4. The engine puts them in the program's fields and indicators, and sets `*INKA`-`*INKY` from the key pressed.

A program called from a batch job (`as4tainr call`) has no display. `EXFMT` then fails with "this job has no display".

## Example

`samples/qddssrc/CUSTINQD.dspf`:

```
     A* CUSTINQD: customer inquiry display file (sample)
     A                                      DSPSIZ(24 80 *DS3)
     A                                      CA03(03 'Exit')
     A          R PROMPT
     A                                  1  2SYSNAME
     A                                  1 31'Customer Inquiry'
     A                                      DSPATR(HI)
     A                                  1 72DATE
     A                                  3  2'Type a customer number, -
     A                                      press Enter.'
     A                                  5  4'Customer number . . . .'
     A            CUSNUM         5Y 0B  5 29
     A  30                                  ERRMSG('Customer not found.' 30)
     A                                 23  2'F3=Exit'
     A                                      COLOR(BLU)
     A          R DETAIL
     A                                      CA12(12 'Cancel')
     A                                  1  2SYSNAME
     A                                  1 31'Customer Inquiry'
     A                                      DSPATR(HI)
     A                                  1 72DATE
     A                                  5  4'Customer number . . . .'
     A            CUSNUM         5Y 0O  5 29EDTCDE(Z)
     A                                  6  4'Name  . . . . . . . . .'
     A            CUSNAM        30   O  6 29
     A                                  7  4'City  . . . . . . . . .'
     A            CUSCTY        20   O  7 29
     A                                  8  4'Balance due . . . . . .'
     A            CUSBAL         9Y 2O  8 29EDTCDE(J)
     A  40                                  COLOR(RED)
     A                                 23  2'F3=Exit   F12=Cancel'
     A                                      COLOR(BLU)
```

`samples/qrpglesrc/CUSTINQ.rpgle`:

```
      * CUSTINQ: interactive customer inquiry (display file CUSTINQD).
      * PROMPT asks for a customer number; DETAIL shows the customer.
      * F3 ends the program; F12 on DETAIL returns to PROMPT.
      * An unknown customer turns on *IN30, which shows ERRMSG on PROMPT.
     FCUSTINQD  CF   E             WORKSTN
     FCUSTMAST  IF   E           K DISK
     C                   DOW       NOT *IN03
     C                   EXFMT     PROMPT
     C                   IF        NOT *IN03
     C     CUSNUM        CHAIN     CUSTMAST
     C                   IF        %FOUND
     C                   IF        CUSBAL < 0
     C                   EVAL      *IN40 = *ON
     C                   ELSE
     C                   EVAL      *IN40 = *OFF
     C                   ENDIF
     C                   EXFMT     DETAIL
     C                   ELSE
     C                   EVAL      *IN30 = *ON
     C                   ENDIF
     C                   ENDIF
     C                   ENDDO
     C                   SETON                                        LR
```

Customer 42 does not exist. The program turns on `*IN30`, so `ERRMSG` shows the message and highlights the field:

```
 AS4TAINR                       Customer Inquiry                         10/15/26

 Type a customer number, press Enter.

   Customer number . . . .  42

















 F3=Exit
 Customer not found.
```

Customer 5. `EDTCDE(J)` edits the negative balance, and `*IN40` shows it in red:

```
 AS4TAINR                       Customer Inquiry                         10/15/26



   Customer number . . . .      5
   Name  . . . . . . . . .  Lake, Pine & Co
   City  . . . . . . . . .  Duluth
   Balance due . . . . . .         75.25-














 F3=Exit   F12=Cancel
```

## Supported DDS

| Area | Supported |
| --- | --- |
| Records and fields | `R` record formats; named fields with length, data type `A`, `Y`, `S` (and `N`, `X`, `W` as character), decimal positions; usage `O`, `I`, `B`, `H`; line and position |
| Constants | `'text'` (with `-`/`+` continuation), `DATE` (MM/DD/YY), `TIME`, `USER`, `SYSNAME` |
| Option indicators | Up to three per line in positions 8-16, `N` for not, `O` lines for OR, condition-only lines that carry to the next line |
| Keywords | `CAnn`/`CFnn` (file or record level, with response indicator), `DSPATR(HI RI UL ND PR PC)`, `COLOR(WHT RED BLU TRQ YLW PNK GRN)`, `EDTCDE(1 2 3 4 J K L M Z)`, `ERRMSG('text' nn)`, `CHECK(LC)` |
| Input | Numeric fields are validated (digits, decimals, sign) and re-shown with a message if wrong. An input-capable numeric field with `EDTCDE` is shown edited (with `EDTCDE(Z)` a zero is blank); blanks and commas are taken out again on input. Character input is folded to upper case unless `CHECK(LC)`. |
| Response indicators | On input, the record's `CA`/`CF` indicators and `ERRMSG` indicators are set off, and the pressed key's indicator is set on. `CA` keys return no data; `CF` keys and Enter do. |
| Screen | `OVERLAY`: without it a record clears the screen; with it, only records sharing a line with it are removed. `WRITE` puts a record on the screen for the next `EXFMT`. |
| Subfiles | `SFL`, `SFLCTL`, `SFLSIZ`, `SFLPAG`, `SFLDSP`, `SFLDSPCTL`, `SFLCLR`, `SFLDLT`, `SFLEND(*MORE *PLUS)`, `SFLNXTCHG`, `SFLMSG`, `PAGEDOWN`/`ROLLUP`, `PAGEUP`/`ROLLDOWN` |

Keywords not in this list are accepted and ignored. Fidelity gaps (`SFLRCDNBR`, `PUTOVR`, `WINDOW`, `REF`/`REFFLD`, `VALUES`/`RANGE`/`COMP`, `EDTWRD`, 27x132 screens, job date formats) are listed in [fidelity-gaps.md](fidelity-gaps.md).

## Subfiles

A subfile is a list of records, numbered 1 to 9999 (relative record numbers), that the program fills with `WRITE` and the system shows a page at a time under a control record. The program names the subfile record format and the variable holding the record number on the F spec:

```
     FCUSTWRKD  CF   E             WORKSTN
     F                                     SFILE(CUSTSFL:RRN)
```

| Program does | as4tainr does |
| --- | --- |
| `WRITE ctl` with `SFLCLR` (or `SFLDLT`) on | Empties the subfile |
| `WRITE sfl` | Adds the record at number `RRN`, with the current indicators (for option indicators and `SFLNXTCHG`). A number already used, or outside 1-9999, is an error |
| `WRITE other` | Puts the record on the screen; it shows with the next `EXFMT` |
| `EXFMT ctl` | Shows the control record and, if `SFLDSP` is on, the first page of the subfile (`SFLPAG` records) with `More...`/`Bottom` for `SFLEND(*MORE)`. Page Down and Page Up move through the records without the program. Past the last page, `PAGEDOWN`/`ROLLUP` hands the key to the program (load-a-page-at-a-time subfiles); without it, the operator gets a message |
| `READC sfl` | Returns the next record the operator typed into, in record number order, and sets `RRN`. Records written or updated with `SFLNXTCHG` on count as typed into. `%EOF` when none is left |
| `CHAIN n sfl` / `UPDATE sfl` | Reads record `n` / rewrites the record last read |

The DDS rules are checked by `crtdspf`: the control record comes right after its subfile record and names it in `SFLCTL`, it has `SFLSIZ` and `SFLPAG`, and a page fits on the display. With `SFLSIZ` equal to `SFLPAG` the subfile cannot grow past one page; with `SFLSIZ` larger it grows as needed, as on IBM i.

### How it is done

The RPG engine sends `WRITE`, `READC`, `CHAIN` and `UPDATE` of display records to the program's display state, `core/src/workstn.rs`; `SFILE(format:field)` names the field that holds each subfile's record number.

The subfile, the records on the screen, and paging all live in as4tainr core (`core/src/workstn.rs`), where they are unit tested.

### Example

`samples/qrpglesrc/WRKCUST.rpgle` with `samples/qddssrc/CUSTWRKD.dspf` loads every customer into a subfile of 4 lines a page. Option 5 shows the customer; a wrong option is shown in reverse image with an `SFLMSG` message, and stays selected through `SFLNXTCHG`:

```
 AS4TAINR                    Work with Customers                         10/15/26

 Type options, press Enter.
   5=Display

 Opt  Cust   Name                            City
          1  Acme Tooling                    Des Moines
  Z       2  Midwest Fasteners               Chicago
          3  Bluebird Supply                 Omaha
          4  Prairie Freight                 Lincoln
                                                                        More...











 F3=Exit   F12=Cancel
 Type option 5 or leave the option blank.
```

## Testing

`dspf.rs` has unit tests for parsing, indicator conditions, edit codes (expected values from IBM's published edit code table), numeric input, and response indicators. `workstn.rs` tests the subfile DDS rules, record numbers, `SFLCLR`, `OVERLAY`, paging and `SFLEND`, `READC` order and `SFLNXTCHG`, `CA` keys and `SFLMSG`. Integration tests run CUSTINQ and WRKCUST on the RPG engine with a stand-in display. `tools/e2e_5250.py` runs `CUSTINQ` and `WRKCUST` through the real tn5250 client: error messages, edited numbers, function keys, Page Down/Up, `More...`/`Bottom`, option 5, a wrong option.
