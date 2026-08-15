# ADR-0011: Append-only is enforced by database triggers

- **Status:** Accepted
- **Date:** 2026-08-14
- **Resolves:** Q8
- **Extends:** [ADR-0005](0005-raw-read-append-only-journal.md)

## Context

[ADR-0005](0005-raw-read-append-only-journal.md) established that raw reads are immutable
evidence. It also admitted the gap in its own consequences section: the invariant was
enforced by convention and code review, which is another way of saying it was enforced by
nobody at 2 a.m.

The realistic threat is not a malicious operator rewriting a finish time. It is an accident:
a cleanup script with a `DELETE` in it, a migration that rebuilds a table and drops rows, a
well-meaning `UPDATE raw_reads SET chip = ...` intended to fix a typo. Every one of those
destroys the only evidence behind a result, and none of them announces itself.

A guarantee that depends on everyone remembering it is not a guarantee. The question was
whether to make it structural, and at what cost.

## Decision

**`raw_reads` carries `BEFORE UPDATE` and `BEFORE DELETE` triggers that `RAISE(ABORT)`.**

```sql
CREATE TRIGGER raw_reads_no_update
BEFORE UPDATE ON raw_reads
BEGIN
    SELECT RAISE(ABORT, 'raw_reads is append-only: UPDATE is not permitted');
END;
```

- The application has no `UPDATE` or `DELETE` path for `raw_reads`. The triggers are a
  second line, not the only one
- The abort message is prose, not an error code, because the person who hits it is holding
  a `sqlite3` prompt rather than a debugger
- `splitforge-storage` translates the failure into `JournalError::AppendOnlyViolation`, so
  the violation keeps its meaning as it crosses the port boundary rather than arriving as a
  generic backend error
- `seq` is `INTEGER PRIMARY KEY AUTOINCREMENT` rather than a plain rowid, so identifiers are
  never reused after a delete. Nothing should ever be deleted — but a monotonic sequence
  that cannot silently restart is what makes "did we lose a read?" answerable independently
  of any clock
- A future migration that genuinely needs to rebuild the table must drop and recreate the
  triggers **explicitly**, in the migration SQL, where it is visible in review

## Consequences

### What this makes easy

- The invariant survives contact with people who have not read ADR-0005, including future
  contributors and the author at 2 a.m.
- Accidental destruction becomes a loud error instead of a silent success
- Sequence gaps are now diagnostic: a gap means something interesting happened, rather than
  meaning someone tidied up

### What this makes hard

- Legitimate schema migrations that rebuild the table need extra ceremony
- Anyone with the database file can drop the triggers. This stops accidents, not adversaries
  with filesystem access — those are [threat-model.md](../threat-model.md)'s problem, and no
  trigger solves them
- Test fixtures cannot clean up by deleting rows. They use a fresh temporary database
  instead, which is better practice anyway

### What we accept

That this is a guardrail, not a security control. It defends against the mistake, which is
the failure that actually happens. It does not defend against someone who has decided to
alter the evidence and has write access to the file — nothing at this layer can, and
claiming otherwise would be worse than not having the triggers at all.

## Alternatives considered

| Alternative | Why not |
|---|---|
| Convention and code review only | The status quo ADR-0005 described, and the one it flagged as insufficient. Review catches the deliberate change and misses the incidental one |
| A read-only database connection for most code | Would not help: the write path needs write access, and that is exactly the path a bad `UPDATE` would travel |
| Application-level checks | The check has to live where the write happens. Anything that reaches SQLite by another route — a migration, a repair script, `sqlite3` — bypasses it entirely |
| Cryptographic chaining (each row hashes the previous) | Detects tampering rather than preventing it, and only for a reader who verifies the chain. Worth revisiting for exports; overkill for the accident this addresses |

## References

- [ADR-0005: Raw reads are an append-only journal](0005-raw-read-append-only-journal.md)
- [timing-model.md § 4](../timing-model.md#4-raw-read)
- [threat-model.md](../threat-model.md)
