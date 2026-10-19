# Order entry sample (the Phase 2 gate)

Phase 2 of the roadmap ends when "a sample order-entry app runs unmodified". This is that app. It is written the way such programs are written on IBM i: fixed-form RPG IV with an input subfile, DDS physical files and a display file, a data area for the next order number, and a CL program that starts it. Nothing in it is specific to as4tainr.

The end-to-end test (`tools/e2e_5250.py`, run in CI with the real tn5250 client) signs on, enters an order with mistakes, corrects them, creates the order and checks the files.

## Objects

| Object | Type | Source | What |
| --- | --- | --- | --- |
| `ORDENTC` | `*PGM` (CLLE) | `samples/qclsrc/ORDENTC.clle` | Checks the files and the data area `ORDNXT` with `CHKOBJ` (`MONMSG CPF9801`), calls `ORDENT`, then reports the next order number with `RTVDTAARA` |
| `ORDENT` | RPG | `samples/qrpglesrc/ORDENT.rpgle` | The order entry program |
| `ORDENTD` | `*FILE` (DSPF) | `samples/qddssrc/ORDENTD.dspf` | Customer prompt, order lines subfile (20 lines, 10 a page), footer |
| `CUSTMAST` | `*FILE` (PF) | `samples/qddssrc/CUSTMAST.pf` | Customers; the order total is added to `CUSBAL` |
| `ITEMMAST` | `*FILE` (PF) | `samples/qddssrc/ITEMMAST.pf` | Items with price and quantity on hand |
| `ORDHDR` | `*FILE` (PF) | `samples/qddssrc/ORDHDR.pf` | Order headers |
| `ORDLIN` | `*FILE` (PF) | `samples/qddssrc/ORDLIN.pf` | Order lines |
| `ORDNXT` | `*DTAARA` (`*DEC 7 0`) | `scripts/samples.sh` (`as4tainr system "CRTDTAARA ..."`) | The next order number, 1004 after the sample orders |

`scripts/samples.sh` creates them in `QGPL`, and the Docker image runs it at start-up.

## Try it

Sign on and type `CALL ORDENTC`.

```
 AS4TAINR                           Order Entry                          10/16/26

 Customer  . . :     2  Midwest Fasteners
 City  . . . . : Chicago

 Type items and quantities, press Enter to check them.

 Item               Qty  Description                        Price       Amount
 BOLT-M8             40  Hex bolt M8 x 30                     .15         6.00
 NOSUCH               1                                       .00          .00
                                                                        More...

                                             Order total . . :           6.00

 F3=Exit   F6=Create order   F12=Cancel
 Item not found.
```

1. Type a customer number (1 to 5) and press Enter.
2. Type item numbers and quantities. Items: `BOLT-M8`, `NUT-M8`, `WASHER-M8`, `WIDGET-A`, `GASKET-12`, `PUMP-200`.
3. Press Enter to check the lines. Unknown items and missing quantities are shown in reverse image with a message, and the cursor goes to the first one.
4. Press F6 to create the order. The prompt comes back with "Order 1004 created, total ...".
5. Press F3 to end. `ORDENTC` says what the next order number is.

Then look at what changed: `DSPPFM ORDHDR`, `DSPPFM ORDLIN`, `DSPPFM ITEMMAST`, `DSPDTAARA ORDNXT`, or `STRSQL` and `SELECT * FROM ORDHDR`.

## What it exercises

- **CL:** `CHKOBJ`, `MONMSG` with `GOTO`, `CALL`, `RTVDTAARA`, `SNDPGMMSG` (`*COMP`, `*ESCAPE`).
- **RPG:** `EXFMT`, `WRITE`/`CHAIN`/`UPDATE` of subfile records, `CHAIN(N)` (read without lock), `CHAIN`/`UPDATE` and `WRITE` on database files, `*DTAARA DEFINE` with `IN *LOCK`/`OUT`, subroutines, named constants, `Z-ADD` of `UYEAR`/`UMONTH`/`UDAY`, and field names shared between the display file and database files (`ITEM`, `QTY`, `PRICE`, `ITMDSC`, `ORDTOT`).
- **DDS:** `SFL`/`SFLCTL` with `SFLSIZ` larger than `SFLPAG`, `SFLEND(*MORE)`, `SFLMSG`, `CF06` (returns data) and `CA12` (does not), option indicators on fields and constants, `DSPATR(RI PC)`, `ERRMSG`, `EDTCDE(Z)` on an input field, `EDTCDE(2)` and `(J)`, `OVERLAY`.

## Many users at once (the Phase 3 gate)

`tools/gate3_5250.py` has three clerks enter orders with this application at the same time, on the same customers and items, while batch jobs print reports. It then checks that the files add up:

- every order has its own number, with no gaps;
- each item's quantity on hand went down by exactly what was ordered;
- each customer's balance went up by exactly their order totals.

What makes them add up is `IN *LOCK` on the data area and the record locks on CUSTMAST and ITEMMAST (see [record-locks.md](record-locks.md)).

The first run of the gate found a race in an earlier version of `ORDENTC`: it created `ORDNXT` the first time it ran. Two clerks starting at the same moment both tried to create it, and the one that lost failed with `CPF1023`. A data area created in the wrong library also restarted the numbering. `ORDNXT` is now created with the sample files.

## Found while building it

- On IBM i every `CHAIN` reads the file, and the record found is the one `UPDATE` changes. (Jariko, the RPG runner used then, cached `CHAIN` results; with the cache, `UPDATE` could overwrite another record. The RPG engine that replaced it has no such cache.)
- An input-capable numeric field with an edit code is now shown edited, so a zero quantity with `EDTCDE(Z)` is blank instead of `0`.
