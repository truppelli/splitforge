# ADR-0005: Raw reads are an append-only journal

- **Status:** Accepted
- **Date:** 2026-08-14

## Context

Race results get disputed. A runner who believes they finished in 3:29:58 and sees 3:30:04
on the results sheet will ask why, sometimes months later, sometimes with a Boston
qualifying place attached to the answer.

A timing system that stores only its conclusions cannot answer that question. If the
deduplication window was too aggressive, or a chip was assigned to the wrong bib, or a
reader's clock was fast, a system that mutated its data in place has destroyed the only
evidence that could establish what happened.

The failure mode is worse than being wrong. It is being wrong and unable to demonstrate
how.

## Decision

**Raw reads are immutable evidence, stored append-only, and never modified or deleted on
any normal code path.**

- Every read is persisted **before** deduplication, chip resolution, or any validity check
- Reads from unrecognized chips are stored — an unmapped EPC is usually a roster error, and
  finding that out afterward requires having kept it
- No `UPDATE` and no `DELETE` against `raw_reads` exists in the application
- Every derived record — `accepted_reads`, `timing_events`, `result_entries` — references
  the raw reads that produced it
- Corrections produce **new** derived records or a **new** result revision. They never
  rewrite evidence
- Manual entries are evidence too, and get the same treatment
- A clock error is corrected with a `time_correction` model applied at derivation time,
  never by rewriting timestamps
  ([clock discipline § 9](../clock-and-time-discipline.md#9-correction-happens-at-derivation-never-in-the-journal))

The ordering constraint is absolute: **the journal write completes before the engine is
told anything.** A crash between those steps loses a derivation, which is recomputable. The
reverse ordering would lose evidence, which is not.

## Consequences

### What this makes easy

- Any scoring bug is fixable after the fact by re-deriving. The event is not lost
- Disputes have an answer that does not depend on memory or logs
- Crash recovery is boring: re-derive from the journal and compare
- Deduplication tuning can be validated against real recorded events
- Removes an entire class of both attacks and accidents — you cannot quietly alter what
  cannot be altered

### What this makes hard

- More storage. Hundreds of reads per crossing are retained rather than collapsed
- "Fixing" data requires understanding the derivation model instead of editing a row
- Schema migrations against an append-only table need care
- Operators used to spreadsheet-style correction will find it unfamiliar

### What we accept

Storage cost, in exchange for the ability to reconstruct any result from evidence. On the
scale SplitForge targets — thousands of reads per event, not millions — this is not a
meaningful cost. A full day of reads with payloads is megabytes.

We also accept that the invariant is currently enforced by convention and code review.
Making it structural is [Q8](../open-questions.md#q8-enforcing-append-only-in-sqlite), and
the leaning is to add SQLite triggers that raise on `UPDATE`/`DELETE`.

## Alternatives considered

| Alternative | Why not |
|---|---|
| Store deduplicated reads only | Smaller and simpler, and destroys the ability to re-tune deduplication after an event. This is the mistake the whole ADR exists to avoid |
| Mutable reads with an audit log | The audit log becomes the real evidence while being easier to lose or corrupt than the data it describes. Two sources of truth is zero |
| Soft deletes (`deleted_at`) | Still a mutation, and every query must remember to filter. Invariants enforced by discipline get violated |
| Event sourcing across all entities | Philosophically aligned but far heavier. Append-only where evidence lives, mutable where configuration lives, is the right split |

## References

- [timing-model.md § 1](../timing-model.md#1-the-evidence-principle)
- [threat-model.md](../threat-model.md)
