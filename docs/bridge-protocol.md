# Bridge protocol (version 6)

Version 6 added program objects and source members: `pgmsrc` and `srcmbr`. Version 5 added spooled files: `splopen`, `splwrite`, `splclose`. Version 4 added record locks: `open` takes `update`, reads take `lock`, and the `unlock` operation. Version 3 added the data area operations `dtaara_in`, `dtaara_out` and `dtaara_unlock`. Version 2 added the display file operations `dspwrite`, `sflwrite`, `sflchain`, `sflreadc` and `sflupdate`.

The bridge connects the JVM (Jariko running RPG) to as4tainr core, which carries out every file operation. It uses TCP on loopback only, with one UTF-8 JSON object per line in each direction, strictly request then response.

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
| `open` | `file`, `update`? | `handle` | With `update: true` (RPG file type U) the file locks each record it reads; see [record-locks.md](record-locks.md) |
| `splopen` | `file`, `user_data` | `spl` | Starts a spooled file for the job (printer output). Without a job (`as4tainr call`), lines go to standard output instead |
| `splwrite` | `spl`, `lines` | | `lines` is a list of `[page, line, text]` |
| `splclose` | `spl` | | The spooled file is complete (status RDY); see [spooled-files.md](spooled-files.md) |
| `unlock` | `handle` | | Releases the record this open file holds locked (RPG `UNLOCK`) |
| `close` | `handle` | | |
| `setll` | `handle`, `keys` | `equal` | `keys: []` means `*LOVAL`. `equal` is `%EQUAL` |
| `setgt` | `handle`, `keys` | | `keys: []` means `*HIVAL` |
| `chain` | `handle`, `keys`, `lock`? | read result | Never sets `eof` |
| `read`, `readp` | `handle`, `lock`? | read result | On a file opened for update, reads lock the record found and release the one locked before; `lock: false` is the N extender. A record another job holds is waited for, then `CPF5027`; after a wait the record is read again |
| `reade`, `readpe` | `handle`, `keys`? | read result | Without `keys`, the key of the last SETLL/SETGT/CHAIN is used |
| `write` | `handle`, `record` | `rrn` | Fields left out get their default (blanks or zero) |
| `update` | `handle`, `record` | | Acts on the last record read; fields left out are unchanged |
| `delete` | `handle` | | Acts on the last record read |
| `eof`, `equal` | `handle` | `eof` / `equal` | The flags from the last operation |
| `dspf` | `file` | `library`, `name`, `source`, `formats` | A display file's DDS source, found through the library list |
| `exfmt` | `file`, `format`, `fields`, `indicators` | `aid`, `fields`, `indicators` | Shows a record format on the job's display and waits for input. Request `indicators` is 99 characters of `0`/`1` for `*IN01`-`*IN99`. The response has the new values of input fields, and the response indicators to set, as `{"03": true, "30": false}`. Fails when the job has no display. For a subfile control record, the subfile is shown below it and the operator can page through it; the records the operator typed into are kept for `sflreadc`. |
| `dspwrite` | `file`, `format`, `fields`, `indicators` | | WRITE of a record format: it is put on the screen (OVERLAY rules) and shown with the next EXFMT. For a subfile control record, `SFLCLR`/`SFLDLT` (if on) empty the subfile first |
| `sflwrite` | `file`, `format`, `rrn`, `fields`, `indicators` | | WRITE of a subfile record with relative record number `rrn` (1-9999). The indicators are kept with the record (option indicators, `SFLNXTCHG`) |
| `sflchain` | `file`, `format`, `rrn` | read result | CHAIN to a subfile record by number |
| `sflreadc` | `file`, `format` | read result | READC: the next subfile record changed by the operator in the last EXFMT (or written/updated with `SFLNXTCHG` on), in number order; `eof` when there are no more. `rrn` is the record's number |
| `sflupdate` | `file`, `format`, `fields`, `indicators` | | UPDATE of the subfile record last read by `sflreadc` or `sflchain` |
| `dtaara_in` | `name`, `lock`? | `library`, `name`, `type`, `length`, `decimals`, `value` | RPG `IN`: reads a data area found through the library list. `type` is `*CHAR`, `*DEC` or `*LGL`; `value` is the padded text, decimal text with `decimals` places, or `0`/`1`. With `lock: true` (`IN *LOCK`) this connection holds the data area until `dtaara_out`, `dtaara_unlock` or the connection closes; others wait |
| `dtaara_out` | `name`, `value` | | RPG `OUT`: writes the whole value (decimal text for `*DEC`); a value that does not fit fails. Releases this connection's lock |
| `dtaara_unlock` | `name` | | Releases this connection's lock without writing |
| `pgmsrc` | `name`, `library`? | `found`, `library`, `source` | The source of an RPG program object (CRTBNDRPG, attribute RPGLE) in `library`, or the first on the library list. The runner asks for the program called (the core passes its library in `AS4TAINR_PGM_LIB`) and for programs it calls. Needs *USE authority to the program. `found: false` when there is no such RPG program (the runner then looks in `AS4TAINR_PGM_PATH`) |
| `srcmbr` | `member`, `file`?, `library`? | `found`, `library`, `source` | `/COPY`: a source member (`file` default QRPGLESRC, `library` default the library list). A `member` with `.` or `/` in it is a stream file path (`/INCLUDE 'x.rpgleinc'`): the shortest stream file path ending with it; `library` is then the path found |

All connections of one bridge (one program call) share the display state: the records on the screen and the subfiles.

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
| `CPF1015` | Data area not found |
| `CPF2110` | Library not found |
| `CPF5013` | UPDATE/DELETE with no current record, or the record is gone |
| `CPF5026` | Duplicate key on a UNIQUE file |
| `CPF5027` | The record is locked by another job (after waiting); the text names the job |
| `CPF7302` | File already exists (CRTPF) |
| `CPF9812` | File not found in the library list |
| `RNX0103` | Value does not fit the numeric field |
| `RNX0105` | Value is not a valid number |
