# ADR-0026: A reader gap is two append-only rows, paired by sequence number

- **Status:** Proposed
- **Date:** 2026-09-01
- **Deciders:** —

## Context

[ADR-0025](0025-m3a-proves-durability-above-the-transport.md) made detecting a disconnection a
deliverable of M3a, and said four things about the evidence it produces:

> **A gap is append-only evidence and an input to derivation**, never a correction applied to
> reads. [ADR-0011](0011-append-only-enforced-by-triggers.md) governs the table […]
>
> **A gap is bounded.** Reconnect writes its end. A gap whose end is the end of the journal is
> itself a reportable state, not an omission.

**Those two cannot both be true of one row, and nobody noticed while ADR-0025 was in review.**
ADR-0011 is not a convention — it is a pair of triggers that `RAISE(ABORT)` on `UPDATE` and on
`DELETE`, and the project demonstrates them by going at the file with a plain SQLite driver and
no SplitForge code in the path. A gap row that is opened when the reader vanishes and has its
end written in when the reader returns is, precisely, an `UPDATE`. The trigger would refuse it,
and the refusal would be correct.

So the shape of the row has to be decided before any of it can be stored, and it is the kind of
decision the ADR process names explicitly: *changing the shape of stored data*, and *anything
touching the read path*.

The constraint that makes this awkward is real rather than bureaucratic. A gap is the only piece
of evidence in this project whose **end arrives later than its beginning** and is not known when
the beginning is recorded. A raw read, a clock step, a manual entry, a status declaration — each
is complete at the moment it is written. A gap is not, and an open gap has to survive a power
cut, because a power cut during a gap is exactly the circumstance the gap exists to describe.

## Decision

**A gap is two rows in one append-only table, paired by sequence number.**

`reader_gap_events` holds one row per *edge*. An `opened` row records that a reader stopped
producing and how that was noticed. A `closed` row records that it resumed, and carries
`closes_seq` — the `seq` of the `opened` row it ends. Neither row is ever updated.

**A gap is derived by pairing**, exactly as results are derived rather than stored: an `opened`
row with a matching `closed` row is a bounded gap; an `opened` row with no match **is** the open
gap. "Currently in a gap" therefore needs no flag, no in-memory state, and no shutdown hook — it
is a question about rows, and it answers correctly after a power cut, because the `opened` row
was on disk before the power went.

**`detection` lives on the `opened` row and is never revised.** `confirmed` means the device node
went away or a read failed outright. `suspected` means the stream went quiet for longer than the
configured interval — ambiguous by construction, because a checkpoint with nobody crossing it is
also silent. ADR-0025's rule that a suspected gap *"is recorded as suspected, never as
confirmed"* is enforced by there being no row that could say otherwise.

**At most one gap is open per reader**, and the first detection wins. A confirmed disconnection
arriving while a suspected gap is already open does not open a second gap and does not upgrade
the first — it is the same outage, noticed twice, and the field honestly records how it was
noticed *first*.

## Consequences

### What this makes easy

- **ADR-0011 stands unmodified.** No exemption, no per-column carve-out, and no first mutable
  evidence table. The triggers that protect `raw_reads` protect this table in the same words.
- **An open gap survives a crash**, which is the case that matters most and the one an in-memory
  representation gets wrong. The `opened` row is durable the moment it is written.
- **The table reads as a log**, which is what it is. `ORDER BY seq` is the history of a device's
  connectivity, and `seq` is the one ordering that does not depend on a clock this project
  already records the untrustworthiness of.

### What this makes hard

- **Every reader of the table pairs rows.** There is no single row anybody can `SELECT` to see a
  gap, and a query that forgets the `LEFT JOIN` sees edges rather than gaps. The pairing is
  therefore written once, in storage, and exposed as `ReaderGap` — callers do not assemble it.
- **A malformed pair is expressible.** A `closed` row naming a `seq` that is not an `opened` row,
  or two `closed` rows naming the same one, are both writable by anything holding the file. A
  foreign key and a `UNIQUE` constraint on `closes_seq` are cheap, and are used.

### What we accept

- **A gap's end is a second row rather than a column**, so the schema is less obvious to a person
  reading it cold than `started_at` / `ended_at` would be. The comment in the migration carries
  the reason, because the reason is not visible in the columns.
- **The first detection wins, and that is sometimes the less informative one.** A module whose
  stream goes quiet and whose cable is *then* pulled is recorded as `suspected`. The alternative —
  closing the suspected gap and opening a confirmed one — manufactures a "reads resumed" edge at a
  moment when nothing resumed, which is a worse lie than an under-confident label.
- **Nothing here is observed against hardware.** The silence threshold is
  [Q14](../open-questions.md#q14-reader-silence-threshold) and has no answer, so it is
  configurable with a conservative default and no accuracy claim attached to it.

## Alternatives considered

| Alternative | Why not |
|---|---|
| One row, `ended_at` written by `UPDATE` | Requires exempting this table from ADR-0011's trigger, making it the first mutable evidence table in the project. The whole argument of ADR-0011 is that the database enforces immutability rather than whoever reviews the pull request; an exemption for the convenient column is how that argument stops being true. |
| One row, written only when the gap closes | Genuinely immutable and the simplest to query — and it loses the open gap on a power cut, which is the one moment the evidence is most wanted. ADR-0025 already ruled it out in advance: *"a gap whose end is the end of the journal is itself a reportable state, not an omission."* |
| Keep the open gap in memory, persist on close | The same loss as above, plus it makes `/health` and `doctor` disagree across a restart about whether the device was ever disconnected. |
| A separate `open_gaps` table, deleted on close | `DELETE` is refused by the same trigger, for the same reason, and a two-table version of one fact invites them to disagree. |

## References

- [ADR-0025](0025-m3a-proves-durability-above-the-transport.md) — makes gap detection a
  deliverable, and is where the contradiction this ADR resolves was introduced. **Not
  superseded:** every decision in it stands, including the restated exit criterion; what is
  decided here is the storage shape it left underspecified.
- [ADR-0011](0011-append-only-enforced-by-triggers.md) — the triggers this table obeys
- [ADR-0023](0023-manual-entries-are-derivation-inputs.md) — the rule governing a gap's role in
  derivation: an input, never an edit to the output
- [ADR-0018](0018-write-ahead-sidecar-journal.md) — why a gap needs no sidecar: like a clock
  step, it is derived from state the process already holds
- [Q14](../open-questions.md#q14-reader-silence-threshold) — the silence threshold, unanswered
