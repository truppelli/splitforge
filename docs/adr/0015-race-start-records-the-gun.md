# ADR-0015: `race start` records the gun; it does not gate ingestion

- **Status:** Accepted
- **Date:** 2026-08-16
- **Supersedes:** —

## Context

Milestone 2's command list includes `splitforge race start` and `splitforge race stop`.
The roadmap does not say what they do, and there are two readings, which lead to very
different systems.

**Reading one — a switch.** Starting the race opens ingestion; stopping it closes
ingestion. Reads arriving outside the window are rejected, or flagged, or dropped. This is
how a stopwatch works, and it is what most people picture when they read `race start`.

**Reading two — a record.** Starting the race writes down the instant the gun went off.
Reads are journalled regardless, before, during, and after. The recorded instant is an
input to scoring, not a filter on evidence.

The first reading is superficially attractive: it makes the state of the system match the
state of the race, and it stops a stray read from a test tag at 06:00 landing in the same
table as the real ones.

It also directly contradicts the sentence the architecture is built around
(`docs/architecture.md` § 1):

> **There is no arrow into the read path from outside the dashed box.** A read is recorded
> because a reader sent it and the local disk accepted it. Nothing else participates.

Under reading one, an operator's memory participates. A timer who is watching the field
come to the line, or fixing an antenna, or talking to a race director, and does not type
`race start` before the gun, loses evidence — permanently, silently, and in a way that is
discovered when somebody asks why they have no time.

## Decision

**`race start` and `race stop` append a row to `race_sessions` and do nothing else.**

- The row records the instant, the actor, an optional note, and when it was written. `--at`
  accepts an RFC 3339 instant so a start can be recorded late and still be correct; it
  defaults to now.
- `race_sessions` is append-only, enforced by triggers, on the same reasoning
  `docs/timing-model.md` § 6 gives manual entries: an operator typed it, which makes it a
  primary record and not a derived one.
- **Ingestion is unaffected.** No code path consults race state before appending to the
  journal, and there is a test that says so.
- The gun time in force is the **most recent** recorded start, falling back to the race's
  scheduled start when nothing has been recorded. A restarted race is a real thing — a
  false start, a delayed wave, a mat that was not live — and the superseded start stays on
  record rather than being overwritten.

`RaceConfig::gun_time()` and `RaceConfig::is_running()` are what the rest of the system
reads. Milestone 4's gun-time scoring consumes the former.

## Consequences

**Good.** The evidence principle survives contact with the operator interface, which is
exactly where principles like it usually die. An operator who forgets to type `race start`
has a fixable problem — record it afterwards with `--at` — rather than a missing one.
Reruns, false starts, and recalled fields are all representable without deleting anything.

**Bad.** The journal accumulates reads that belong to no race: a test tag waved at the mat
during setup, a marshal's chip in a pocket. They are real detections and they stay. In
Milestone 1 this was already true and unremarkable; the difference now is that there is a
race with a start time, so the temptation to filter is stronger. Derivation handles them
the same way it handles anything else with no interpretation — they resolve to nobody and
are reported as unassigned crossings, visible rather than hidden.

**The thing to watch.** "Reads are always ingested" must not quietly acquire an exception.
The most likely place is a future pre-race check or a "clear test reads" convenience.
Neither may delete from `raw_reads`; the triggers make that a database error rather than a
code review question, which is the point of [ADR-0011](0011-append-only-enforced-by-triggers.md).

## Alternatives considered

**Gate ingestion on race state.** Rejected above. Worth noting the specific failure it
produces: it is not that reads are rejected loudly, it is that the system looks like it is
working — the reader is connected, the process is running, the mat lights up — and the
journal is empty. That is the worst shape a failure can take at a race.

**Record the start *and* flag reads outside the window at ingest.** Keeps the evidence but
stamps each read with whether it fell inside a race. Rejected because the flag would be
computed from configuration that can change afterwards, baking a mutable judgement into an
immutable record — the exact confusion [ADR-0014](0014-mutable-configuration-immutable-evidence.md)
exists to prevent. The same question is answerable at derivation time, from data that is
current, at no cost.

**Make `race start` mutable, so a mistyped gun time can be corrected in place.** Rejected
for the reason revisions are never edited (`docs/timing-model.md` § 7): a published gun
time that later differs, with no record of the change, is worse than one that was
superseded in the open. Recording a second start supersedes the first and leaves both
visible.
