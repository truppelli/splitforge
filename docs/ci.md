# Continuous Integration

Implemented in [`.github/workflows/ci.yml`](../.github/workflows/ci.yml). Runs on every
pull request and every push to `main`.

## Jobs

| Job | Command | Why it exists |
|---|---|---|
| **Format** | `cargo fmt --all --check` | Removes style from code review entirely |
| **Clippy** | `cargo clippy --workspace --all-targets --all-features -- -D warnings` | Warnings are errors. A warning nobody fixes is a warning nobody reads |
| **Test** | `cargo test --workspace --all-features` | — |
| **MSRV** | `cargo check` on Rust 1.85.0 | The Pi may run an older toolchain than a developer laptop. Better to find out here than the night before an event |
| **Security advisories** | `cargo audit --deny warnings` | Known vulnerabilities in the dependency tree |
| **Licenses and bans** | `cargo deny check` | License compatibility ([ADR-0007](adr/0007-license-selection.md)), duplicate versions, unknown registries |
| **Cross-build** | `cargo build --release --target aarch64-unknown-linux-gnu -p splitforge-edge` | Proves the Pi target still builds |

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
| **Crate dependency rule enforcement** | M1 | Make `engine` ↛ `llrp` a build failure, not a code-review catch. [Q6](open-questions.md#q6-enforcing-crate-dependency-rules) |
| **Fuzzing the LLRP decoder** | M3 | Highest-risk code in the project: binary parsing of untrusted network input |
| **Durability tests in CI** | M1 | Kill the process mid-write, reopen, assert nothing was lost |
| **Capture-driven reader tests** | M3 | Recorded protocol captures so reader behavior is testable without hardware |
| **Hardware-in-the-loop validation** | M3 | A self-hosted Pi runner with a real reader attached. The only thing that validates the claims above |
| **Release artifacts** | M5 | Signed `aarch64` binaries and a deployment bundle |

## Dependency policy

`Cargo.lock` is committed — this workspace produces binaries, and reproducible field
builds matter more than dependency freshness.

Dependency count on the read path is kept deliberately small. Every crate that runs
between a reader report and a durable write is a crate that can lose a read; additions
there warrant an [ADR](adr/).
