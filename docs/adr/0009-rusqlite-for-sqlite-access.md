# ADR-0009: `rusqlite` for SQLite access

- **Status:** Accepted
- **Date:** 2026-08-14
- **Resolves:** Q1

## Context

[ADR-0003](0003-sqlite-wal-local-persistence.md) settled on SQLite in WAL mode. It did not
settle on how Rust talks to it, and the two realistic answers pull in opposite directions.

`sqlx` offers compile-time-checked queries and an async API that matches the Tokio service
the rest of SplitForge is built on. `rusqlite` is a thin synchronous binding over the C
library with essentially no machinery between the call and the write.

The deciding constraint is what the read path actually does. It executes one small
`INSERT` per read and must return only when that insert is durable — the whole durability
contract in [ADR-0005](0005-raw-read-append-only-journal.md) rests on knowing exactly when
the fsync happened. SQLite writes are synchronous at the C level regardless of what the
Rust wrapper presents; an async facade over a blocking call does not make it concurrent, it
makes it harder to see where the process is actually blocked.

The second constraint is deployment. The Pi 3 is the target, cross-compiled from an
`aarch64-unknown-linux-gnu` build. Whatever is chosen has to build there without a working
database to introspect at compile time and without a network.

## Decision

**SplitForge uses `rusqlite` with the `bundled` feature.**

- Bundled, not system: the Pi's SQLite version stops being a deployment variable, and the
  `STRICT` tables and trigger behavior the schema depends on are guaranteed present
- Journal writes are synchronous calls, and stay that way. When the edge service needs them
  off the async executor, they move to a blocking thread — an explicit, visible boundary
  rather than an implicit one
- Connection configuration (`journal_mode = WAL`, `synchronous = FULL`, `foreign_keys`,
  `busy_timeout`) is applied at open, in one place, and is part of the durability contract
  rather than a deployment detail
- Migrations are embedded `&'static str` SQL applied in a transaction with their version
  record. A Pi in a field cannot be told to "run the migration script"

## Consequences

### What this makes easy

- The durability contract is legible: the append returns after the transaction commits, and
  nothing sits between that statement and the C call
- Cross-compilation is self-contained — no database server, no offline query cache, no
  `DATABASE_URL` in CI
- Behavior is identical on a developer laptop and on the Pi, because it is literally the
  same amalgamation

### What this makes hard

- Queries are strings checked at runtime. A typo in a column name is a test failure, not a
  compile error
- Row decoding is hand-written, so every schema change touches mapping code
- Bundled SQLite means SplitForge owns the update cadence for a C dependency, including its
  CVEs. `cargo audit` and `cargo deny` in CI are load-bearing because of this choice
- Longer clean builds: the amalgamation is compiled from source

### What we accept

Runtime-checked SQL, in exchange for a read path with nothing hidden in it. The mitigation
is coverage rather than types: the storage crate's tests exercise every column through a
full round trip, so a mapping error fails immediately rather than at a race.

We also accept compiling a C dependency, which is the price of not caring what the host
system ships.

## Alternatives considered

| Alternative | Why not |
|---|---|
| `sqlx` with SQLite | Compile-time query checking is genuinely valuable, and it is most valuable in the results and reporting code that does not exist yet. It buys least where the risk is highest — the single hot insert on the read path — while adding an async layer over a call that is synchronous underneath |
| `rusqlite` against system SQLite | Makes the SQLite version a property of the deployed image. `STRICT` tables need 3.37+, and "which SQLite does this Pi have?" is not a question worth answering at 6 a.m. |
| A hand-rolled binding | No. |
| Revisit for the results layer | Left open deliberately. If compile-time checking proves worth it for reporting, that is a new ADR, not a reason to complicate the journal now |

## References

- [ADR-0003: SQLite in WAL mode](0003-sqlite-wal-local-persistence.md)
- [ADR-0005: Raw reads are an append-only journal](0005-raw-read-append-only-journal.md)
- [architecture.md § 7](../architecture.md#7-technology-choices)
