# ADR-0023: A manual entry is an input to derivation, not an override of its output

- **Status:** Accepted
- **Date:** 2026-08-24
- **Supersedes:** —

## Context

Chips fail. A tag comes off in a jacket pocket, a mat drops a read, a runner finishes down
the side of the funnel. When that happens the person at the finish line does the only thing
available: they write the bib down. `docs/timing-model.md` § 6 has always said what that
record is —

> Manual entries are **evidence too** — append-only, recording who entered it, when, why,
> and what they claimed.

— but nothing implemented it, and the `five-k` fixture shows the cost. Bib 109 starts, the
chip stops reporting on course, and the runner is scored `dnf`. That is a correct reading of
the evidence and the wrong answer about the race.

There are two plausible ways to let an operator fix it.

**Reading one — write the time into the result.** `splitforge results set --bib 109
--finish 08:26:41`, landing in the results table next to the placement. It is the shortest
path from the problem to a corrected scoreboard, and it is what most timing software does.

**Reading two — record the claim as evidence, and derive again.** The operator states what
they saw; derivation takes it as an input alongside the raw reads and recomputes everything
downstream.

Reading one breaks the property the whole system is built on.
[ADR-0014](0014-mutable-configuration-immutable-evidence.md) draws the line between mutable
configuration and immutable evidence, and results sit on neither side: they are **derived**,
and the guarantee is that re-deriving reproduces them. A finish time typed directly into a
result is a value with no evidence behind it. Re-derive and it vanishes. Restore from a
snapshot taken an hour earlier and it vanishes. Publish a new revision and it has to be
retyped, correctly, from memory. The one operation the architecture promises is always safe
— *throw away the derived data and compute it again* — silently destroys data under reading
one.

It also fails the audit test `docs/timing-model.md` § 8 sets:

> Given a finished event and a disputed result, can you reconstruct — from the database
> alone, without logs, without asking anyone — *why* that participant received that time?

"Because somebody typed it into the results table" is not an answer that survives being
asked twice.

## Decision

**A manual entry is a second kind of evidence, and derivation takes both kinds as input.**

- `manual_entries` records `(id, seq, race, participant, checkpoint, at, actor, reason,
  recorded_at)`. `BEFORE UPDATE` and `BEFORE DELETE` triggers abort, per
  [ADR-0011](0011-append-only-enforced-by-triggers.md). It sits beside `raw_reads`, not
  inside the results.
- **`TimingEvent` gains its second origin.** It always had exactly one — an accepted read —
  and the type has said "manual entries arrive later" since Milestone 1. `TimingEventOrigin`
  is now `AcceptedRead | Manual`, and the enum carries the entry's identifier rather than
  copying its contents, so the reason and the person who typed it are one lookup away from
  any result that depends on them.
- **`at` and `recorded_at` are separate columns.** `at` is what the operator claims
  happened; `recorded_at` is when the row was written. These differ by however long it took
  to walk back from the checkpoint, and a dispute needs both. `--at` is required, with
  deliberately no default of "now" — a default would quietly record the second as the first.
- **`reason` is required at the parser**, the same rule
  [ADR-0016](0016-status-declarations-are-evidence.md) applies to a status declaration. An
  entry with no stated grounds cannot be defended six months later.
- **The identifier is a random v4 UUID, not a derived one.** Every other derived record in
  the system computes its identifier from its contents so that re-derivation is
  bit-identical. An entry is not derived: it is an *act*. Two officials who both write down
  bib 104 at 08:17:32 have produced two records of one event, and a content-derived
  identifier would silently collapse them into one — discarding the corroboration that makes
  a disputed time defensible.
- **Neither kind of evidence suppresses the other.** A manual entry never rejects a read and
  a read never rejects an entry. Both produce timing events; the scoring rules decide which
  one counts. Laps are counted over the two merged into one chronological sequence, so a
  runner whose chip failed on lap 2 is still on lap 3 for the read that follows.

## Consequences

**Good.** The correction survives everything. Re-derive, restore from a snapshot that
predates the entry, publish a tenth revision — the entry is still there, because it is an
input rather than an output. The `five-k` case above goes from `dnf` to a placed finish, and
the chip time is computed from the runner's own start crossing, which the chip *did* record:
one result assembled from both kinds of evidence.

**Good.** A revision published before the entry was recorded stays exactly as it was. It was
true about what was known at the time, somebody may have acted on it, and the correction
lives in the next revision with a digest that differs. That is the same shape as the DQ
workflow, which is not a coincidence — it is the same decision applied to a different input.

**Bad.** An operator who enters the wrong bib cannot take it back. They can only append a
correcting entry, and both rows survive. This is deliberate and it will feel like a missing
feature at 11 p.m., exactly as it does for status declarations.

**Bad.** Two officials recording the same crossing produce two timing events, and nothing
merges them. Scoring picks a finish per participant so the result is right, but anything that
counts timing events sees both. Corroboration is worth more than a tidy count, and a system
that silently merged them would be discarding the evidence that they agreed.

**The thing to watch.** The temptation will be `--at now` as a convenience for the common
case where somebody types the entry immediately. The common case is not the dangerous one:
the entry recorded forty minutes later, by a volunteer who is guessing, is where a default
would put a wrong time into a result while looking like a right one.

## Alternatives considered

**A result override with an audit row.** Write the time into the result and append a record
of who did it. Rejected for the same reason
[ADR-0016](0016-status-declarations-are-evidence.md) rejects the mutable status column: it
makes the audit trail a derivative of the truth rather than the truth, and the two can
diverge. If the history is authoritative, it should be the thing that is stored.

**Synthesizing a raw read.** Fabricate a `RawRead` with the claimed time and let the existing
pipeline handle it, which would need no new table and no new origin. Rejected outright — it
puts a read in the journal that no reader ever produced, which is precisely the property
[ADR-0005](0005-raw-read-append-only-journal.md) exists to guarantee against. The journal
means "this is what the hardware told us"; a single fabricated row makes it mean nothing.

**Letting a manual entry suppress the reads near it**, on the grounds that an operator
correcting a bad read knows better than the mat. Rejected because it conflates two different
acts. Recording what you saw is evidence; deciding that a read is wrong is a scoring
judgement, and it belongs with the deduplication policy where it can be reviewed and changed
without destroying anything.

**A `source` column on `raw_reads` instead of a separate table.** One table, a discriminator,
no new origin variant. Rejected because the two records have genuinely different shapes — a
manual entry has no chip, no antenna, no RSSI, no payload, and has an actor and a reason that
a read never has — and because it would put operator prose into the table the
[diagnostic bundle](0020-diagnostic-bundles-carry-no-participant-data.md) counts most freely.
