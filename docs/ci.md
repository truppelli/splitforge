# Continuous Integration

Implemented in [`.github/workflows/ci.yml`](../.github/workflows/ci.yml). Runs on every
pull request and every push to `main`.

## Jobs

| Job | Command | Why it exists |
|---|---|---|
| **Format** | `cargo fmt --all --check` | Removes style from code review entirely |
| **Clippy** | `cargo clippy --workspace --all-targets --all-features -- -D warnings` | Warnings are errors. A warning nobody fixes is a warning nobody reads |
| **Test** | `cargo test --workspace --all-features` | — |
| **MSRV** | `cargo check --all-targets` on Rust 1.88.0 | Establishes what the floor actually is, rather than what the manifest claims. `--all-targets` so a dev-dependency cannot raise it unnoticed. The floor is a tested fact, not a promise — [ADR-0013](adr/0013-msrv-policy.md) |
| **Security advisories** | `cargo audit --deny warnings` | Known vulnerabilities in the dependency tree |
| **Licenses and bans** | `cargo deny check` | License compatibility ([ADR-0007](adr/0007-license-selection.md)), duplicate versions, unknown registries |
| **Cross-build** | `cargo build --release --target aarch64-unknown-linux-gnu --workspace` | Proves the Pi target still builds, including the bundled SQLite C sources ([ADR-0009](adr/0009-rusqlite-for-sqlite-access.md)) |

Run all of them locally before pushing — the commands are identical.

## What CI does not prove

The cross-build job is the one most likely to be misread. It proves the code **compiles**
for `aarch64-unknown-linux-gnu`. It says nothing about:

- Whether a reader connects, stays connected, or reconnects
- Write latency or SD card behavior under sustained journal writes
- Recovery from power loss mid-write
- Clock accuracy or drift
- Memory headroom on 1 GB of RAM

Those are validated on hardware or they are not validated. See
[hardware-support.md](hardware-support.md) and the Milestone 3 and 5 exit criteria in the
[roadmap](roadmap.md).

## Planned additions

Not built yet; listed so the gaps are visible rather than forgotten.

| Addition | Milestone | Purpose |
|---|---|---|
| ~~Crate dependency rule enforcement~~ | ~~M1~~ | **Done.** `crates/splitforge-testkit/tests/dependency_rules.rs` makes `engine` → `llrp` a test failure. [ADR-0012](adr/0012-architecture-rules-enforced-by-tests.md) |
| **Fuzzing the LLRP decoder** | M3 | Highest-risk code in the project: binary parsing of untrusted network input |
| ~~Durability tests in CI~~ | ~~M1~~ | **Done.** `crates/splitforge-cli/tests/restart.rs` kills the process mid-write, reopens, and asserts the journal is contiguous and re-derives identically |
| **Capture-driven reader tests** | M3 | Recorded protocol captures so reader behavior is testable without hardware |
| **Hardware-in-the-loop validation** | M3 | A self-hosted Pi runner with a real reader attached. The only thing that validates the claims above |
| **Release artifacts** | M5 | Signed `aarch64` binaries and a deployment bundle |

## Dependency policy

`Cargo.lock` is committed — this workspace produces binaries, and reproducible field
builds matter more than dependency freshness.

Dependency count on the read path is kept deliberately small. Every crate that runs
between a reader report and a durable write is a crate that can lose a read; additions
there warrant an [ADR](adr/).

### Accepted advisories

**None. That is the target state, not a coincidence.**

An advisory may be muted only in `deny.toml`'s `[advisories] ignore` list, and only with two
things written down: why the vulnerable code is unreachable from this workspace, and the
condition under which the entry gets deleted. An ignore without a deletion condition is a
permanent exception pretending to be a temporary one.

The list is empty because the one entry that ever lived there —
[RUSTSEC-2026-0009](https://rustsec.org/advisories/RUSTSEC-2026-0009), reachable only
through `time`'s RFC 2822 parser, which SplitForge never calls — existed solely because the
MSRV held `time` below the release that fixed it. Raising the MSRV
([ADR-0013](adr/0013-msrv-policy.md)) removed the reason, so the entry went with it.

That is the precedent worth keeping: **an accepted advisory whose only justification is a
version pin is a bug in the pin.** Move the pin.
