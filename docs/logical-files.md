# Logical files

A logical file gives programs another view of a physical file's records: in another key order, with fewer fields, or only the records that pass its select/omit rules. RPG opens it like any database file (`F...IF E K DISK`), and it holds no data of its own: a record changed through the physical file shows up in the logical file at once, and the other way round.

```sh
as4tainr crtlf QGPL/CUSTOWE --src samples/qddssrc/CUSTOWE.lf
```

## Supported DDS

Written from IBM's DDS reference for logical files.

| Area | Supported |
| --- | --- |
| Record format | `R format PFILE(file)` or `PFILE(lib/file)`, over one physical file. The physical file is found through the library list when not qualified. `TEXT`. |
| Fields | Names only, to choose some of the physical file's fields; none listed means all of them. |
| Keys | `K field`, with `DESCEND` for high-to-low order. `UNIQUE` at file level. Without keys, records come in arrival order. |
| Select/omit | `S` or `O` in column 17 starts a rule, with `COMP(EQ\|NE\|LT\|NL\|GT\|NG\|LE\|GE value)`, `VALUES(v ...)` or `RANGE(low high)`; following lines with a field name and a blank column 17 are ANDed to it; `ALL` ends with a default rule. The first rule that holds decides; when none holds, the record is omitted if the last rule was a select, and selected if it was an omit. Character constants are in apostrophes. |

Physical files also accept `DESCEND` on their key fields now.

## How it is stored

| IBM i | as4tainr |
| --- | --- |
| Logical file | A PostgreSQL view over the physical file's table: `rrn` and the chosen fields, `WHERE` the select/omit rules |
| Its access path | An index on the physical file's table, in key order (DESC for DESCEND keys) then arrival order, partial (`WHERE` select/omit) when there are rules; character keys in EBCDIC order (lowercase, uppercase, digits), as on IBM i |
| UNIQUE | A partial unique index on the keys, over the records the logical file selects: records it omits do not count as duplicates |

Record-level access needs nothing special: SETLL, SETGT, CHAIN, READ, READP, READE, READPE work on the view, and WRITE, UPDATE and DELETE through it change the physical file (a record written through a logical file that its rules omit is still written, as on IBM i). With a DESCEND key, "after" a key means a smaller value: `SETLL 1250.50` on a descending balance key positions at the first balance at or below 1250.50.

## Example

`samples/qddssrc/CUSTOWE.lf` and `samples/qrpglesrc/OWING.rpgle`:

```
     A* CUSTOWE: customers with a balance due, largest balance first
     A          R CUSTREC                   PFILE(CUSTMAST)
     A                                      TEXT('Customers who owe')
     A          K CUSBAL                    DESCEND
     A          S CUSBAL                    COMP(GT 0)
```

`CALL OWING PARM('L')` prints the customers who owe, largest first, then two customers in name order from `L` (logical file CUSTNAME). The demo checks it against `samples/expected/OWING_L.txt`.

## Not supported yet

Join logical files (`JFILE`), several record formats in one logical file, `RENAME`, `CONCAT`, `SST`, changing a field's length or type, `DYNSLT`, comparing to another field in `COMP`, and logical files over logical files. See [fidelity-gaps.md](fidelity-gaps.md).
