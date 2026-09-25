# ADR-0037: Startup recovery reads what it has not already verified, a line at a time

- **Status:** Accepted
- **Date:** 2026-09-25
- **Extends:** [ADR-0018](0018-write-ahead-sidecar-journal.md), [ADR-0031](0031-a-failed-write-is-retried-and-an-unstorable-read-is-set-aside.md)

## Context

[ADR-0018](0018-write-ahead-sidecar-journal.md) writes every read to a text sidecar before the
database, and has the writing process reconcile the two on every start. It counts that as a
strength: *"the recovery path is exercised on every writer start, not only during a disaster."*
[ADR-0031](0031-a-failed-write-is-retried-and-an-unstorable-read-is-set-aside.md) made the replay
one transaction that takes each id once, and named the price of restarting in its alternatives:
*"a recovery pass over the whole sidecar every time."*

That price has now been measured
([roadmap, hygiene](../roadmap.md#hygiene)). Synthetic journals, grown from the `five-k`
fixture's lines and replayed by the service itself, on an x86 machine rather than a Pi:

| Reads | Sidecar | Start, the two agree | Start, replaying all |
|---|---|---|---|
| 100k | 57 MB | 93 MB peak, 0.3 s | 93 MB, 1.5 s |
| 500k | 283 MB | 444 MB, 2.1 s | 445 MB, 10.9 s |
| 1M | 565 MB | 883 MB, 4.1 s | 883 MB, 25.5 s |

Two costs, and both scale with how long the event has run rather than with how much there is to
repair.

**Memory.** `compare` reads the whole sidecar into memory, parses every line into a record,
and builds a `HashSet<String>` of every id on each side. That is about 0.9 KB per read, or 1.6
times the sidecar, on an ordinary restart that repairs nothing. On the Pi 4's 2 GB
([ADR-0036](0036-raspberry-pi-4-is-the-edge-target.md)) the service would fail to start
somewhere around 1.5 to 2 million reads, and `Restart=always` would turn that into a crash loop
that records nothing for the rest of the event.

**Time with no reader.** `splitforge-edge` opens the journal with `open_recovering` before it
composes the reader, so nothing is read from the module until recovery returns, and a runner
crossing in that window is not recorded. Recovery re-reads and re-digests every line on every
start, so a power cut late in a long event costs the most. 4.1 s at a million reads on x86 is
likely several times that on a Pi.

The restart that matters most is the one after a power cut mid-event. It is also the one where
the database is almost always intact, and where the sidecar and the database agreed about every
read but the last few.

## Decision

### 1. Recovery streams the sidecar and holds ids, not reads

The sidecar is read a line at a time, not with one `std::fs::read`. Read ids are UUIDs, and are
held as 16-byte values, not strings. A record is kept in memory only if it has to be
replayed or backfilled; everything else is checked and dropped. The torn-tail and torn-write
rules of ADR-0018's line format are unchanged: the complete line behind the remains of an
interrupted write is still found and kept.

**The replay is still one transaction** (ADR-0031), but reads are inserted as the scan finds
them, not collected first. `survey` and `reconcile` still share one comparison, so what
`doctor` reports and what the service repairs cannot disagree; the difference between them is
whether a missing read is counted or inserted.

### 2. The journal records how far the sidecar and the database are known to agree

A new append-only table, `sidecar_checkpoints`, holds rows of:

| Column | Meaning |
|---|---|
| `sidecar_bytes` | An offset in the sidecar, at the end of a complete line |
| `last_line_sha256` | The digest carried by the complete line that ends there |
| `through_seq` | The highest `raw_reads.seq` at that moment |
| `recorded_at` | When the row was written |

A row claims: **every read in the sidecar before `sidecar_bytes` is in `raw_reads`, and every
row in `raw_reads` up to `through_seq` is in the sidecar before `sidecar_bytes`.** It is written
only at a moment when that is true:

- **by `reconcile`**, when it finishes, for the part of the file it read, because the union it
  just took makes it true;
- **by the writing process**, holding the journal's lock, directly after an append has
  committed, at most once every 60 seconds, and on a clean shutdown. At that moment every line
  this process has written has its row. A retry after a database-only failure (ADR-0031) is
  finished before the next read is taken, so it cannot be in progress then. Only the writer
  appends to the sidecar; `doctor` and `reads --follow` never do.

### 3. A writer's start reads only what comes after the latest checkpoint

`open_recovering` takes the latest checkpoint and trusts it only if the file still matches it:
the sidecar is at least `sidecar_bytes` long, and the complete line that ends there carries
`last_line_sha256`. Then it reads from that offset on, and compares database rows after
`through_seq` against what it reads. After a power cut, that is the reads since the last
checkpoint, which is at most a minute of the event.

It reads the whole sidecar, streamed as in part 1, when there is no checkpoint, when the
checkpoint does not match the file, and always for `splitforge recover`. A database deleted and
created afresh has no checkpoint. A database restored from a snapshot has the checkpoint the
snapshot was taken with, and every read after it is in the part of the file that is read.

**`doctor` always reads the whole sidecar.** It is the full verification, it runs when an
operator asks, and running out of memory or taking a while costs that command and not the
event.

### 4. The reader still starts after recovery

This ADR does not move reader composition ahead of `open_recovering`. With parts 2 and 3, an
ordinary restart replays a minute of reads at most. The long case that remains is a database
that has been destroyed, and that is a disaster with its own procedure.

## Consequences

### What this makes easy

- **An ordinary restart costs what the last minute costs**, not what the day has cost, in both
  memory and time. The window in which a crossing goes unrecorded after a power cut stops
  growing with the event.
- **The full scan costs a tenth of the memory.** About 32 bytes per id on each side instead of
  about 100, plus the reads actually being repaired, instead of the whole file and every parsed
  record.
- ~~**A forged line inserted into the verified part of the sidecar is no longer replayed by a
  restart.**~~ *Amended 2026-09-25, when this was implemented: it was wrong.* A line inserted
  into the verified part moves every line after it, so the line ending at the checkpoint's
  offset is no longer the one it recorded. The checkpoint is not trusted, the whole file is
  read, and the inserted line is replayed and audited as `journal.replay`, as it was before this
  ADR. `a_line_inserted_before_the_checkpoint_makes_the_start_read_everything` holds that. Only
  a line overwritten in place with another of exactly the same length, leaving the checkpoint's
  line where it was, is now passed over by a restart; `doctor` still reports it and
  `splitforge recover` still replays it, on the record. The sidecar remains a write path into
  the journal, as the 2026-09-13 review found.

### What this makes hard

- **ADR-0018's "exercised on every start" becomes partly untrue.** Every start still runs
  recovery, over the part after the checkpoint. The full scan runs on `doctor`, on
  `splitforge recover`, and on a start without a usable checkpoint. The full path needs tests
  that force it, because an ordinary start no longer does.
- **A new piece of state that has to be right.** A checkpoint that claimed more than was true
  would hide reads from recovery. So it is written only at the moments listed in part 2,
  validated against the file before it is trusted, and ignored rather than repaired when it
  does not match.
- **A schema migration, and another append-only table** with the same triggers as the others
  ([ADR-0011](0011-append-only-enforced-by-triggers.md)).
- **One more commit a minute on the read path**, under the journal's lock. Small, and it is a
  cost on an SD card that M3a should measure with the rest.

### What we accept

**A destroyed database is still recovered before the first read.** Its start reads the whole
sidecar, streamed. At a million reads that was 25.5 s on x86 before this change and will be
longer on a Pi, with the reader waiting. Starting the reader first was considered and not
chosen; see below.

**A sidecar damaged only in its verified part is not noticed by a restart.** Today every start
reports a rotted line anywhere in the file. After this, a start reports one only after the
checkpoint, and `doctor` reports all of them. The database holds every read in the verified
part, so a rotted line there loses nothing unless the database is lost too, and then there is
no checkpoint and the whole file is read.

**Memory is still proportional to the event on a full scan**, at about a tenth of the rate.

## Alternatives considered

| Alternative | Why not |
|---|---|
| Streaming alone, still reading the whole sidecar on every start | Fixes memory, not time. The window with no reader still grows with the event, and after a power cut late in a long event it is at its longest exactly when a restart is most likely |
| Start the reader before recovery, and recover alongside it | The read path writes the sidecar and then the database; a recovery reading the same file could see a new line before its row and replay it, and the read path's insert would then fail on the unique id and retry forever (ADR-0031). Bounding recovery to the file's length at start avoids that, but the replay is one transaction on a single-writer database, so the read path's appends would wait behind it for as long as it runs. Batching the replay would give up ADR-0031's all-or-nothing replay and its single audit row. Worth revisiting for the destroyed-database case if M3a shows that case matters |
| Compare the tails of the two files instead of recording a checkpoint | Finding the sidecar's last lines is cheap, but proving the part before them agrees is not, short of reading it. A checkpoint is that proof, recorded at a moment it was true |
| Record the checkpoint in a separate file | A second file to keep beside the database and the sidecar. In the database it travels with backups, which is what makes a restored snapshot recover correctly |
| `MAX(seq)` and file sizes instead of ids | `seq` is `AUTOINCREMENT`, increasing and not gap-free, and a retried append can leave a duplicate line, so neither count proves the two agree |

## References

- [ADR-0018](0018-write-ahead-sidecar-journal.md): the sidecar, its line format, and recovery
  on every start
- [ADR-0031](0031-a-failed-write-is-retried-and-an-unstorable-read-is-set-aside.md): the
  one-transaction replay, and the duplicate line a retry can leave
- [ADR-0011](0011-append-only-enforced-by-triggers.md): how the new table is kept append-only
- [roadmap, hygiene](../roadmap.md#hygiene): the measurements and the two items this answers
- `crates/splitforge-storage/src/journal.rs` (`compare`, `reconcile`) and `src/sidecar.rs`
  (`scan`): what changes
