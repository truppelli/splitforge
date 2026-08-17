# ADR-0018: Evidence is written to a text sidecar before it reaches the database

- **Status:** Accepted
- **Date:** 2026-08-17
- **Resolves:** [Q7](../open-questions.md#q7-corruption-recovery-strategy)
- **Extends:** [ADR-0005](0005-raw-read-append-only-journal.md), [ADR-0003](0003-sqlite-wal-local-persistence.md)

## Context

[ADR-0005](0005-raw-read-append-only-journal.md) made raw reads immutable evidence, and
[ADR-0011](0011-append-only-enforced-by-triggers.md) made the immutability structural. Both
protect the rows *inside* the database. Neither says anything about the database itself
being unreadable, which [architecture.md § 4](../architecture.md#4-failure-behavior) has
carried as an **OPEN** row since Milestone 0 and
[threat-model.md O4](../threat-model.md#operational-risks) rates Critical.

The gap is specific. SQLite is not the risk here — it is among the most tested software in
existence, and `synchronous = FULL` handles power loss correctly on hardware that honors
`fsync`. The risk is everything under it: a microSD card that lies about flushing, a card
that wears out mid-event, a filesystem damaged by a brownout, a partial write during a
cable-pull that leaves a torn page. On a battery-powered Pi 3 writing to consumer flash,
these are not exotic.

And the blast radius is total. One damaged page in the wrong place makes the file
unopenable, and a file that will not open holds every read of the event. The pre-race
snapshot bounds the loss to "everything since the last backup," which for a race is
precisely the interval that matters — nobody takes a backup during the finish rush.

`splitforge doctor` could already *detect* corruption. It had nothing to offer afterwards.

## Decision

**Every raw read is appended to a plain-text sidecar file and fsynced before the database
transaction that stores it opens.**

```
event.db                  the database
event.db.reads.jsonl      the sidecar
```

```
append_batch(reads):
  1. serialize each read to a line
  2. write_all + fsync the sidecar     <-- the read is now on disk
  3. BEGIN; INSERT ...; COMMIT         (synchronous = FULL)
  4. return
```

The ordering is the entire decision. Because the sidecar is written first, it is a
**superset of the database at every instant** — never a subset, never a race. A crash
anywhere in step 3 leaves a read that exists on disk in a form nothing needs SQLite to
read.

### The line format

```text
SFJ1 <64 hex characters of sha256(json)> <json>\n
```

- **Version tag first**, so a later format can change everything after it. A reader that
  meets an unfamiliar tag refuses rather than guesses
- **Digest before payload**, so a line is rejected without being parsed
- **JSON carries the `raw_reads` columns**, with timestamps as microseconds since the Unix
  epoch — the same encoding the database uses, so a replayed read is the read that was
  stored rather than a re-interpretation of it
- **Every field on every line.** A recovery format must not have optional syntax: a missing
  field should fail loudly, never decode quietly as `None`
- **No sequence number.** It is assigned by `AUTOINCREMENT` when the row is inserted, which
  is after this file is written. Writing a guess would write a number that is wrong exactly
  when it matters

### The acknowledgment point does not move

A read is still durable when the database commit returns, exactly as
[ADR-0003](0003-sqlite-wal-local-persistence.md) and `splitforge-storage`'s durability
contract say. The sidecar is a backstop, not a new system of record. This costs a second
fsync per batch and keeps every existing guarantee literally true.

### Recovery is bidirectional, and belongs to the writer

`reconcile` takes the **union**: reads the sidecar holds and the database lacks are
inserted; reads the database holds and the sidecar lacks are appended. Picking a winner
would mean deciding one of two copies of the evidence is not evidence. The backfill
direction is also how a database written before this ADR acquires a sidecar.

It runs automatically when the **writing** process starts, and nowhere else. `reads
--follow` opens a journal too, and an observer that repairs what it is observing is a
command that writes to a database the operator believes it is only reading. Everyone else
gets `survey`, which reports the divergence without touching it; `doctor` turns that into a
finding naming the command that fixes it.

### The sidecar and the snapshot divide the problem

The sidecar carries **reads only**. Configuration — roster, checkpoints, chips, policies —
comes back from the pre-race snapshot. The two mechanisms split along the line where the
problem actually splits:

| | Changes | Recovered by |
|---|---|---|
| Configuration | Rarely, before the gun | `backup restore` |
| Evidence | Every second of the race | `recover` |

`backup restore` therefore never touches the sidecar. Moving it aside during a restore
would discard the only copy of exactly the reads the snapshot is missing.

## Consequences

### What this makes easy

- "The database is corrupt" stops meaning "the reads are gone." It means two commands
- Recovery is inspectable with `tail`, `grep`, and eyes. The failure this defends against
  is the one where good tooling is unavailable
- The recovery path is exercised on every writer start, not only during a disaster. A
  mechanism whose first real execution is the emergency is a mechanism nobody has tested
- Damage is bounded per line: one rotted line costs one read, not the file
- A crash between the sidecar write and the commit is now an ordinary, recoverable state
  rather than an inconsistency

### What this makes hard

- **A second fsync per reader report.** Per report, not per tag — a batch is one write and
  one sync — but it is real, and its cost on a microSD card is unmeasured until Milestone 3
  puts a reader on a Pi. If it proves too expensive, the honest fix is a faster storage
  device, not a quieter sidecar
- Disk usage roughly doubles for the read path. A 638-read 5K produces a few hundred
  kilobytes; an all-day event with a busy mat is still megabytes
- A second file to keep, back up, and not commit. `.gitignore` covers `*.reads.jsonl`,
  which matters more than it sounds: it holds chip identifiers and payload bytes, and it
  looks enough like a log file to be committed by accident
- Two files can be separated by a careless copy. A sidecar without its database still holds
  the evidence; a database without its sidecar is what we had before

### What we accept

That this does not make the evidence indestructible, and nothing at this layer could.
`rm -rf` takes both. A filesystem that loses a directory takes both. What it removes is the
single most likely total-loss path — one damaged page in one file — and replaces it with a
format that degrades line by line instead of all at once.

We also accept that these tests destroy files deliberately and therefore do not prove
anything about how an SD card behaves during a brownout. That needs hardware, and it stays
in Milestone 5.

## Alternatives considered

| Alternative | Why not |
|---|---|
| Snapshot restore only | The option Q7 listed first, and the cheapest. It bounds loss to "everything since the last backup," which during a finish rush is the only interval anybody cares about |
| Sidecar written but not fsynced | Halves the cost and defends against bit rot and torn pages. Fails at the co-occurrence that matters most on a Pi: a power cut that both loses the tail and corrupts the file |
| Move the acknowledgment to the sidecar and relax SQLite to `synchronous = NORMAL` | Same fsync count, and arguably more honest — the database *is* a derived index over the evidence. Rejected for now because it supersedes ADR-0003's durability contract to buy performance nobody has yet measured a need for. Worth revisiting with numbers from real hardware |
| Fail over to a fresh database and reconcile later | Q7's second option. Leaves two partial databases and a reconciliation nobody has rehearsed, at the moment least suited to rehearsal |
| Write the sidecar to a second physical device | Strictly better against device failure, and it presumes hardware the project does not require. Compatible with this decision if it ever arrives — the sidecar path is already configurable |
| Cryptographic chaining across lines | Would detect a removed line, not just an altered one. Deferred: it defends against tampering, and this ADR is about damage. [ADR-0011](0011-append-only-enforced-by-triggers.md) drew the same line |

## References

- [Q7: Corruption recovery strategy](../open-questions.md#q7-corruption-recovery-strategy)
- [ADR-0003: SQLite with WAL for local persistence](0003-sqlite-wal-local-persistence.md)
- [ADR-0005: Raw reads are an append-only journal](0005-raw-read-append-only-journal.md)
- [ADR-0011: Append-only is enforced by database triggers](0011-append-only-enforced-by-triggers.md)
- [architecture.md § 4](../architecture.md#4-failure-behavior)
- [threat-model.md O4](../threat-model.md#operational-risks)
