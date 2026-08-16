# Architecture Decision Records

An ADR records a decision that was expensive to make and would be expensive to reverse —
along with the context that made it the right call. When someone asks "why is it like
this?" two years from now, the ADR is the answer.

## Index

| # | Decision | Status |
|---|---|---|
| [0001](0001-rust-workspace.md) | Multi-crate Rust workspace | Accepted |
| [0002](0002-raspberry-pi-target.md) | Raspberry Pi 3, 64-bit Linux, `aarch64-unknown-linux-gnu` | Accepted |
| [0003](0003-sqlite-wal-local-persistence.md) | SQLite in WAL mode for local persistence | Accepted |
| [0004](0004-llrp-first-reader-adapter.md) | LLRP as the first reader protocol | Accepted |
| [0005](0005-raw-read-append-only-journal.md) | Raw reads are an append-only journal | Accepted |
| [0006](0006-optional-outbound-integrations.md) | Outbound integrations are always optional | Accepted |
| [0007](0007-license-selection.md) | GPL-3.0-or-later | Accepted |
| [0008](0008-offline-first-operation.md) | Offline-first: no cloud service on any race-day path | Accepted |
| [0009](0009-rusqlite-for-sqlite-access.md) | `rusqlite` (bundled) for SQLite access | Accepted |
| [0010](0010-time-crate-for-timestamps.md) | The `time` crate for all timestamps | Accepted |
| [0011](0011-append-only-enforced-by-triggers.md) | Append-only enforced by database triggers | Accepted |
| [0012](0012-architecture-rules-enforced-by-tests.md) | Crate dependency rules enforced by a test | Accepted |
| [0013](0013-msrv-policy.md) | The MSRV is a tested floor, not a compatibility promise | Accepted |
| [0014](0014-mutable-configuration-immutable-evidence.md) | Configuration is mutable; evidence and the audit trail are not | Accepted |
| [0015](0015-race-start-records-the-gun.md) | `race start` records the gun; it does not gate ingestion | Accepted |

## Process

1. Copy [`template.md`](template.md) to `NNNN-short-kebab-title.md`, numbering sequentially.
2. Open it as its own pull request when the decision deserves discussion separate from the
   code that implements it.
3. Status starts as `Proposed`, becomes `Accepted` on merge.
4. ADRs are **not edited after acceptance** except to change status. A decision that no
   longer holds gets a new ADR that supersedes it, and the old one is marked
   `Superseded by ADR-NNNN`.

That last rule is the same principle the timing model applies to race data: the record of
what was decided, and why, is more valuable than a tidy document that pretends the
decision was always obvious.

## When an ADR is warranted

- Crossing or changing a crate boundary
- Changing the shape of stored data
- Adding a dependency that would be painful to remove
- Anything touching the read path
- Anything that changes what SplitForge promises about durability or accuracy

Routine implementation choices do not need one. If you are unsure, the fact that you are
unsure is usually a sign that it does.
