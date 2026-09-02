# Open Questions

Decisions that need an owner rather than a guess. Each is referenced from the document
that raised it. Resolving one means writing or updating an [ADR](adr/) and moving the entry
to [Resolved](#resolved) — kept rather than deleted, because the inbound links stay valid
and because a record of what was undecided, and for how long, is worth as much here as it
is in the timing model.

| # | Question | Blocks | Owner |
|---|---|---|---|
| [Q3](#q3-reader-clock-trust-defaults) | Reader clock trust defaults and alarm thresholds | M3b | — |
| [Q4](#q4-code-of-conduct-enforcement-contact) | Code of Conduct enforcement contact | Publicizing repo | — |
| [Q9b](#q9b-first-llrp-reader-model) | Which networked LLRP reader comes first? | **M3b — hard gate** | — |
| [Q10](#q10-gps-pps-time-reference) | Is GPS+PPS required hardware or a recommendation? | M5 | — |
| [Q11](#q11-clock-error-budget-enforcement) | Refuse to publish when clock error exceeds budget? | M5 | — |
| [Q12](#q12-leap-second-handling) | Leap-second policy | M5 | — |
| [Q14](#q14-reader-silence-threshold) | How long may a streaming reader be silent before it is presumed gone? | M3a | — |

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
[Q9b](#q9b-first-llrp-reader-model). It is **not** answerable by
[Q9a](#q9a-first-serial-module)'s serial module, which has no reader clock at all — there
is no offset to measure and no skew to threshold, which is one of the two criteria
[ADR-0024](adr/0024-serial-reader-adapter-before-llrp.md) records that M3a cannot close.

### Q4: Code of Conduct enforcement contact

**Raised in:** [CODE_OF_CONDUCT.md](../CODE_OF_CONDUCT.md)

The Code of Conduct has a `TODO` where the enforcement contact belongs. A project-specific
address (not a personal one) is preferable for a public repository. Must be filled in
before the repo is publicized.

### Q9: First reader model

**Split into [Q9a](#q9a-first-serial-module) and [Q9b](#q9b-first-llrp-reader-model)** by
[ADR-0024](adr/0024-serial-reader-adapter-before-llrp.md). Kept here so the inbound links
from [hardware-support.md](hardware-support.md), [hardware-plan.md](hardware-plan.md), and
older ADRs still resolve.

The question was raised as one and turned out to be two. It asked "which physical reader
comes first," and assumed the answer to that was also the answer to "which reader closes
the support checklist" — because when it was written the only candidates were networked LLRP
readers, for which those are the same question.

They are not the same question for a serial module, which can be bought this week and closes
six of the nine criteria. So Q9a asks which module comes first and is closed; Q9b asks which
LLRP reader comes first, is exactly as open as Q9 was, and keeps gating M3b — and therefore
M5.

**What did not happen:** Q9 was not answered by lowering what counts as an answer. M3b's
exit criteria are M3's, verbatim.

### Q9b: First LLRP reader model

**Raised in:** [hardware-support.md](hardware-support.md). Formerly the second half of
[Q9](#q9-first-reader-model).

**This is the hard gate on Milestone 3b**, and through it on Milestone 5. M3b cannot start
until a physical LLRP-capable reader is in hand. Selection criteria: LLRP 1.0.1+ support,
configurable NTP server (see [Q10](#q10-gps-pps-time-reference)), documented timestamp
behavior, availability at a price an unfunded project can absorb, and a form factor suited to
outdoor race use.

Milestones 1, 2, 4, and the hardware-free half of 5 were deliberately built to make progress
while this is unresolved. [M3a](roadmap.md#milestone-3a--one-serial-reader) is the same
strategy applied once more — and it is the last time it works, because M5's exit criterion
names unplugging Ethernet and a serial module has none.

[hardware-plan.md § 4](hardware-plan.md#4-phase-1--field-unit-and-finding-the-real-bom-2000)
holds ~$350 to buy a used FCC-band Impinj R220/R420 or Zebra FX7500 opportunistically, as a
**test instrument** rather than as product hardware. That would close this question. Do not
block on it — blocking on it is what cost three milestones.

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

### Q14: Reader silence threshold

**Raised in:** [ADR-0025](adr/0025-m3a-proves-durability-above-the-transport.md), which makes
detecting a disconnection a deliverable of M3a and then declines to choose the number that
detection turns on.

A streaming ThingMagic module has no liveness signal. User Guide § 5.1.4.1 says *"flow control
is not supported"* and § 8.8.2 says the module cannot *"detect a broken communications interface
connection and stop streaming the tag results"* — so **a stream that has gone quiet is
indistinguishable from a checkpoint with nobody crossing it.** The adapter has to presume the
reader gone after some interval of silence, and the interval is a guess:

- Too short, and the tail end of a 10K manufactures gaps in evidence that is perfectly intact.
- Too long, and a module that died at the gun goes unnoticed for that long.

**The prerequisite is not a measurement.** It is whether the module emits *anything* during a
continuous read with no tags in the field — a keepalive, a periodic status frame, an empty tag
report. The user guide does not say, in either direction. If the module does speak into a quiet
field, the threshold can be a small multiple of that period and the ambiguity mostly disappears;
if it does not, the threshold is a race policy rather than a protocol constant, and probably
belongs per checkpoint — a finish line goes quiet differently from a start.

**Half-answered by reading the SDK, 2026-08-31.** There is a mechanism: two search flags the
user guide never mentions, `TMR_SR_SEARCH_FLAG_STATUS_REPORT_STREAMING` (32) and
`TMR_SR_SEARCH_FLAG_STATS_REPORT_STREAMING` (256), and a branch in MercuryAPI's continuous-read
receive path for *"a status stream response"* — a non-tag frame that arrives mid-stream. So the
answer to *"can anything arrive but tags?"* is **yes**.

That is not yet the answer to this question, which needs **periodicity**: on what interval such
a frame arrives, and whether it arrives when the field is empty. The `TMR_SR_STATUS_*` content
flags that would say are in a header the archived mirror does not carry a current copy of —
see [finding 9](readers/vendor-documents.md#9-the-command-set-is-spread-across-three-files-and-one-was-archived)
and [finding 12](readers/vendor-documents.md#12-a-liveness-signal-may-exist-after-all-and-adr-0025-assumed-it-did-not).
Two routes to the rest: a current `tm_reader.h` from a vendor SDK distribution, or a module on
a bench with nothing in front of it and a terminal capturing the port for a minute.

**What is already decided** and not in scope here: that a silence-derived gap is recorded as
*suspected* rather than confirmed, and that erring toward a false gap is the safe direction.
This question is only the number.

*Leaning:* configurable per checkpoint, with a conservative default and no accuracy claim
attached to it, until a module and the SDK together say which of the two situations this is.

**Half of that leaning is now built, and the question is not answered by it.** The threshold is
`reader_silence_ms` in `device_settings`, set by `splitforge device set --reader-silence-ms` and
reported by `device show`; it defaults to `DEFAULT_SILENCE_THRESHOLD_MS`, which is two minutes
chosen to be wrong in the safe direction and carries no measurement. **Zero disables the check**,
because an operator who has decided a false gap is worse than a late one needs to say so
explicitly rather than by setting a week and hoping.

It is **per device, not per checkpoint**. The leaning above still stands and this is not a
decision against it: one reader is composed per service today, so a per-checkpoint setting would
be a schema with exactly one row in it and no way to tell whether it was right. What the number
should be is unchanged and still open.

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
| [Q9a](#q9a-first-serial-module) | Which serial module is the first physical adapter? | [ADR-0024](adr/0024-serial-reader-adapter-before-llrp.md) |

### Q9a: First serial module

**Resolved — [ADR-0024](adr/0024-serial-reader-adapter-before-llrp.md): the ThingMagic
M7e-Pico, as the first *physical* adapter while LLRP stays the first *networked* protocol.**

**Raised in:** [hardware-plan.md](hardware-plan.md). Formerly the first half of
[Q9](#q9-first-reader-model).

Chosen because it is a **current** part — FCC modular grant, US distributor, documented
serial protocol — where every LLRP reader in the project's price range is out of production
and priced by whatever a liquidator lists this month. You cannot build a repeatable bill of
materials around a scavenged component.

**Closing this closes less than it appears to**, which is the part worth keeping. The module
cannot close two of the nine support criteria — there is no reader clock to measure offset
and skew against, and there is one RF port, so no per-antenna identity. Neither is "not yet";
both are structural. So the module enters
[`docs/readers/thingmagic-m7e-pico.md`](readers/thingmagic-m7e-pico.md) as *experimental —
under evaluation*, the support matrix stays empty, and
[Q3](#q3-reader-clock-trust-defaults) stays open because this module cannot supply the
measurements it asks for.

What it does buy is the four Pi-side durability measurements M5 could not take — whether an
SD card honors `fsync`, what the second sync costs on real flash, what a day's journal
weighs, what a write in flight does when power goes — none of which ever needed LLRP, only a
real stream of real reads.

**Nothing has been ordered.** The decision is made; the purchase is not, and
[hardware-plan.md § 3](hardware-plan.md#3-phase-0--bench-validation-500-now) lists four
questions to answer before it, each of which can turn a $345 order into a box that cannot be
used on arrival.

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
