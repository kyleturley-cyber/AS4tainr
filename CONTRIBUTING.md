# Contributing to ibmish

Thanks for helping. ibmish only works if it is both faithful to IBM i behavior and legally clean, so please read this whole page before your first pull request.

## Clean-room rules (mandatory)

ibmish reimplements IBM i *interfaces*. It never copies IBM *implementation*.

1. **Allowed sources:** IBM's public documentation (IBM Docs, Redbooks, CL/DDS/RPG references), published RFCs (TN5250: RFC 1205, 2877, 4777), open-source projects under compatible licenses, and behavior you observe on a system you are licensed to use.
2. **Never use** IBM binaries, licensed internal code, disassembly or decompilation of IBM software, or confidential IBM material. If you have seen IBM i source code under NDA, do not contribute to the component it covers.
3. **Command, API, opcode, keyword and message *identifiers* are interface facts.** Names like `CRTPF`, `CHAIN`, `CPF5026` are fine. Message *texts*, help texts and documentation are written fresh, in your own words.
4. **When you study a behavior on a real system**, write it down as a behavior spec or a golden-output test (see below), and implement from that.
5. **Customer code stays with the customer.** Only sample programs written for ibmish go into this repository.

## Developer Certificate of Origin

Every commit must be signed off (`git commit -s`). The sign-off certifies the [Developer Certificate of Origin](https://developercertificate.org/): you wrote the change, or you have the right to submit it under the project license, and it follows the clean-room rules above.

## Fidelity first

Business software must give the right answer to the cent, so every behavior needs a test that pins it down:

- **Rust core:** unit tests next to the code, integration tests in `core/tests/` (these need `IBMISH_TEST_DATABASE_URL`).
- **Programs:** add an RPG program to `samples/qrpglesrc/` and its expected `DSPLY` output to `samples/expected/PGM_PARM.txt`. `scripts/demo.sh` runs it in CI on the real Jariko runner.
- When you know what real IBM i does (from documentation or a licensed system), say where the expected result comes from in the test or the PR.
- Known differences go in [docs/fidelity-gaps.md](docs/fidelity-gaps.md). A documented gap is fine; a silent one is not.

## Using AI coding tools

AI assistants are welcome, and `CLAUDE.md` describes the project's conventions for them. You are responsible for every line you submit. Review generated code especially closely in the decimal, database and protocol code. A test that passes because it was written to match the code proves nothing, so check expected values against documentation or a real system.

## Workflow

```sh
cargo fmt && cargo clippy --all-targets -- -D warnings
IBMISH_TEST_DATABASE_URL=postgres://ibmish:ibmish@127.0.0.1/ibmish cargo test
cd jvm && ./gradlew fatJar && cd .. && scripts/demo.sh
```

Keep pull requests small and focused, and describe what IBM i behavior they implement.
