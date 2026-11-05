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
| Fields | Names, to choose some of the physical file's fields; none listed means all of them. `RENAME(field)`: a physical field under another name (one physical field may be in several fields). `CONCAT(field field ...)`: fields one after another; any character field makes a character field, only numeric fields (no decimal positions) a zoned decimal field of their digits with the sign of the rightmost, as long as their lengths together. `SST(field start length)`: part of a character or zoned field (a zoned field's digits as characters), a character field, input only. |
| Field attributes | A length (positions 30-34), data type (35) and decimal positions (36-37: a value, or `+n`/`-n`) of the field's own: numeric types to each other (P, S, B, F, within the notes of IBM's table of valid conversions), zoned decimal and character over each other (as many characters as digits), a character field's length. Decimal positions are cut, not rounded, when they are fewer (2.52 with none is 2), and a value too large for the physical field is an error. `VARLEN`, `TEXT`. |
| Usage (position 38) | B (the default) or I: an input-only field is read, left as it is when a record is updated through the logical file, and gets its default when one is written. N is for join logical files and is refused. |
| Keys | `K field`, with `DESCEND` for high-to-low order. `UNIQUE` at file level. Without keys, records come in arrival order. |
| Select/omit | `S` or `O` in column 17 starts a rule, with `COMP(EQ\|NE\|LT\|NL\|GT\|NG\|LE\|GE value)`, `VALUES(v ...)` or `RANGE(low high)`; following lines with a field name and a blank column 17 are ANDed to it; `ALL` ends with a default rule. The first rule that holds decides; when none holds, the record is omitted if the last rule was a select, and selected if it was an omit. Character constants are in apostrophes. `COMP(op field)` compares with another field, `COMP(EQ *NULL)` and `COMP(NE *NULL)` with the null value. A select/omit field is a field of the record format, else a physical field named on RENAME or CONCAT, else (as4tainr only) any physical field. `DYNSLT` at file level: select/omit is tested as records are read and the access path holds every record. |

Physical files also accept `DESCEND` on their key fields now.

## How it is stored

| IBM i | as4tainr |
| --- | --- |
| Logical file | A PostgreSQL view over the physical file's table: `rrn` and the record format's fields, each a column or an expression over the physical file's columns (`core/src/lfmap.rs`), `WHERE` the select/omit rules |
| Changes through it | When every field is a physical file column as it is (renamed or not), PostgreSQL changes the table through the view. Otherwise (a field converted, concatenated or input only, one physical field in two fields) an `INSTEAD OF` trigger on the view does it: fields are moved in their order, so the last one that sets a physical field gives its value, as IBM's reference says. The trigger's function is in the library, named after the file with `~lfw`; DLTF drops it, SAVLIB/SAVOBJ save it |
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

Join logical files (`JFILE`), several record formats in one logical file, keys on a physical field named on RENAME or CONCAT that is not in the record format, hexadecimal, graphic, date, time and timestamp conversions and substrings, and `TRNTBL`. PFILE must name a physical file, as on IBM i. See [fidelity-gaps.md](fidelity-gaps.md).
