//! Schema migrations: versioned, forward-only.
//!
//! Migrations are embedded in the binary rather than shipped as loose files. A Pi in a
//! field cannot be told to "run the migration script" — whatever binary is installed must
//! carry everything it needs to bring a database to the schema it expects.

/// One forward-only schema migration.
#[derive(Debug, Clone, Copy)]
pub struct Migration {
    /// Sequential version. Applied in ascending order, never re-applied.
    pub version: i64,
    /// Short human-readable name, recorded in `schema_migrations`.
    pub name: &'static str,
    /// The SQL to execute. Runs inside a transaction with the version record.
    pub sql: &'static str,
}

/// The schema version this build expects.
pub const SCHEMA_VERSION: i64 = 1;

/// Every migration, in order.
pub const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    name: "initial_raw_read_journal",
    sql: INITIAL,
}];

const INITIAL: &str = r"
CREATE TABLE schema_migrations (
    version        INTEGER PRIMARY KEY,
    name           TEXT    NOT NULL,
    applied_at_us  INTEGER NOT NULL
) STRICT;

-- The journal. Append-only evidence (ADR-0005).
--
-- `seq` is AUTOINCREMENT rather than plain rowid so identifiers are never reused after a
-- delete. Nothing should ever be deleted, but a monotonic sequence that cannot silently
-- restart is what makes 'did we lose a read?' answerable independently of any clock.
CREATE TABLE raw_reads (
    seq                        INTEGER PRIMARY KEY AUTOINCREMENT,
    id                         TEXT    NOT NULL UNIQUE,
    source                     TEXT    NOT NULL,
    antenna                    INTEGER,
    chip                       TEXT    NOT NULL,
    reader_timestamp_us        INTEGER,
    reader_uptime_us           INTEGER,
    received_at_us             INTEGER NOT NULL,
    received_at_monotonic_ns   INTEGER,
    rssi_dbm                   INTEGER,
    device_clock_state         TEXT    NOT NULL,
    timestamp_source           TEXT    NOT NULL,
    timestamp_fallback_reason  TEXT,
    clock_offset_ms            INTEGER,
    raw_payload                BLOB    NOT NULL,
    payload_sha256             TEXT    NOT NULL,
    recorded_at_us             INTEGER NOT NULL
) STRICT;

CREATE INDEX raw_reads_chip_time_idx   ON raw_reads (chip, received_at_us);
CREATE INDEX raw_reads_source_time_idx ON raw_reads (source, received_at_us);

-- Append-only enforced at the database, not by convention (open question Q8, resolved
-- here in favour of triggers). Application code has no UPDATE or DELETE path for this
-- table; these stop the accident, which is the realistic threat.
CREATE TRIGGER raw_reads_no_update
BEFORE UPDATE ON raw_reads
BEGIN
    SELECT RAISE(ABORT, 'raw_reads is append-only: UPDATE is not permitted');
END;

CREATE TRIGGER raw_reads_no_delete
BEFORE DELETE ON raw_reads
BEGIN
    SELECT RAISE(ABORT, 'raw_reads is append-only: DELETE is not permitted');
END;
";
