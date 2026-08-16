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
pub const SCHEMA_VERSION: i64 = 2;

/// Every migration, in order.
pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "initial_raw_read_journal",
        sql: INITIAL,
    },
    Migration {
        version: 2,
        name: "event_configuration",
        sql: EVENT_CONFIGURATION,
    },
];

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

// Event configuration: the tables that let an event be set up without touching the
// database directly (Milestone 2).
//
// The important distinction, and the reason this is a separate migration rather than more
// columns on an existing one: **configuration is mutable, evidence is not.** A roster gets
// re-imported when somebody registers late, a chip gets reassigned when one fails at the
// start line, a dedup window gets widened after the first wave. Freezing those would make
// the tool unusable.
//
// What makes that safe is not immutability but two other things: `audit_log` records every
// change that could move a published outcome, and a result revision snapshots the policy it
// used rather than pointing at a row that can later be edited (docs/timing-model.md § 7).
// So config answers "what is true now" and the append-only tables answer "what was true
// then" — and scoring only ever asks the second question.
const EVENT_CONFIGURATION: &str = r"
CREATE TABLE events (
    id             TEXT    PRIMARY KEY,
    name           TEXT    NOT NULL UNIQUE,
    created_at_us  INTEGER NOT NULL
) STRICT;

CREATE TABLE races (
    id                  TEXT    PRIMARY KEY,
    event_id            TEXT    NOT NULL REFERENCES events (id),
    name                TEXT    NOT NULL,
    scheduled_start_us  INTEGER,
    expected_laps       INTEGER NOT NULL,
    created_at_us       INTEGER NOT NULL,
    UNIQUE (event_id, name)
) STRICT;

CREATE TABLE checkpoints (
    id        TEXT    PRIMARY KEY,
    race_id   TEXT    NOT NULL REFERENCES races (id),
    name      TEXT    NOT NULL,
    kind      TEXT    NOT NULL,
    sequence  INTEGER NOT NULL,
    UNIQUE (race_id, name)
) STRICT;

CREATE TABLE participants (
    id       TEXT PRIMARY KEY,
    race_id  TEXT NOT NULL REFERENCES races (id),
    bib      TEXT NOT NULL,
    name     TEXT NOT NULL,
    UNIQUE (race_id, bib)
) STRICT;

-- Time-bounded, never a current-state lookup: a chip resolves to whoever was wearing it
-- *at the instant of the read*. Overlap is rejected in the domain rather than here,
-- because the operator needs to be told which chip and which two people, not `SQLITE_
-- CONSTRAINT`.
CREATE TABLE chip_assignments (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    chip            TEXT    NOT NULL,
    participant_id  TEXT    NOT NULL REFERENCES participants (id),
    race_id         TEXT    NOT NULL REFERENCES races (id),
    valid_from_us   INTEGER NOT NULL,
    valid_until_us  INTEGER
) STRICT;

CREATE INDEX chip_assignments_chip_idx ON chip_assignments (chip, valid_from_us);
CREATE INDEX chip_assignments_race_idx ON chip_assignments (race_id);

CREATE TABLE readers (
    id               TEXT    PRIMARY KEY,
    label            TEXT    NOT NULL,
    endpoint         TEXT,
    timestamp_trust  TEXT    NOT NULL,
    created_at_us    INTEGER NOT NULL
) STRICT;

CREATE TABLE reader_antennas (
    reader_id      TEXT    NOT NULL REFERENCES readers (id),
    antenna        INTEGER,
    checkpoint_id  TEXT    NOT NULL REFERENCES checkpoints (id)
) STRICT;

-- `antenna IS NULL` is the reader-wide wildcard. Two partial indexes rather than one
-- `UNIQUE (reader_id, antenna)`, because SQLite treats NULLs as distinct in a UNIQUE
-- constraint — a single constraint would happily accept two contradictory wildcards for
-- the same reader and then resolve crossings to whichever one the query planner reached
-- first.
CREATE UNIQUE INDEX reader_antennas_exact_idx
    ON reader_antennas (reader_id, antenna) WHERE antenna IS NOT NULL;
CREATE UNIQUE INDEX reader_antennas_wildcard_idx
    ON reader_antennas (reader_id) WHERE antenna IS NULL;

-- Stored as JSON rather than shredded into columns. The policy is snapshotted verbatim
-- into every result revision, so it has to round-trip exactly; a column layout would have
-- to be migrated in lockstep with `SelectionRule` and would make old revisions unreadable
-- the first time a rule gains a field.
CREATE TABLE timing_policies (
    race_id        TEXT    PRIMARY KEY REFERENCES races (id),
    policy_json    TEXT    NOT NULL,
    updated_at_us  INTEGER NOT NULL
) STRICT;

-- When the gun went off. Append-only: an operator typed it, which makes it evidence in
-- exactly the sense docs/timing-model.md § 6 gives manual entries.
--
-- Recording a start does *not* gate ingestion. Reads are persisted because a reader sent
-- them and the disk accepted them, full stop (docs/architecture.md § 1) — an operator who
-- forgets to type `race start` must not thereby lose evidence. The gun time is an input to
-- scoring, not a filter on evidence.
CREATE TABLE race_sessions (
    seq             INTEGER PRIMARY KEY AUTOINCREMENT,
    race_id         TEXT    NOT NULL REFERENCES races (id),
    action          TEXT    NOT NULL,
    at_us           INTEGER NOT NULL,
    actor           TEXT    NOT NULL,
    note            TEXT,
    recorded_at_us  INTEGER NOT NULL
) STRICT;

CREATE INDEX race_sessions_race_idx ON race_sessions (race_id, seq);

CREATE TRIGGER race_sessions_no_update
BEFORE UPDATE ON race_sessions
BEGIN
    SELECT RAISE(ABORT, 'race_sessions is append-only: UPDATE is not permitted');
END;

CREATE TRIGGER race_sessions_no_delete
BEFORE DELETE ON race_sessions
BEGIN
    SELECT RAISE(ABORT, 'race_sessions is append-only: DELETE is not permitted');
END;

-- Every operator action that could move a published outcome (docs/timing-model.md § 8).
-- The test this table has to pass: given a finished event and a disputed result, can the
-- database alone explain why that participant got that time, and what changed between the
-- provisional and final results?
CREATE TABLE audit_log (
    seq             INTEGER PRIMARY KEY AUTOINCREMENT,
    at_us           INTEGER NOT NULL,
    actor           TEXT    NOT NULL,
    action          TEXT    NOT NULL,
    subject         TEXT,
    detail_json     TEXT,
    recorded_at_us  INTEGER NOT NULL
) STRICT;

CREATE INDEX audit_log_action_idx ON audit_log (action, seq);

CREATE TRIGGER audit_log_no_update
BEFORE UPDATE ON audit_log
BEGIN
    SELECT RAISE(ABORT, 'audit_log is append-only: UPDATE is not permitted');
END;

CREATE TRIGGER audit_log_no_delete
BEFORE DELETE ON audit_log
BEGIN
    SELECT RAISE(ABORT, 'audit_log is append-only: DELETE is not permitted');
END;
";
