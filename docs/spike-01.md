# Spike 01: Rust core → Jariko → PostgreSQL

**Question:** can RPG programs running on Jariko do their file I/O through a Rust core that stores the data in PostgreSQL, with IBM i record-level-access semantics?

**Answer so far: yes for the Rust core, the protocol and the Java client. These were built and tested end to end on 2026-10-14. On 2026-10-15 the Jariko adapter compiled against Jariko v2.2.0. Running the sample programs on Jariko against PostgreSQL (CI job `jariko`) is the remaining check.**

## What was built

| Piece | Status | Evidence |
| --- | --- | --- |
| DDS parser for physical files (A/P/S fields, keys, UNIQUE, TEXT) | Done | 3 unit tests |
| Catalog: libraries as schemas, file descriptions, library list | Done | Integration tests |
| Record-level access: SETLL, SETGT, CHAIN, READ, READP, READE, READPE, WRITE, UPDATE, DELETE | Done | 12 integration tests on PostgreSQL 16 |
| Bridge server (JSON lines over loopback TCP) | Done | Protocol round-trip test |
| Java bridge client, no dependencies | Done | `SELFTEST`: 11 checks pass |
| `ibmish call`: starts the bridge, launches the JVM runner, returns its exit code | Done | `scripts/demo.sh` matches the golden outputs |
| Jariko adapter (reload `DBMManager` + `DBFile`) and runner | Compiles (Windows, JDK 21, Jariko v2.2.0); end-to-end run on Jariko still pending | CI job `jariko` |
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

## Open items before calling the spike done

1. ~~Build `ibmish-jvm.jar` against the pinned Jariko release~~ (done 2026-10-15). Still to do: run both sample programs on Jariko and match their golden output (CI job `jariko`).
2. Confirm the number format Jariko sends on WRITE/UPDATE (`asString()` of packed values) parses as a plain decimal.
3. Confirm how Jariko renders `DSPLY` on standard output, and adjust the golden-file comparison if it adds a prefix.
4. Decide on the EBCDIC collating sequence fix (see fidelity-gaps.md).
