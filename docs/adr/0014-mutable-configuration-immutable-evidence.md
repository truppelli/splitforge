# ADR-0014: Configuration is mutable; evidence and the audit trail are not

- **Status:** Accepted
- **Date:** 2026-08-16
- **Supersedes:** —

## Context

Milestone 2 puts event configuration in the database for the first time: events, races,
checkpoints, participants, chip assignments, readers, antenna mappings, and the timing
policy. Until now the only table was `raw_reads`, which [ADR-0005](0005-raw-read-append-only-journal.md)
made append-only and [ADR-0011](0011-append-only-enforced-by-triggers.md) enforced with
triggers.

The obvious question is whether the new tables get the same treatment. The project's
central principle is that records are not rewritten, and applying it uniformly is
appealing.

It is also wrong, and the reason is worth stating precisely rather than assuming.

Evidence is a claim about **what happened**: a reader detected this chip at this instant.
Nothing that happens later can make it untrue, so rewriting it is always a lie.

Configuration is a claim about **what is currently true**: this person is entered under
bib 104, this chip is on their wrist, this antenna covers the finish. All of those change
during a normal race morning, repeatedly and legitimately. Somebody registers late. A chip
fails at the start line and is swapped. An operator widens the dedup window after watching
the first wave come through. A roster arrives with a misspelling that gets corrected.

An append-only configuration model would mean either versioning every config table — a
temporal schema, with every read becoming an as-of query — or making the tool refuse the
things operators actually need to do. The first is a large amount of complexity to buy a
property nothing needs; the second is a tool nobody uses.

## Decision

**Configuration tables are mutable. Evidence and the audit trail are append-only, enforced
by triggers.**

Append-only, with `BEFORE UPDATE` / `BEFORE DELETE` triggers that `RAISE(ABORT)`:

- `raw_reads` — what a reader reported
- `race_sessions` — when an operator recorded the gun (see [ADR-0015](0015-race-start-records-the-gun.md))
- `audit_log` — every operator action that could move a published outcome

Mutable:

- `events`, `races`, `checkpoints`, `participants`, `chip_assignments`, `readers`,
  `reader_antennas`, `timing_policies`

What makes the mutable half safe is not immutability but two other things, both of which
already existed in the design and are now load-bearing:

1. **`audit_log` records every change.** Actor, timestamp, action, subject, and structured
   before/after detail. `docs/timing-model.md` § 8 sets the bar — "given a finished event
   and a disputed result, can you reconstruct from the database alone why that participant
   received that time, and what changed" — and the trail is what answers it.

2. **A result revision snapshots the policy it used**, rather than pointing at a row that
   can later be edited (`docs/timing-model.md` § 7). So configuration answers *what is true
   now*, the append-only tables answer *what was true then*, and published results only
   ever ask the second question.

Two consequences follow, and both are implemented rather than left as intentions:

- **Re-import preserves identity.** Importing a roster twice matches on `(race, bib)` and
  updates in place. If it minted fresh participant ids, every chip assignment would dangle
  and every already-derived timing event would point at somebody who no longer exists.
  `docs/architecture.md` § 4 already required this ("roster import is versioned; re-import
  produces a new derivation, not a destroyed journal"); it is now a test.

- **The timing policy is stored as JSON**, not shredded into columns. It has to round-trip
  verbatim into a revision snapshot, and a column layout would need migrating in lockstep
  with `SelectionRule` — making old revisions unreadable the first time a rule gains a
  field.

## Consequences

**Good.** Operators can do what race mornings require without fighting the tool. The
schema stays small: no temporal tables, no as-of queries, no version columns on nine
tables. The distinction between the two kinds of record stays visible in the code, because
the type that can `UPDATE` (`ConfigStore`) has no way to reach `raw_reads`, and the type
that holds evidence (`SqliteJournal`) has no `UPDATE` path at all.

**Bad.** "What did the roster look like at 09:15?" is answerable only by replaying
`audit_log`, not by querying a table. That is a real loss, accepted because the question
that actually gets asked is "why did *this* result change?", which the trail answers
directly.

**The risk this creates.** The audit trail is now the only record of configuration history,
which makes an un-audited mutation a silent hole in the story. Every mutating path in
`splitforge-cli::configure` writes an audit row before returning, and
`crates/splitforge-cli/tests/console.rs` asserts that each configuration command leaves
one. A new mutation that skips the trail is a bug of the same severity as losing a read.

## Alternatives considered

**Append-only configuration with temporal queries.** Every config table gains
`valid_from` / `valid_until`, and every read becomes as-of. Genuinely more faithful to the
project's principle. Rejected on cost: it is a large amount of schema and query complexity
purchased for a question nobody has asked, at the exact moment the priority is getting an
operator interface working at all. `chip_assignments` is already time-bounded because
resolution genuinely depends on read time — that is the one place the cost is justified,
and it was paid.

**Mutable configuration with no audit trail.** Simpler, and indefensible. It makes
`docs/timing-model.md` § 8's test unanswerable, which is the whole reason the audit table
exists.

**Copy-on-write configuration versions.** A new row set per change, with results pointing
at a version. Close to what result revisions already do for policy, but applied to
everything. Rejected as premature: revisions are Milestone 4, and building the versioning
before the thing that consumes it would mean guessing at its shape.
