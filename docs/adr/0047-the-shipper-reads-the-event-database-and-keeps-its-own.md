# ADR-0047: The shipper reads the event database, and keeps what it sent in its own

- **Status:** Proposed
- **Date:** 2026-09-27
- **Extends:** [ADR-0046](0046-raceday-connect-is-reached-with-ureq-and-rustls-from-a-process-of-its-own.md)
- **Amends:** [ADR-0006](0006-optional-outbound-integrations.md) (the `outbox_messages` table), [ADR-0039](0039-raceday-connect-publishes-what-splitforge-derived.md) § 7

## Context

[ADR-0046](0046-raceday-connect-is-reached-with-ureq-and-rustls-from-a-process-of-its-own.md)
made the shipper a process of its own, `splitforge-ship`, and left one question for this ADR:
how it records what it has sent **without taking the write lock the read path appends
through**.

The plan it inherited was [ADR-0006](0006-optional-outbound-integrations.md)'s and
[ADR-0039](0039-raceday-connect-publishes-what-splitforge-derived.md) § 7's: an
`outbox_messages` table in the event database, *"written in the same transaction as what it
describes"*. Two things have changed since that was written.

**There is nothing to write in that transaction.** What RaceDay Connect receives is derived:
the course manifest from the configuration, each runner's crossings from the journal, and
result revisions that the CLI already writes as append-only rows. No command creates a
message. The read path appends raw reads, and nothing derives crossings until something asks.
The outbox that ADR-0039 actually built is a diff. The runner's crossings now, against what
was last sent, is the message.

**A second process writing the event database competes with the read path.** Every write the
shipper made would take the same lock the read path's appends wait on.

Measured on 2026-09-27. The first three rows used Python's `sqlite3` (SQLite 3.40) in the CI
image, as two users mirroring the Pi. The timer owns `/var/lib/splitforge`, `0750`, with its
files `0640`. The shipper is a second user in the `splitforge` group. The last row is
`splitforge export crossings`, which derives the same way the shipper will, in a release build
on x86:

| | Result |
|---|---|
| Shipper opens the database read-only while the timer writes | Reads every committed row. `INSERT` refused: *"attempt to write a readonly database"* |
| The same, while the timer holds a write transaction | Reads the committed rows at once, without waiting |
| `PRAGMA data_version` on the shipper's read-only connection | Changes on each of the timer's commits, and only then |
| Timer stopped, so SQLite has removed `-wal` and `-shm` | **Cannot open**: SQLite must create `-shm`, and the shipper cannot write the directory |
| Deriving crossings at 100k, 500k and 1M reads | 63 MB, 0.55 s; 267 MB, 2.4 s; 525 MB, 5.2 s. About 0.5 KB a read |

## Decision

**1. The shipper runs as its own user, `splitforge-ship`, in the `splitforge` group, and opens
the event database read-only.** Group read is what `/var/lib/splitforge` already grants
([deployment.md](../deployment.md#who-can-do-what)). The kernel refuses its writes, so a bug in
the shipper cannot alter evidence, and in WAL mode its reads never block the timer's writes.

**2. What it has sent lives in a database of its own**, `ship.db`, in the shipper's state
directory (`/var/lib/splitforge-ship`, `0700`). It holds:
- for each race it publishes, the digest of the manifest last delivered;
- for each runner, the digest of the crossings last delivered;
- which result revisions were delivered;
- what is waiting to be retried: its next attempt, how many attempts it has had, and the last
  error;
- the pairing.

**There is no `outbox_messages` table in the event database.** This amends ADR-0006's
enforcement bullet and ADR-0039 § 7. It keeps their purpose: nothing is lost across a restart,
and delivery never blocks timing. A restart recomputes what is owed by deriving again and
diffing against `ship.db`.

**3. ADR-0006's test becomes structural.** *"An event timed with sync disabled must produce
byte-identical exports to one timed with it enabled."* The shipper cannot write the database
the exports come from, so enabling it cannot change them.

**4. It derives only when something changed.** Every 10 s it reads `PRAGMA data_version`. When
that has moved, or a retry is due, it derives each published race, diffs it against `ship.db`,
and sends. Deriving holds every read, at about 0.5 KB each. So between changes it costs nothing,
and during a race it costs one derivation per interval with new reads. The 10 s is a starting
point for how live the board is, not a measurement.

**5. Its unit contains it.** It has its own cgroup with `MemoryMax=768M`. That is room for the
1M-read derivation measured above, with margin, and it is to be measured on the Pi. It also has
`Nice=10` and a low `CPUWeight=`. Running out of memory kills the shipper, not the timer, and
the timer's unit does not know the shipper exists. Network access is as ADR-0046 set out.

**6. The credential never touches the event database.** Pairing is a subcommand of the
shipper, `splitforge-ship pair`, run as its user. It replaces ADR-0039's
`splitforge raceday pair`. The token is stored in `ship.db` and nowhere else. So it is not in a
backup of the event database, not in a diagnostic bundle, and not readable by the `splitforge`
group.

**7. When the event database cannot be opened, the shipper waits.** That happens only when no
writer has it open, because SQLite removes `-wal` and `-shm` on the last close. During an event
the timer's service has it open all day. The shipper reports that it is waiting, and retries
each interval. Nothing is lost: it derives again when it can read. The operational rule that
follows is that **final results are published with the timer's service running**, which
`deployment.md` will say.

## Consequences

### What this makes easy

- **The timer is untouched.** It has no new table, no new write, no new lock holder, and no
  change to its unit. A shipper that hangs, leaks or crashes does so in its own cgroup.
- **Evidence cannot be altered by the integration**, and that is enforced by the kernel's file
  permissions, not by the code.
- **The credential's exposure is small.** One file, readable by one user, and in no artifact
  SplitForge produces.
- **Recovery is free.** A lost `ship.db` means everything is sent once more. RaceDay Connect
  takes each runner's crossings as a replacement and each revision idempotently, so a resend is
  harmless.

### What this makes hard

- **The shipper derives the whole race each time it derives.** At a million reads that is half a
  gigabyte and seconds of CPU on x86, per interval with new reads. Deriving incrementally would
  be an engine change, and is not this ADR's to make. The Pi measurement decides whether it is
  needed.
- **Two databases and two users to install.** `deployment.md` and the sysusers file gain the
  shipper's.
- **`doctor` cannot read `ship.db`.** It runs as `splitforge`. The shipper reports its own state
  with `splitforge-ship status`.

### What we accept

- **Nothing is sent while no process has the database open.** Keeping `-wal` and `-shm` in place
  after the last close would remove this. That needs `SQLITE_FCNTL_PERSIST_WAL`, which rusqlite
  0.40 reaches only through its `unsafe` raw handle, and `unsafe` is denied across the
  workspace. Revisit if rusqlite exposes it safely.
- **The shipper's view is as fresh as its interval.** It is up to 10 s behind the timer, which is
  live enough for a results board and nothing a result depends on.

## Alternatives considered

| Alternative | Why not |
|---|---|
| `outbox_messages` in the event database, as ADR-0006 planned | No command produces a message to write in the same transaction, and a second writer competes with the read path for the lock its appends wait on |
| The shipper as the `splitforge` user, writing its state into the event database | The same lock contention, and a bug in the integration could alter evidence |
| The shipper's state in the event database, written by the timer on its behalf | Puts the integration into the process ADR-0046 kept it out of |
| Make `/var/lib/splitforge` group-writable, so the shipper can create `-shm` | Write access to a directory is permission to delete the evidence in it |
| Open the database with `immutable=1` when `-shm` is absent | SQLite then skips locking. A writer starting mid-read would give the shipper a torn view, and it would send it |
| Derive on a timer regardless of change | A full derivation every interval, all day, for nothing when no reads arrived |
| Keep `splitforge raceday pair` in the CLI | The CLI runs as `splitforge`, so the token would sit where that user, and the event database's backups, can reach it |

## References

- [ADR-0006](0006-optional-outbound-integrations.md): outbound integrations are optional
- [ADR-0039](0039-raceday-connect-publishes-what-splitforge-derived.md): the translation and the delivery table
- [ADR-0046](0046-raceday-connect-is-reached-with-ureq-and-rustls-from-a-process-of-its-own.md): the client, and the shipper as its own process
- [deployment.md, who can do what](../deployment.md#who-can-do-what): the `splitforge` group's read access
- SQLite, [WAL mode on read-only databases](https://www.sqlite.org/wal.html#readonly)
