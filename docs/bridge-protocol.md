# Bridge protocol (version 1)

The bridge connects the JVM (Jariko running RPG) to OpenEye core, which carries out every file operation. It uses TCP on loopback only, with one UTF-8 JSON object per line in each direction, strictly request then response.

One connection is one job's database session. The handles it opens belong to that connection and disappear when it closes.

## Envelope

Request: `{"id": <any>, "op": "<operation>", ...}`
Success: `{"id": <same>, "ok": true, ...}`
Failure: `{"id": <same>, "ok": false, "error": "CPF5026: duplicate key on a UNIQUE file"}`

When an error corresponds to an IBM i message, its text starts with that message ID and a colon, so callers can map it to `%STATUS` or a monitor group.

## Values

Records are objects that map field names to strings:

- **Character fields** carry their full declared length, including trailing blanks.
- **Numeric fields** (packed `P`, zoned `S`) are plain decimal strings with the declared scale: `"1250.50"`, `"-75.25"`, `"0"`. Leading zeros and surrounding blanks are accepted on input. An empty string means zero.
- **Key values** use the same formats. Fewer values than the file has key fields means a partial key.

## Operations

| op | Request fields | Response fields | Notes |
| --- | --- | --- | --- |
| `hello` | | `server`, `version` | Sent first; clients refuse a version mismatch |
| `metadata` | `file` | `library`, `name`, `format`, `text`, `unique`, `keys`, `fields[]` | Each field has `name`, `type` (`A`/`P`/`S`), `length`, `decimals`, `numeric`, `text`. `file` may be `LIB/FILE`; otherwise the library list is searched |
| `open` | `file` | `handle` | |
| `close` | `handle` | | |
| `setll` | `handle`, `keys` | `equal` | `keys: []` means `*LOVAL`. `equal` is `%EQUAL` |
| `setgt` | `handle`, `keys` | | `keys: []` means `*HIVAL` |
| `chain` | `handle`, `keys` | read result | Never sets `eof` |
| `read`, `readp` | `handle` | read result | |
| `reade`, `readpe` | `handle`, `keys`? | read result | Without `keys`, the key of the last SETLL/SETGT/CHAIN is used |
| `write` | `handle`, `record` | `rrn` | Fields left out get their default (blanks or zero) |
| `update` | `handle`, `record` | | Acts on the last record read; fields left out are unchanged |
| `delete` | `handle` | | Acts on the last record read |
| `eof`, `equal` | `handle` | `eof` / `equal` | The flags from the last operation |

A **read result** is `{"found": bool, "eof": bool, "record": {...} | null, "rrn": n | null}`.

## Cursor semantics

These match IBM i record-level access for keyed physical files:

- **Access path order** is the key fields, then arrival order (RRN), so duplicate keys come back in the order they were written. Unkeyed files are read in RRN order.
- **`SETLL k`** positions before the first record with key ≥ k. **`SETGT k`** positions after the last record with key ≤ k.
- **`READE`** returns the next record only if its key equals the search key. Otherwise it sets `eof`, and the position moves past that key's records.
- **After `DELETE`** the position is kept, so `READ` continues with the next record.
- **`UPDATE` and `DELETE`** need a current record, meaning the last operation was a successful CHAIN or read. Otherwise they fail with `CPF5013`.

## Error IDs used so far

| ID | When |
| --- | --- |
| `CPF2110` | Library not found |
| `CPF5013` | UPDATE/DELETE with no current record, or the record is gone |
| `CPF5026` | Duplicate key on a UNIQUE file |
| `CPF7302` | File already exists (CRTPF) |
| `CPF9812` | File not found in the library list |
| `RNX0103` | Value does not fit the numeric field |
| `RNX0105` | Value is not a valid number |
