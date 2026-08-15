# ADR-0003: SQLite in WAL mode for local persistence

- **Status:** Accepted
- **Date:** 2026-08-14

## Context

SplitForge needs durable, transactional local storage on a single device that may lose
power at any moment. The requirements are unusual in their asymmetry: write volume is
modest (hundreds to low thousands of reads per event), but the cost of losing a single
write is unrecoverable, and there is no operator available to fix a database at 8 a.m.
on race morning.

There is exactly one writer, one machine, and no network. That eliminates most of the
reasons anyone reaches for a database server.

## Decision

Use **SQLite in WAL mode** as the local store, with `synchronous=FULL` on the raw-read
journal path.

- One database file per event, under `/var/lib/splitforge/`
- Migrations are versioned and forward-only, tracked in `schema_migrations`
- Backup is a first-class CLI command, plus an automatic pre-race snapshot
- A raw read is acknowledged only after its append returns

WAL mode is chosen because it lets readers (the API, exports, `reads tail`) run without
blocking the writer — and the writer is the one path that must never stall.

## Consequences

### What this makes easy

- No server to install, configure, secure, or restart. One file
- Real ACID transactions with well-understood crash semantics
- Backup and restore are file operations an operator can understand and verify
- Concurrent reads during an event do not contend with journal writes
- Trivially portable — an event database can be copied to a laptop for analysis

### What this makes hard

- Single-writer only. Multi-device checkpoints will need a different design (deliberately
  out of scope — see [roadmap](../roadmap.md))
- `synchronous=FULL` costs write latency and SD card wear. That is the correct trade for
  the journal, and needs measuring on hardware in Milestone 3
- WAL adds `-wal` and `-shm` sidecar files, which backup and restore must handle correctly
- SQLite corruption on failing SD media is real, and recovery strategy is unresolved —
  [Q7](../open-questions.md#q7-corruption-recovery-strategy)

### What we accept

That the durability guarantee is only as good as the storage hardware honoring `fsync`.
Consumer SD cards have been known to lie. This is why the mitigation stack does not stop
at the database: pre-race snapshots, periodic backups to separate media, and possibly a
plain-text write-ahead log alongside the database (Q7) all exist because we do not fully
trust the card.

## Alternatives considered

| Alternative | Why not |
|---|---|
| PostgreSQL | A server process, more RAM, more failure modes, more to go wrong unattended. Nothing about one reader on one Pi justifies it |
| Append-only flat file only | Excellent durability, terrible querying. Still attractive *alongside* SQLite as a corruption backstop |
| Embedded KV (sled, redb) | Reasonable durability, but results and reporting want real queries, and SQLite's crash semantics are far better documented and tested |
| In-memory + periodic flush | Fails the one requirement that matters: an acknowledged read must survive an unannounced power cut |

## References

- [timing-model.md](../timing-model.md)
- [ADR-0005](0005-raw-read-append-only-journal.md)
- [ADR-0009: `rusqlite` for SQLite access](0009-rusqlite-for-sqlite-access.md) — resolves the
  crate choice this ADR left open
