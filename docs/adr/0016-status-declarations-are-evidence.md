# ADR-0016: A disqualification is evidence, not a column

- **Status:** Accepted
- **Date:** 2026-08-16
- **Supersedes:** —

## Context

Milestone 4 introduces four statuses: `Finished`, `DNS`, `DNF`, `DQ`. Three of them are
conclusions the software can reach on its own — a finish crossing means finished, a start
without a finish means DNF, no crossings at all means DNS.

**`DQ` is different in kind.** No arrangement of chip reads implies "cut the course at the
turnaround", "took a tow", or "wore somebody else's chip". A disqualification is a decision
a human made, usually after hearing an account of something the timing system never
observed. The same is true of the other statuses when an operator overrides them: a marshal
who watched a runner cross with a dead chip is asserting a fact the reader missed.

That leaves a modelling question with two obvious answers.

**Reading one — a status column.** `participants.status`, updated in place. Simple, and it
matches how most people would first draw it. `UPDATE participants SET status = 'dq' WHERE
bib = '217'`.

**Reading two — a declaration table.** Append a row saying who declared what, when, and
why. The status in force is derived by taking the most recent row per participant.

Reading one has a specific, ugly failure. Three weeks after the race, a runner's lawyer
asks why bib 217 is not in the results. The database says `status = 'dq'`. It does not say
who decided that, when, on what grounds, or whether it was ever anything else. If somebody
disqualified the wrong bib at 11:04 and corrected it at 11:31, the column shows only the
correction — and the provisional results that went out at 11:15, with the wrong runner
removed, are unexplainable from the database alone.

`docs/timing-model.md` § 8 sets exactly this test:

> Given a finished event and a disputed result, can you reconstruct — from the database
> alone, without logs, without asking anyone — *why* that participant received that time,
> and *what changed* between the provisional and final results?

A mutable status column fails it.

## Decision

**Status declarations are append-only evidence, in the same sense as raw reads and manual
entries.**

- `status_declarations` records `(id, seq, race, participant, status, reason, actor,
  declared_at)`. `BEFORE UPDATE` and `BEFORE DELETE` triggers abort, per
  [ADR-0011](0011-append-only-enforced-by-triggers.md).
- **The status in force is the declaration with the highest `seq` for that participant.**
  Reversing a disqualification means appending a new declaration, not deleting the old one.
  Both rows survive, and `splitforge results declarations` shows all of them with the
  current one marked.
- **`reason` is required**, at the parser, not as a runtime check. A status with no stated
  grounds cannot be defended later, so it should not be reachable from the command line.
- A declaration overrides the derived status. Everything else stays derived: statuses that
  nobody declared are recomputed from the crossings every time.

This puts declarations on the evidence side of
[ADR-0014](0014-mutable-configuration-immutable-evidence.md)'s line, alongside
`race_sessions` ([ADR-0015](0015-race-start-records-the-gun.md)) and for the same reason —
an operator typed it, which makes it a primary record.

## Consequences

**Good.** The audit question above is answerable from the database alone. A DQ and its
reversal are both visible, in order, with the actor and the stated reason on each. Scoring
stays a pure function: it takes declarations as input rather than reading mutable state,
which is what lets the whole of `splitforge-results` be tested without a database.

**Good, unexpectedly.** Because the declaration carries its own `declared_at`, a status
applied after the fact is representable without lying about when it was decided — the same
property `--at` gives `race start`.

**Bad.** Reading the current status is a query with a `MAX(seq)` in it rather than a column
read. At the scale of a race roster this is irrelevant, and the index
`status_declarations (race_id, participant_id, seq)` is that query read backwards. It would
matter at a scale this project explicitly does not target.

**Bad.** An operator who disqualifies the wrong bib cannot make it go away. They can only
add a row saying they were wrong. This is the intended behaviour and it will still feel
like a missing feature the first time somebody hits it at 11 p.m.

**The thing to watch.** The temptation will be a "clean up test declarations" convenience,
or a `--force` that deletes. The triggers make that a database error rather than a code
review question, which is the point.

## Alternatives considered

**A mutable status column with an audit trigger.** Keeps the simple read and writes history
to a side table. Rejected because it makes the audit trail a derivative of the truth rather
than the truth — and the two can diverge, which is precisely the failure mode being
designed out. If the history is authoritative, it should be the thing that is stored.

**Storing the status on the result entry only.** Statuses would exist solely inside
published revisions, which are already immutable. Rejected because a status has to exist
*before* it can be published — an operator disqualifies a runner and then publishes, not
the other way round — and because it would make the status unrecoverable between revisions.

**Allowing a declaration to be edited before the first publication**, on the grounds that
nothing has been published yet so nothing has been relied upon. Rejected: it makes
immutability conditional on a state that is easy to misjudge, and "nobody has seen it yet"
is exactly what somebody believes right before discovering otherwise.
