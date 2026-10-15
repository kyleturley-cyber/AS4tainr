# CLAUDE.md

Guidance for AI coding sessions working in this repository.

## What OpenEye is

An IBM i-compatible runtime for containers. It reimplements IBM i interfaces (objects and libraries, DDS files, record-level access, later 5250, CL and jobs) on Linux and PostgreSQL. RPG runs on Jariko (JVM). It is NOT a hardware emulator.

## Non-negotiable rules

1. **Clean room.** Implement only from public IBM documentation, RFCs, compatible open-source code, and documented observed behavior. Never reproduce IBM source code, IBM message texts or help texts, or code from memory of proprietary IBM software. Identifiers (command names, opcodes, message IDs like `CPF5026`) are fine. Write message texts fresh.
2. **No floating point for business values.** Packed and zoned decimals stay exact: they are strings at the boundaries and `numeric(p,s)` in PostgreSQL. Never parse them into `f64`/`double`.
3. **Fidelity changes need tests.** Every behavior change gets a test whose expected value comes from IBM documentation or a real system, not from running the new code. Say where the expected value came from in a comment.
4. **Document gaps.** If you implement something only partly, add it to `docs/fidelity-gaps.md`.
5. **Do not weaken tests to make them pass.** If a test looks wrong, explain why instead of editing it.

## Layout

| Path | What |
| --- | --- |
| `core/src/dds.rs` | DDS parser (physical files: R, K lines, fields, `UNIQUE`, `TEXT`) |
| `core/src/catalog.rs` | Libraries = PostgreSQL schemas; file descriptions in schema `openeye`; library list resolution |
| `core/src/rla.rs` | Record-level access: cursor position per open file, keyset queries ordered by (keys, rrn) |
| `core/src/bridge.rs` | JSON-lines TCP server the JVM calls for every file operation |
| `core/src/main.rs` | `openeye` CLI |
| `core/tests/rla_test.rs` | Integration tests (need `OPENEYE_TEST_DATABASE_URL`) |
| `jvm/src/main/java/io/openeye/bridge` | Dependency-free Java bridge client (keep it dependency-free) |
| `jvm/src/main/kotlin/io/openeye/jvm` | Jariko integration: reload `DBMManager`/`DBFile` adapter and runner `main` |
| `jvm/src/test/java/.../BridgeConformance.java` | JDK-only stand-in runner used for end-to-end tests without Maven |
| `samples/` | DDS, CSV data, RPG programs, golden outputs in `expected/` |
| `docs/bridge-protocol.md` | Protocol spec. Update it in the same change as the protocol |

## Commands

```sh
cargo fmt && cargo clippy --all-targets -- -D warnings
OPENEYE_TEST_DATABASE_URL=postgres://openeye:openeye@127.0.0.1/openeye cargo test
javac -d target/conformance-classes jvm/src/main/java/io/openeye/bridge/*.java jvm/src/test/java/io/openeye/bridge/*.java
OPENEYE_BIN=target/debug/openeye OPENEYE_RUNNER="java -cp target/conformance-classes io.openeye.bridge.BridgeConformance" scripts/demo.sh
cd jvm && ./gradlew fatJar   # needs Maven Central
```

## Conventions

- Errors that a program could monitor for start with the IBM-style message ID, e.g. `CPF5026: ...`.
- SQL is built dynamically only with `catalog::ident()` for identifiers. Values are always bound parameters (as text, then `CAST`).
- Bridge protocol changes are versioned: bump `PROTOCOL_VERSION` in both `bridge.rs` and `BridgeClient.java`.
- Commits are signed off (`git commit -s`). Commit messages describe the IBM i behavior implemented.
