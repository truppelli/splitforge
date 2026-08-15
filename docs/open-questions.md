# Open Questions

Decisions that need an owner rather than a guess. Each is referenced from the document
that raised it. Resolving one means writing or updating an [ADR](adr/) and moving the entry
to [Resolved](#resolved) — kept rather than deleted, because the inbound links stay valid
and because a record of what was undecided, and for how long, is worth as much here as it
is in the timing model.

| # | Question | Blocks | Owner |
|---|---|---|---|
| [Q3](#q3-reader-clock-trust-defaults) | Reader clock trust defaults and alarm thresholds | M3 | — |
| [Q4](#q4-code-of-conduct-enforcement-contact) | Code of Conduct enforcement contact | Publicizing repo | — |
| [Q5](#q5-local-api-authentication-model) | Local API authentication model | M2 | — |
| [Q7](#q7-corruption-recovery-strategy) | Database corruption recovery strategy | M5 | — |
| [Q9](#q9-first-reader-model) | Which physical reader model comes first? | **M3 — hard gate** | — |
| [Q10](#q10-gps-pps-time-reference) | Is GPS+PPS required hardware or a recommendation? | M5 | — |
| [Q11](#q11-clock-error-budget-enforcement) | Refuse to publish when clock error exceeds budget? | M5 | — |
| [Q12](#q12-leap-second-handling) | Leap-second policy | M4 | — |
| [Q13](#q13-msrv-policy) | How is the MSRV chosen, and when does it move? | M2 | — |

---

### Q3: Reader clock trust defaults

**Raised in:** [clock-and-time-discipline.md § 5](clock-and-time-discipline.md#5-getting-a-time-reference-without-the-internet)

Specifically:

- What does `auto` trust mode do when measured offset is large? Fall back to device time,
  or trust the reader and alarm?
- What offset threshold triggers an alarm — 100 ms? 1 s?
- What skew threshold — 10 ppm? 50 ppm?
- Should a reader reporting `Uptime` be usable for a race at all without an anchor?

These need real measurements from real hardware, which makes this partly gated on
[Q9](#q9-first-reader-model).

### Q4: Code of Conduct enforcement contact

**Raised in:** [CODE_OF_CONDUCT.md](../CODE_OF_CONDUCT.md)

The Code of Conduct has a `TODO` where the enforcement contact belongs. A project-specific
address (not a personal one) is preferable for a public repository. Must be filled in
before the repo is publicized.

### Q5: Local API authentication model

**Raised in:** [threat-model.md S2](threat-model.md#security-risks)

The API runs on an untrusted event LAN. Options: a bearer token in local config; mTLS
(strong, painful to operate from a phone); Unix socket only, with all remote access over
SSH (simplest and most secure, but rules out a browser console later).

Whatever is chosen must not make the timer unusable at 6 a.m. in the cold — an
authentication scheme that gets bypassed in practice is worse than a simple one that gets
used.

### Q7: Corruption recovery strategy

**Raised in:** [architecture.md § 4](architecture.md#4-failure-behavior),
[threat-model.md O4](threat-model.md#operational-risks)

If the SQLite file is corrupt mid-event, what is the operator supposed to do? Options: fail
over to a fresh database and reconcile later; run in a degraded append-to-flat-file mode;
stop and restore from snapshot.

The right answer probably involves a **write-ahead text journal** — a plain append-only
log file written alongside the database, cheap to write and trivially recoverable, so
"database is corrupt" never means "reads are gone." Needs design.

### Q9: First reader model

**Raised in:** [hardware-support.md](hardware-support.md)

**This is the hard gate on Milestone 3.** Milestone 3 cannot start until a physical
LLRP-capable reader is in hand. Selection criteria: LLRP 1.0.1+ support, configurable NTP
server (see [Q10](#q10-gps-pps-time-reference)), documented timestamp behavior,
availability at a price an unfunded project can absorb, and a form factor suited to
outdoor race use.

Milestones 1 and 2 are deliberately designed to make progress while this is unresolved.

### Q10: GPS PPS time reference

**Raised in:** [clock-and-time-discipline.md § 5](clock-and-time-discipline.md#5-getting-a-time-reference-without-the-internet)

Is a GPS+PPS receiver **required** hardware, or a strong recommendation?

Requiring it guarantees the accuracy budget and enables the Pi-as-LAN-NTP-server design
that removes cross-domain clock error entirely. Recommending it keeps the barrier to entry
low for someone who just wants to try SplitForge on a bench.

Related and needing verification against real hardware: can the chosen reader be pointed
at an arbitrary LAN NTP server, or does it only accept a vendor default?

*Leaning:* required for any event where results are published; optional for development.
Enforced by the pre-race check rather than by refusing to run.

### Q11: Clock error budget enforcement

**Raised in:** [clock-and-time-discipline.md § 10](clock-and-time-discipline.md#10-health-checks-and-alarms)

If measured drift implies accumulated error beyond ±0.1 s, should SplitForge refuse to
publish a `final` revision, or publish with a prominent accuracy caveat recorded in the
revision?

Refusing protects the project's credibility. Publishing with a caveat respects that the
organizer, not the software, owns the decision about whether the result stands.

*Leaning:* record the estimated accuracy in the revision, warn loudly, do not refuse.
A timer that will not produce results has failed at its job.

### Q12: Leap-second handling

**Raised in:** [clock-and-time-discipline.md § 12](clock-and-time-discipline.md#12-open-questions)

A leap second inserted mid-race is a full second — ten times the accuracy budget. Options:
rely on the upstream time source's smearing, detect and record the event, or ignore it as
vanishingly unlikely.

Low probability, non-zero impact, cheap to at least *record*. Deciding to ignore it is
fine; doing so without noticing is not.

### Q13: MSRV policy

**Raised in:** Milestone 1 implementation,
[ADR-0010](adr/0010-time-crate-for-timestamps.md)

The workspace pins `rust-version = "1.85"`. That is no longer a theoretical cost.

`time` >= 0.3.47 requires Rust 1.88, so the workspace holds at `time` 0.3.45 — which
carries **[RUSTSEC-2026-0009](https://rustsec.org/advisories/RUSTSEC-2026-0009)**, a stack
exhaustion in its RFC 2822 parser. SplitForge cannot reach it: every timestamp it parses or
emits is RFC 3339, and `Rfc2822` appears nowhere in the workspace. The advisory is
therefore accepted, with the reasoning and the revisit conditions recorded in `deny.toml`
and `.cargo/audit.toml`.

That is a defensible position and an uncomfortable one. Holding an MSRV is now costing a
known-vulnerable dependency in the tree, for a project whose entire claim is that its
records can be trusted. `AntennaMap::resolve` is also written without a let-chain for the
same reason, which is trivial by comparison but the same shape of cost.

The question is what the policy actually is:

- Is the MSRV a **promise to packagers** (Debian and Raspberry Pi OS ship older toolchains),
  or just the version the author happened to have?
- Does it follow a rule — N latest stable releases, or "whatever Raspberry Pi OS stable
  ships" — or move ad hoc when a dependency forces it?
- Does raising it need an ADR, or is it a routine dependency decision?

Worth answering before Milestone 2 adds more dependencies, because the cost of an MSRV is
paid every time one of them moves — and the next accepted advisory may not be one that
happens to be unreachable.

---

## Resolved

Kept for the record, and so that links from ADRs and older documents still resolve.

| # | Question | Resolved by |
|---|---|---|
| [Q1](#q1-sqlite-crate-rusqlite-vs-sqlx) | `rusqlite` or `sqlx`? | [ADR-0009](adr/0009-rusqlite-for-sqlite-access.md) |
| [Q2](#q2-time-crate-time-vs-chrono) | `time` or `chrono`? | [ADR-0010](adr/0010-time-crate-for-timestamps.md) |
| [Q6](#q6-enforcing-crate-dependency-rules) | Enforcing crate dependency rules | [ADR-0012](adr/0012-architecture-rules-enforced-by-tests.md) |
| [Q8](#q8-enforcing-append-only-in-sqlite) | Enforcing append-only at the database level | [ADR-0011](adr/0011-append-only-enforced-by-triggers.md) |

### Q1: SQLite crate, `rusqlite` vs `sqlx`

**Resolved — [ADR-0009](adr/0009-rusqlite-for-sqlite-access.md): `rusqlite`, `bundled`.**

The read path is one small insert that must return only when it is durable. SQLite writes
are synchronous underneath either crate, and `sqlx`'s compile-time query checking buys
least exactly where the risk is highest. Bundled so the Pi's system SQLite version stops
being a deployment variable.

### Q2: Time crate, `time` vs `chrono`

**Resolved — [ADR-0010](adr/0010-time-crate-for-timestamps.md): `time`, UTC everywhere.**

Smaller surface, and `time` makes reading local time from a threaded process unavailable
rather than merely unwise. All persisted timestamps are UTC; interval arithmetic uses
monotonic values, never the wall clock. The MSRV cost this carries became
[Q13](#q13-msrv-policy).

### Q6: Enforcing crate dependency rules

**Resolved — [ADR-0012](adr/0012-architecture-rules-enforced-by-tests.md): a test.**

`crates/splitforge-testkit/tests/dependency_rules.rs` parses every member manifest and
fails on a violation, with a message that names the crate, the dependency, and the document
that explains the rule. Chosen over `cargo deny`'s `bans.deny` on error messages: this is a
rule people need to understand rather than obey.

### Q8: Enforcing append-only in SQLite

**Resolved — [ADR-0011](adr/0011-append-only-enforced-by-triggers.md): triggers.**

`BEFORE UPDATE` and `BEFORE DELETE` triggers on `raw_reads` that `RAISE(ABORT)`, translated
by the storage layer into `JournalError::AppendOnlyViolation`. A guardrail against the
accident, which is the failure that actually happens — not a defense against someone with
write access to the file, which nothing at this layer could be.
