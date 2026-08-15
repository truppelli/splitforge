# Contributing to SplitForge

SplitForge times real races. A bug here does not produce a stack trace in a log file —
it produces a runner who drove four hours, ran a personal best, and has no finish time.
That reality sets the bar for everything below.

## Before you start

SplitForge is pre-alpha and the architecture is still being established. Please open an
issue to discuss anything larger than a bug fix before writing code. Work that crosses a
crate boundary or changes a stored data shape needs an
[ADR](docs/adr/) first.

## Development setup

Requires a current stable Rust toolchain (edition 2024, MSRV 1.85).

```bash
cargo test --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```

CI runs exactly these, plus `cargo audit`, `cargo deny check`, and a
`aarch64-unknown-linux-gnu` cross-build. Run them locally before pushing.

`Cargo.lock` **is committed** — this workspace produces binaries, and reproducible field
builds matter more than dependency freshness. Include lockfile changes in your PR.

## The rules that are not negotiable

These come from [ADR-0005](docs/adr/0005-raw-read-append-only-journal.md) and
[ADR-0006](docs/adr/0006-optional-outbound-integrations.md). A PR that violates one will
be rejected regardless of how well it is written.

1. **Never mutate or delete a raw read** on any normal code path. Corrections produce new
   derived records or a new result revision. If you find yourself writing
   `UPDATE raw_reads`, stop and open an issue.
2. **Never put a network call on the read path.** Persisting a read must not depend on
   any remote service being reachable, including RaceDay Connect.
3. **Never let `splitforge-engine` depend on `splitforge-llrp`** — or on any other
   protocol crate. The engine consumes normalized `RawRead` values only.
4. **`splitforge-domain` takes no I/O dependencies.** No database, no HTTP, no async
   runtime, no filesystem. Ports are traits; implementations live elsewhere.
5. **All persisted timestamps are UTC**, and every timestamp records which clock produced
   it. See [the timing model](docs/timing-model.md).

## Code standards

| Concern | Convention |
|---|---|
| Errors | `thiserror` in libraries, `anyhow` only at application boundaries (`apps/`, `splitforge-cli`) |
| Async | Tokio. Keep it out of `splitforge-domain` |
| Logging | `tracing`. Structured fields, not interpolated strings |
| Time | UTC everywhere in storage. Local time only at display boundaries |
| Unsafe | Denied workspace-wide. If you need it, open an issue first |
| Panics | No `unwrap`/`expect` on any path reachable during an event, including reader input parsing |

Malformed reader input is **expected**, not exceptional. Protocol parsing must return
errors, never panic — a single corrupt frame from a reader may not take down the timer.

## Testing expectations

- Domain logic gets unit tests. Dedup, placement, and status transitions get property
  tests (`proptest`) where the invariant is clearer than the examples.
- Anything touching storage gets a durability test: write, kill the process, reopen,
  assert nothing was lost.
- Reader adapters get tests driven by **recorded captures**, not live hardware, so CI can
  run them.
- New scoring rules need a fixture-driven end-to-end test in `splitforge-testkit`.

## Commits and pull requests

- Write commit subjects in the imperative mood: `Add duplicate suppression window`.
- Keep PRs scoped to one concern. A PR that renames things *and* changes behavior is two
  PRs.
- Describe what you tested, and on what. "Tested on a Pi 3 with a simulated reader" and
  "compiles" are different claims — say which one is true.
- Do not claim hardware support in docs or release notes for a device you have not
  physically tested. See [`docs/hardware-support.md`](docs/hardware-support.md).

## Architecture decision records

Substantive decisions are recorded in [`docs/adr/`](docs/adr/). Copy
[`docs/adr/template.md`](docs/adr/template.md), number it sequentially, and open it as its
own PR when the decision is significant enough to argue about separately from code.

## Code of conduct

Participation is governed by [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md).
