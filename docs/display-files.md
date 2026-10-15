# Display files (interactive RPG)

RPG programs that use DDS display files run on ibmish. When a program does `EXFMT`, ibmish draws the record format on the user's 5250 session, waits for input, and hands the field values and response indicators back to the program.

## How it works

```
 5250 session ◄── ibmish core ◄── bridge ◄── Jariko (EXFMT)
   (screen)       (DDS parsed,      "exfmt"     onExfmt callback:
                   drawn, input      op          sends field values
                   validated)                    and *IN01-*IN99,
                                                 applies the response
```

1. `ibmish crtdspf LIB/FILE --src FILE.dspf` checks and stores the DDS source. It is a `*FILE` object with attribute `DSPF`.
2. The runner gives Jariko the same source (bridge op `dspf`), so the program's variables match the display file's fields.
3. On `EXFMT`, the runner sends the record's field values and all 99 indicators (bridge op `exfmt`). ibmish draws the record, honoring option indicators, `DSPATR`, `COLOR`, `EDTCDE` and `ERRMSG`. It then validates the input: numbers must fit, and only the function keys defined with `CA`/`CF` are allowed. It returns the new field values and the response indicators.
4. The runner applies them to the program: fields through Jariko's `OnExfmtResponse`, indicators through the program's indicator table.

A program called from a batch job (`ibmish call`) has no display. `EXFMT` then fails with "this job has no display".

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
      * (No ITER: Jariko v2.2.0 does not support ITER inside DOW.)
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
 IBMISH                       Customer Inquiry                         10/15/26

 Type a customer number, press Enter.

   Customer number . . . .  42

















 F3=Exit
 Customer not found.
```

Customer 5. `EDTCDE(J)` edits the negative balance, and `*IN40` shows it in red:

```
 IBMISH                       Customer Inquiry                         10/15/26



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
| Input | Numeric fields are validated (digits, decimals, sign) and re-shown with a message if wrong. Character input is folded to upper case unless `CHECK(LC)`. |
| Response indicators | On input, the record's `CA`/`CF` indicators and `ERRMSG` indicators are set off, and the pressed key's indicator is set on. `CA` keys return no data; `CF` keys and Enter do. |

Keywords not in this list are accepted and ignored. Fidelity gaps (subfiles, `OVERLAY`, `PUTOVR`, `WINDOW`, `REF`/`REFFLD`, `VALUES`/`RANGE`/`COMP`, `EDTWRD`, 27x132 screens, job date formats) are listed in [fidelity-gaps.md](fidelity-gaps.md).

## Testing

`dspf.rs` has unit tests for parsing, indicator conditions, edit codes (expected values from IBM's published edit code table), numeric input, and response indicators. The bridge `exfmt` op has integration tests. `tools/e2e_5250.py` runs `CUSTINQ` through the real tn5250 client: the error message, DETAIL, edited numbers, F5 rejected, F12, and F3.
