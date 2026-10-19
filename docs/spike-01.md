# Spike 01: Rust core → Jariko → PostgreSQL

**Question:** can RPG programs running on Jariko do their file I/O through a Rust core that stores the data in PostgreSQL, with IBM i record-level-access semantics?

**Answer: yes.** On 2026-10-15 the real Jariko v2.2.0 runner (built on Windows) ran all three sample RPG programs, CUSTLIST, ORDTOTAL and CUSTUPD, against PostgreSQL through the Rust core. Their output matched the golden files, including READ, READE, CHAIN, UPDATE and WRITE with negative and fractional packed values.

## What was built

| Piece | Status | Evidence |
| --- | --- | --- |
| DDS parser for physical files (A/P/S fields, keys, UNIQUE, TEXT) | Done | 3 unit tests |
| Catalog: libraries as schemas, file descriptions, library list | Done | Integration tests |
| Record-level access: SETLL, SETGT, CHAIN, READ, READP, READE, READPE, WRITE, UPDATE, DELETE | Done | 12 integration tests on PostgreSQL 16 |
| Bridge server (JSON lines over loopback TCP) | Done | Protocol round-trip test |
| Java bridge client, no dependencies | Done | `SELFTEST`: 11 checks pass |
| `as4tainr call`: starts the bridge, launches the JVM runner, returns its exit code | Done | `scripts/demo.sh` matches the golden outputs |
| Jariko adapter (reload `DBMManager` + `DBFile`) and runner | Done | Built on Windows (JDK 21); 3 sample programs match golden output on Jariko |
| Docker image + compose | Written, not yet built | CI job `docker` |

## Measurements

These numbers come from the stand-in Java runner on the same host as PostgreSQL 16 (2 vCPU sandbox), using a 20,000-record keyed file, a release build, and one bridge round trip per operation.

| Operation | Throughput | Latency |
| --- | --- | --- |
| Sequential READ | ~3,250 records/s | ~310 µs |
| Random CHAIN (full key) | ~3,100 lookups/s | ~320 µs |
| WRITE (CSV load, debug build) | ~1,200 records/s | ~850 µs |

That's fine for interactive programs, but slow for batch jobs that read millions of records. IBM i reads input-only files in blocks. The planned improvements are record blocking for input-only opens, prepared-statement caching per open file, and batching WRITEs inside a commitment boundary.

## Bug found by the benchmark

Sequential READ stopped after 4,000 of 20,000 records. The cause: `ORDER BY ordno` in PostgreSQL bound to the select-list output `ordno::text`, so numeric keys sorted as text. The original tests passed because every sample key had the same number of digits. The fix qualifies every column with a table alias, which also lets PostgreSQL use the key index (8 ms → 0.3 ms per read). A regression test now covers keys of different widths.

This is exactly the "plausible but wrong" failure that AI-assisted development risks. The lesson: test data must include awkward values (different widths, negatives, blanks, duplicate keys) on purpose.

## Findings from the first Jariko run

1. **Jariko prints a banner on standard output.** `io.as4tainr.jvm.Launcher` now initializes Jariko with standard output pointed at standard error.
2. **`%FOUND(filename)` is not implemented in Jariko v2.2.0.** The sample uses `%FOUND` with no argument instead. This is a good first upstream contribution (see fidelity-gaps.md).
3. **Numbers on WRITE/UPDATE work.** Jariko sends plain decimal strings; -75.25 + 10.25 stored as -65.00, and -1234.56 round-trips.
4. **DSPLY output is padded** to the message field's length; the golden comparison ignores trailing blanks.
5. **Jariko's `ChainCacheFlag` is on by default.** Cached CHAIN results could go stale when another job updates the same record. To review when record locking and multi-job work begin.

## Open items

- Decide on the EBCDIC collating sequence fix (see fidelity-gaps.md).
