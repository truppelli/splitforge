# ADR-0031: A failed write is retried, and a read the journal cannot store is set aside

- **Status:** Accepted
- **Date:** 2026-09-14
- **Supersedes:** —

## Context

The [2026-09-13 security review](../roadmap.md#security-review--2026-09-13) found that
`read_into_journal` in `splitforge-edge` returned on the first failed append. Health went to
503, and the process stayed up, so `Restart=always` never fired. Every read the module sent
after that was dropped, and a serial module has no flow control to hold them. The triggers it
named are ordinary: a transient `ENOSPC` or `EIO` on an SD card, `SQLITE_BUSY` lasting longer
than the 5 s busy timeout, and at Milestone 3b a single value the schema cannot store, such as
an LLRP `Uptime` of 2⁶³ µs or more.

The review offered two fixes: retry with bounded backoff, or exit non-zero so systemd restarts
the service and startup recovery replays the sidecar. Checking the second against the code
found that it does not work for the last trigger, and that the first is not safe as the code
stood.

- **A value the schema cannot store poisoned recovery.** The sidecar stores `reader_uptime_us`
  as a JSON number, so the sidecar append succeeded. The database insert then failed. The
  read was now a sidecar line the database could never hold, and replay runs in one
  transaction. Every later start would fail to replay it, fail to open the journal, and exit.
  Exiting on that failure would have turned one read into a crash loop that records nothing.
- **A retry could poison recovery too.** When only the database half of an append fails, a
  retry writes the sidecar line again. `raw_reads.id` is unique, so replaying both copies
  fails the transaction, with the same result.

The comment on the loop gave the reason it stopped: *"A timer that cannot persist evidence must
say so and stop, not keep counting into a journal that is missing rows."* That concern is real.
Stopping was the wrong answer to it, because stopping also drops every read after the failed
one.

## Decision

**1. A failed append is retried with the same read, for as long as it takes.** The delay starts
at 100 ms and doubles to a ceiling of 5 s. The read path never moves past a read it could still
store, so it never counts into a journal missing a row. While it retries, `/health` is degraded
with the current reason, and a write that lands clears it.

**2. A read the journal cannot represent is refused before either file is written.**
`StorageError::Unstorable`, surfaced as `JournalError::Unstorable`, means retrying cannot help.
The check runs before the sidecar append, so the sidecar and the database always agree about
the read, including when neither has it.

**3. A refused read is set aside on the record, and recording continues.** The read path writes
it to the audit trail as `journal.unstorable`, by `splitforge-edge`, with every field it carried,
the raw payload in hex, and the reason. It counts it in `reads_set_aside`, degrades health for
the rest of the process's life, and moves to the next read.

**4. Replay takes each read id once, and skips a line the database cannot hold.** Such a line
is counted with the corrupt lines, as damage, rather than failing the recovery of every read
around it. This build cannot write one, so a line like that came from an older build or from a
hand.

**5. A poisoned lock ends the process.** Something panicked while holding the journal, nothing in
the process can make it usable again, and a restart with recovery can.

## Consequences

### What this makes easy

- A disk that fills and is then freed, a lock held too long, or a card that fails a write and
  then succeeds loses no read that reached the device, beyond what back-pressure costs at the
  port.
- Health says which read is waiting and why, so an operator can see the problem while it is
  still happening.
- No single read can stop recording, now or at Milestone 3b.

### What this makes hard

- **A write that never succeeds keeps the service up and recording nothing.** That was the
  review's complaint about stopping, and retrying forever has the same result when the fault
  is permanent, such as a card that has gone read-only. The difference is that health says so
  the whole time, and a restart would not help: recovery would open the same card. Exiting
  after some number of failures was considered and rejected below.
- **While it retries, reads queue and then stop being taken off the port.** Back-pressure works
  as architecture § 4 describes, and on a serial module the reads it emits meanwhile are gone.
  That is true of any fix that does not skip reads, and skipping reads is the one thing the
  journal must not do.
- **A retry after a database-only failure leaves a duplicate line in the sidecar.** Replay reads
  it once. `doctor`'s record count includes it.

### What we accept

- **A set-aside read is not in the journal and does not score.** The audit row keeps it
  inspectable, and an operator can enter it by hand if it matters. The alternative would be
  changing the value so it fits, and evidence is not changed.
- **The count of set-aside reads lives in the process.** It resets when the service restarts. The
  audit rows do not.
- **Health can be degraded for one read for the rest of the event.** Each set-aside read is a
  permanent fact about that event, so the degradation stays until the service restarts.

## Alternatives considered

| Alternative | Why not |
|---|---|
| Exit non-zero on any failed append | A value the schema cannot store would still be in the sidecar, and replaying it would fail every start. Even without that, health is unreachable while the service restarts, and a full disk restarts it once a second |
| Retry a bounded number of times, then exit | Nothing a restart does helps a full disk, a lock held by another process, or a dying card. It adds a gap in health during the event and a recovery pass over the whole sidecar every time |
| Retry an unstorable read like any other | It will fail the same way every time, so the read path stops exactly as it did before |
| Clamp the value so it fits | Changes evidence. A reader that reports a nonsensical uptime has told us something, and the value it sent is the record of that |
| Store the unstorable read in the sidecar only | The sidecar is replayed into the database, and this read cannot be. It would be the poisoned line again |
| Avoid the duplicate sidecar line by remembering which reads are already written | State in the journal that has to be right across every failure path. Replaying each id once is simpler, and it also handles a duplicate from any other cause |

## References

- [ADR-0005](0005-raw-read-append-only-journal.md): raw reads are append-only evidence
- [ADR-0018](0018-write-ahead-sidecar-journal.md): the sidecar, and why it is written first
- [ADR-0022](0022-the-service-never-waits-for-the-network.md): `Restart=always` and
  `StartLimitIntervalSec=0`
- [Architecture § 4](../architecture.md#4-failure-behavior)
- [Roadmap: Security review, 2026-09-13](../roadmap.md#security-review--2026-09-13)
- `apps/splitforge-edge/src/main.rs`: `store`
- `crates/splitforge-storage/src/journal.rs`: `storable`, `SqliteJournal::record_unstorable`
