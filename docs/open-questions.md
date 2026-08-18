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
| [Q9](#q9-first-reader-model) | Which physical reader model comes first? | **M3 — hard gate** | — |
| [Q10](#q10-gps-pps-time-reference) | Is GPS+PPS required hardware or a recommendation? | M5 | — |
| [Q11](#q11-clock-error-budget-enforcement) | Refuse to publish when clock error exceeds budget? | M5 | — |
| [Q12](#q12-leap-second-handling) | Leap-second policy | M5 | — |

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

**Re-scoped after Milestone 4**, which this was listed as blocking. It does not, and the
reason is worth writing down rather than asserting.

Every time M4 publishes is a *difference between two stored instants*, both of them
microseconds since the Unix epoch, both taken from the same clock domain. Unix time has no
leap seconds by construction, so the arithmetic is unaffected by the choice made here. What
the choice affects is whether those two instants were *correct*, which is a clock-discipline
question and belongs with the rest of them in M5.

It is not negligible, though, and the numbers are worth having:

| Elapsed time measured across a smear window | Error contributed |
|---|---|
| 20 min (5K) | ~14 ms |
| 1 h | ~42 ms |
| 2 h | ~83 ms |
| 4 h (marathon) | ~167 ms |
| 12 h (ultra) | ~500 ms |

A 24-hour smear changes the clock's rate by ~11.6 ppm, which exhausts the ±0.1 s budget at
**2.4 hours of elapsed time**. Short races are comfortably inside it; marathons and ultras
are not. So "rely on smearing" is a defensible answer for a 5K and an undefensible one for a
100-miler, and the policy has to say which races it is claiming accuracy for.

Nothing was deferred to dodge this. M4 computes the difference it is given; making the
inputs trustworthy is [Q10](#q10-gps-pps-time-reference)'s and
[Q11](#q11-clock-error-budget-enforcement)'s territory, and this belongs beside them.

---

## Resolved

Kept for the record, and so that links from ADRs and older documents still resolve.

| # | Question | Resolved by |
|---|---|---|
| [Q1](#q1-sqlite-crate-rusqlite-vs-sqlx) | `rusqlite` or `sqlx`? | [ADR-0009](adr/0009-rusqlite-for-sqlite-access.md) |
| [Q2](#q2-time-crate-time-vs-chrono) | `time` or `chrono`? | [ADR-0010](adr/0010-time-crate-for-timestamps.md) |
| [Q6](#q6-enforcing-crate-dependency-rules) | Enforcing crate dependency rules | [ADR-0012](adr/0012-architecture-rules-enforced-by-tests.md) |
| [Q8](#q8-enforcing-append-only-in-sqlite) | Enforcing append-only at the database level | [ADR-0011](adr/0011-append-only-enforced-by-triggers.md) |
| [Q13](#q13-msrv-policy) | How is the MSRV chosen, and when does it move? | [ADR-0013](adr/0013-msrv-policy.md) |
| [Q7](#q7-corruption-recovery-strategy) | Database corruption recovery strategy | [ADR-0018](adr/0018-write-ahead-sidecar-journal.md) |
| [Q5](#q5-local-api-authentication-model) | Local API authentication model | [ADR-0021](adr/0021-local-api-listens-on-a-unix-socket.md) |

### Q5: Local API authentication model

**Resolved — [ADR-0021](adr/0021-local-api-listens-on-a-unix-socket.md): a Unix socket, and no authentication of its own.**

**Raised in:** [threat-model.md S2](threat-model.md#security-risks)

The API runs on an untrusted event LAN. Options were: a bearer token in local config; mTLS
(strong, painful to operate from a phone); Unix socket only, with all remote access over
SSH (simplest and most secure, but rules out a browser console later).

Whatever was chosen could not make the timer unusable at 6 a.m. in the cold — an
authentication scheme that gets bypassed in practice is worse than a simple one that gets
used.

**Re-scoped after Milestone 2.** This was listed as blocking M2, which turned out to be
wrong: M2 is a CLI running as a local process over SSH, and it opens no socket. Nothing was
deferred to dodge the question — there was simply nothing to authenticate.

**The answer turned out to be that the question had a wrong premise.** All three options
assumed the API would be reachable and asked how to guard it. A Unix socket makes it
unreachable, so [S2](threat-model.md#security-risks) stops being a risk to mitigate and
becomes one that does not apply — no token to leak, no certificate to expire on race
morning. The cost is a browser console, which now needs an ADR superseding this one.

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

### Q13: MSRV policy

**Resolved — [ADR-0013](adr/0013-msrv-policy.md): a tested floor, now 1.88.**

The MSRV was protecting nobody. Every crate here is `publish = false`, no distribution
packages SplitForge, and [ADR-0002](adr/0002-raspberry-pi-target.md) means the Pi never
compiles anything — it receives cross-compiled binaries. Meanwhile holding 1.85 kept `time`
below the release that fixed
[RUSTSEC-2026-0009](https://rustsec.org/advisories/RUSTSEC-2026-0009), so the project was
carrying a known-vulnerable dependency to defend a version number nobody had chosen.

The MSRV is now the oldest toolchain CI tests against, and it moves when a security advisory
cannot be cleared without moving it. `deny.toml`'s ignore list is empty again.

### Q7: Corruption recovery strategy

**Resolved — [ADR-0018](adr/0018-write-ahead-sidecar-journal.md): a write-ahead text sidecar.**

The instinct recorded here while the question was open turned out to be right, and the
design that closed it is the one this entry guessed at: every raw read is appended to a
plain-text file beside the database and fsynced **before** the database transaction opens,
which makes the sidecar a superset of the journal at every instant.

What the entry did not anticipate was the division of labour. The sidecar carries reads
only. Configuration comes back from the pre-race snapshot, because the two halves of the
problem have opposite shapes — the configuration barely changes and is small, the evidence
changes every second and is the part nobody can retype. So `backup restore` brings back the
race and `splitforge recover` brings back the reads, and a restore deliberately never
touches the sidecar: it holds exactly the reads the snapshot is missing.

Of the three options originally listed, "stop and restore from snapshot" survives as half
the answer, "fail over to a fresh database and reconcile later" was rejected as a
reconciliation nobody would have rehearsed, and "degraded append-to-flat-file mode" became
the normal mode rather than the degraded one — which is the part worth remembering. A
fallback that only runs when things are already wrong is a fallback nobody has tested.
