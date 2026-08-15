# Open Questions

Decisions that need an owner rather than a guess. Each is referenced from the document
that raised it. Resolving one means writing or updating an [ADR](adr/) and deleting the
entry here.

| # | Question | Blocks | Owner |
|---|---|---|---|
| [Q1](#q1-sqlite-crate-rusqlite-vs-sqlx) | `rusqlite` or `sqlx`? | M1 | — |
| [Q2](#q2-time-crate-time-vs-chrono) | `time` or `chrono`? | M1 | — |
| [Q3](#q3-reader-clock-trust-defaults) | Reader clock trust defaults and alarm thresholds | M3 | — |
| [Q4](#q4-code-of-conduct-enforcement-contact) | Code of Conduct enforcement contact | Publicizing repo | — |
| [Q5](#q5-local-api-authentication-model) | Local API authentication model | M2 | — |
| [Q6](#q6-enforcing-crate-dependency-rules) | Enforcing crate dependency rules in CI | M1 | — |
| [Q7](#q7-corruption-recovery-strategy) | Database corruption recovery strategy | M5 | — |
| [Q8](#q8-enforcing-append-only-in-sqlite) | Enforcing append-only at the database level | M1 | — |
| [Q9](#q9-first-reader-model) | Which physical reader model comes first? | **M3 — hard gate** | — |
| [Q10](#q10-gps-pps-time-reference) | Is GPS+PPS required hardware or a recommendation? | M5 | — |
| [Q11](#q11-clock-error-budget-enforcement) | Refuse to publish when clock error exceeds budget? | M5 | — |
| [Q12](#q12-leap-second-handling) | Leap-second policy | M4 | — |

---

### Q1: SQLite crate, `rusqlite` vs `sqlx`

**Raised in:** [architecture.md § 7](architecture.md#7-technology-choices)

`rusqlite` is a thin, synchronous binding with predictable behavior and no query-time
magic — appealing when the top priority is knowing exactly what hits the disk on the read
path. `sqlx` gives compile-time-checked queries and async, which fits the Tokio-based
service but adds indirection between the code and the write.

The read path is a small number of hot, simple inserts. Compile-time query checking is
more valuable in the reporting and results code.

*Leaning:* `rusqlite` on a dedicated blocking thread for the journal, since SQLite writes
are synchronous regardless and pretending otherwise adds risk. Needs a decision before
Milestone 1.

### Q2: Time crate, `time` vs `chrono`

**Raised in:** [architecture.md § 7](architecture.md#7-technology-choices)

Both are viable. `time` has a smaller surface and a better security history; `chrono` has
wider ecosystem familiarity. The sketches in these documents use `DateTime<Utc>` for
readability, which should not be read as a decision.

Whichever is chosen, all persisted timestamps are UTC, and monotonic intervals use
`std::time::Instant` regardless.

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

### Q6: Enforcing crate dependency rules

**Raised in:** [architecture.md § 2](architecture.md#dependency-rules)

The dependency table is currently documentation. The rule that matters most —
`engine` must never depend on `llrp` — will be violated eventually by a well-meaning PR
unless CI catches it. Options: `cargo deny`'s `bans.deny` with `wrappers`, a custom test
that parses `cargo metadata`, or a lint script. Cheap to add, and worth adding before the
crates have any content.

### Q7: Corruption recovery strategy

**Raised in:** [architecture.md § 4](architecture.md#4-failure-behavior),
[threat-model.md O4](threat-model.md#operational-risks)

If the SQLite file is corrupt mid-event, what is the operator supposed to do? Options: fail
over to a fresh database and reconcile later; run in a degraded append-to-flat-file mode;
stop and restore from snapshot.

The right answer probably involves a **write-ahead text journal** — a plain append-only
log file written alongside the database, cheap to write and trivially recoverable, so
"database is corrupt" never means "reads are gone." Needs design.

### Q8: Enforcing append-only in SQLite

**Raised in:** [timing-model.md § 4](timing-model.md#4-raw-read)

Convention alone will not hold. SQLite triggers can raise on `UPDATE`/`DELETE` against
`raw_reads`, which makes the invariant real rather than aspirational. Cost: triggers can
be dropped by anyone with database access, and complicate legitimate schema migrations.

*Leaning:* add the triggers. They stop accidents, which is the realistic threat, and the
migration path can drop and recreate them explicitly and audibly.

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
