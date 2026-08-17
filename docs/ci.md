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

## Running the gates locally

Six of the seven need nothing but a Rust toolchain. The **cross-build does not**: `rusqlite`
is built with the `bundled` feature ([ADR-0009](adr/0009-rusqlite-for-sqlite-access.md)), so
SQLite is compiled from C source for `aarch64`, which needs `gcc-aarch64-linux-gnu` and the
matching target libc. On Debian or Ubuntu:

```bash
rustup target add aarch64-unknown-linux-gnu
sudo apt-get install -y gcc-aarch64-linux-gnu   # pulls libc6-dev-arm64-cross
```

Everywhere else — Windows, macOS — that toolchain is not installable, which would leave the
gate most likely to catch a newly added dependency unrunnable by the developer who added it.
So it ships as an image:

```bash
docker build -t splitforge-ci -f docker/Dockerfile .

# every gate, in the same order, with the same commands
docker run --rm -v "$PWD:/repo" -v splitforge-target:/build splitforge-ci

# or just one
docker run --rm -v "$PWD:/repo" -v splitforge-target:/build splitforge-ci cross
```

Gates: `fmt`, `clippy`, `test`, `msrv`, `audit`, `deny`, `cross`. The build directory is a
named volume rather than the repo's `target/`, so a Linux build never fights the host's
artifacts over the same directory.

[`docker/ci.sh`](../docker/ci.sh) is a transcription of the workflow, not a paraphrase of
it. If a command there stops matching [`ci.yml`](../.github/workflows/ci.yml), the script is
the thing that is wrong — a local run that is "basically the same" is a local run that
passes while CI fails.

## What CI does not prove

The cross-build job is the one most likely to be misread. It proves the code **compiles**
for `aarch64-unknown-linux-gnu`. It says nothing about:

- Whether a reader connects, stays connected, or reconnects
- Write latency or SD card behavior under sustained journal writes, including what the
  second fsync per reader report ([ADR-0018](adr/0018-write-ahead-sidecar-journal.md))
  actually costs on real flash
- Recovery from power loss mid-write. The recovery tests destroy files deliberately; they
  do not reproduce a card losing its cache during a brownout
- Whether an SD card honors `fsync` at all
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
| ~~Operator interface tested through the binary~~ | ~~M2~~ | **Done.** `crates/splitforge-cli/tests/console.rs` configures a race entirely through subcommands and CSV imports, then times, derives, exports, and backs it up — reaching for no library type the operator does not have |
| ~~Scoring tested against hand-computed answers~~ | ~~M4~~ | **Done.** `crates/splitforge-results` is a pure function, so its tests state the expected placement directly rather than comparing against the implementation's own output. `crates/splitforge-cli/tests/results.rs` then publishes, disqualifies, republishes, and asserts the first revision is byte-identical afterwards |
| ~~Recovery rehearsed rather than discovered~~ | ~~M5~~ | **Done.** `crates/splitforge-cli/tests/recovery.rs` deletes the database, overwrites it with garbage, and throws away the snapshot entirely — then asserts the event derives identically afterwards. [ADR-0018](adr/0018-write-ahead-sidecar-journal.md) |
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
