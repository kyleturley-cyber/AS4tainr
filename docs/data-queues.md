# Data queues and user spaces

Two kinds of objects that programs use to pass data: data queues (*DTAQ), which jobs send entries to and receive them from, and user spaces (*USRSPC), named blocks of bytes that the list APIs fill. Both are written from IBM's documentation (CL reference: CRTDTAQ; Object APIs: "Data Queue APIs" and "User Space APIs"; the list APIs QUSLMBR and QUSLOBJ). They are called the IBM way: `CALL QSNDDTAQ PARM(...)` from CL, a prototype with `EXTPGM('QSNDDTAQ')` from RPG.

## Data queues (`core/src/dtaq.rs`)

| Command or API | What it does |
| --- | --- |
| `CRTDTAQ DTAQ(lib/name) MAXLEN(n) SEQ(*FIFO\|*LIFO\|*KEYED) KEYLEN(k) SENDERID(*YES)` | A data queue: entries of up to `n` bytes (1-64512), received first in first out, last in first out, or by key (keys of `k` bytes, 1-256). FORCE, SIZE and AUTORCL are kept. CPF9870 if it exists, CPF9810 without the library |
| `DLTDTAQ DTAQ(lib/name)` | The queue and its entries (CPC2191; CPF2105 when not found) |
| `QSNDDTAQ` (name, library, length, data [, key length, key [, asynchronous request [, data is from a journal entry]]]) | Sends an entry. CPF2498 when longer than MAXLEN; CPF9501, CPF9502, CPF9506, CPF9507 for keys that do not fit the queue |
| `QRCVDTAQ` (name, library, length, data, wait [, key order, key length, key, sender information length, sender information [, remove, size of data receiver, error code]]) | Receives the first entry: by key with EQ, NE, GT, GE, LT or LE (lowest key first, equal keys in the order sent; the key received is returned in the key parameter). Wait: less than 0 forever, 0 not at all (length 0 when there is no entry), else seconds. Remove *NO leaves the entry; the size of data receiver cuts what is copied, not the length returned. Sender information (queues created with SENDERID(*YES)): bytes returned and available (PACKED(7,0) each), job name, user, job number and current user (44 bytes) |
| `QCLRDTAQ` (name, library [, key order, key length, key, error code]) | Removes all entries, or a keyed queue's entries whose keys match |
| `QMHQRDQD` (receiver, length, RDQD0100, qualified name) | The queue's description: entry length, key length, sequence (F, L or K), sender ID, force, text, automatic reclaim, number of entries, the library it was found in, maximum and initial entries, size |

A receive removes the entry it returns in one statement, so two jobs waiting on the same queue never get the same entry. A receive that waits looks again every fifth of a second. Entries are bytes as programs see them: EBCDIC (CCSID 37) for text from CL. Authority: *USE to receive and retrieve the description, *CHANGE to send and clear (IBM's *ADD, *READ and *DLT data authorities are part of those).

## User spaces (`core/src/usrspc.rs`)

| API | What it does |
| --- | --- |
| `QUSCRTUS` (qualified name, extended attribute, initial size, initial value, public authority, text [, replace, error code [, domain [, transfer size, alignment]]]) | A user space of 1 to 16,776,704 bytes, each set to the initial value. Replace *YES replaces an existing one (its authorities kept); *NO fails with CPF9870 |
| `QUSDLTUS` (qualified name, error code) | Deletes it |
| `QUSRTVUS` (qualified name, starting position, length, receiver [, error code]) | Bytes from a 1-based position |
| `QUSCHGUS` (qualified name, starting position, length, data, force [, error code [, returned library]]) | Changes bytes; past the end only when the space extends itself |
| `QUSRUSAT` (receiver, length, SPCA0100, qualified name, error code) | Size, automatic extendibility, initial value, library |
| `QUSCUSAT` (returned library, qualified name, attributes, error code) | Changes the size (key 1), initial value (key 2) and automatic extendibility (key 3) |
| `QUSPTRUS` (qualified name, pointer [, error code]) | RPG only: a pointer to the space's bytes. The program changes them in place; they are written back when it calls another program or API and when it ends, and read again after a call |
| `QUSLMBR` (space, MBRL0100 or MBRL0200, qualified file, member, override [, error code]) | The members of a file: names (MBRL0100), or names, source types, creation and change dates (CYYMMDDHHMMSS) and texts (MBRL0200) |
| `QUSLOBJ` (space, OBJL0100 or OBJL0200, qualified object name, type [, error code]) | Objects by name (generic, *ALL) in a library or *LIBL, *CURLIB, *ALL, *ALLUSR: name, library, type (OBJL0100), and the extended attribute and text (OBJL0200) |
| `QUSLFLD` (space, FLDL0100, qualified file, record format or *FIRST, override [, error code]) | A file's fields (560-byte entries): name, data type, use, buffer positions, length, digits, decimal positions, text, column heading, alternative name, null capable, date and time format, varying length, CCSIDs; the header section has the record length, format level identifier and text |
| `QUSLSPL` (space, SPLF0100, user, output queue, form type, user data [, error code [, qualified job]]) | Spooled files (88-byte entries): user, output queue, form type, user data and the internal job and spooled file identifiers that QUSRSPLA takes |

The list APIs write IBM's generic header (format 0100) at the start of the space and leave its user area (bytes 0-63) alone: the offset to the list data at 124, the number of entries at 132, the size of each entry at 136, information status 'C' at 103. A space too small for the list is made larger.

## Retrieve APIs (`core/src/sysapi.rs`)

| API | What it returns |
| --- | --- |
| `QUSROBJD` (receiver, length, OBJD0100 or OBJD0200, qualified object, type [, error code]) | Name, library, type, the library found when *LIBL or *CURLIB was given, ASP 1, owner, domain (*U for user spaces, *S otherwise), creation and change dates; OBJD0200 adds the extended attribute and text |
| `QUSRMBRD` (receiver, length, MBRD0100 or MBRD0200, qualified file, member (*FIRST, *LAST, a name), override [, error code]) | File, library, member, PF or LF, source type, dates, text, whether a source file; MBRD0200 adds the record count and other counters |
| `QUSRSPLA` (receiver, length, SPLA0100, qualified job ('*' this job, '*INT'), internal job ID, internal spooled file ID, spooled file name ('*INT'), number (-1 the last, 0 the only one) [, error code]) | The spooled file's identifiers, job, name, number, user data, status (*READY, *OPEN, *HELD), total pages, output queue, the date and time it was created |

`sysapi.rs` also decides which of these APIs a CALL from CL or RPG goes to (`sysapi::is_api`, `call_api`); a program of the same name in a library named on the call (other than QSYS) is called instead.

## Error codes

The APIs that have an error code parameter (ERRC0100) return their errors in it when its bytes provided is 8 or more: bytes available 15 (or the bytes provided, if less) and the exception ID at offset 8. Otherwise errors are escape messages, monitored with MONMSG in CL and ON-ERROR or (E) in RPG.

What is not done is listed in [fidelity-gaps.md](fidelity-gaps.md) under "Data queues" and "User spaces".
