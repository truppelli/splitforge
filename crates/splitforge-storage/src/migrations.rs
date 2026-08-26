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
pub const SCHEMA_VERSION: i64 = 6;

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
    Migration {
        version: 3,
        name: "results_and_revisions",
        sql: RESULTS_AND_REVISIONS,
    },
    Migration {
        version: 4,
        name: "device_settings",
        sql: DEVICE_SETTINGS,
    },
    Migration {
        version: 5,
        name: "clock_steps",
        sql: CLOCK_STEPS,
    },
    Migration {
        version: 6,
        name: "manual_entries",
        sql: MANUAL_ENTRIES,
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

// Results (Milestone 4). Two kinds of record here, and the difference is the whole design.
//
// `status_declarations` is **evidence**: an operator decided something. SplitForge cannot
// derive a disqualification — no arrangement of chip reads implies "cut the course" — so a
// DQ enters the system the same way a manual entry does, as a record that somebody said so.
// Reversing one appends a newer declaration; the original stays exactly where it is, because
// "disqualified at 11:04, reinstated at 11:31" is the true account and an organizer may have
// to defend it.
//
// `result_revisions` and `result_entries` are **frozen derivations**: computed from evidence,
// but immutable once published, because publication is an event in the world. Someone
// screenshotted revision 1. Superseding it is honest; rewriting it is not
// (docs/timing-model.md § 7).
//
// Both get append-only triggers, for different reasons: evidence because it is evidence,
// revisions because they were published.
const RESULTS_AND_REVISIONS: &str = r"
-- Which clock placement is measured from. Race configuration, snapshotted into every
-- revision (docs/timing-model.md § 7) — so changing it later cannot retroactively restate
-- what an already-published sheet meant.
--
-- A column rather than a field inside `policy_json`, because `TimingPolicy` is what the
-- engine uses to turn reads into crossings, and the engine has no business knowing that
-- start modes exist.
ALTER TABLE timing_policies ADD COLUMN start_mode TEXT NOT NULL DEFAULT 'gun';

CREATE TABLE status_declarations (
    seq             INTEGER PRIMARY KEY AUTOINCREMENT,
    id              TEXT    NOT NULL UNIQUE,
    race_id         TEXT    NOT NULL REFERENCES races (id),
    participant_id  TEXT    NOT NULL REFERENCES participants (id),
    status          TEXT    NOT NULL,
    reason          TEXT    NOT NULL,
    actor           TEXT    NOT NULL,
    declared_at_us  INTEGER NOT NULL,
    recorded_at_us  INTEGER NOT NULL
) STRICT;

-- Scoring asks for the newest declaration per participant, which is this index read
-- backwards.
CREATE INDEX status_declarations_race_idx
    ON status_declarations (race_id, participant_id, seq);

CREATE TRIGGER status_declarations_no_update
BEFORE UPDATE ON status_declarations
BEGIN
    SELECT RAISE(ABORT, 'status_declarations is append-only: UPDATE is not permitted');
END;

CREATE TRIGGER status_declarations_no_delete
BEFORE DELETE ON status_declarations
BEGIN
    SELECT RAISE(ABORT, 'status_declarations is append-only: DELETE is not permitted');
END;

-- `number` is monotonic within a race and unique, so 'revision 2' names one thing forever.
-- `digest` fingerprints the scored rows only, which is what makes 'this republish changed
-- nothing' a detectable fact rather than an opinion.
CREATE TABLE result_revisions (
    id               TEXT    PRIMARY KEY,
    race_id          TEXT    NOT NULL REFERENCES races (id),
    number           INTEGER NOT NULL,
    status           TEXT    NOT NULL,
    generated_at_us  INTEGER NOT NULL,
    actor            TEXT    NOT NULL,
    reason           TEXT    NOT NULL,
    policy_json      TEXT    NOT NULL,
    digest           TEXT    NOT NULL,
    recorded_at_us   INTEGER NOT NULL,
    UNIQUE (race_id, number)
) STRICT;

-- Bib and name are denormalized on purpose. A revision has to stay readable exactly as
-- published, and a roster re-import that corrects a spelling must not retroactively edit
-- what a published results sheet said.
--
-- `flags_json` and `timing_events_json` are JSON arrays for the same reason `policy_json`
-- is: they round-trip verbatim, and a column layout would have to migrate in lockstep with
-- the enum, making old revisions unreadable the first time a variant gains a field.
CREATE TABLE result_entries (
    revision_id      TEXT    NOT NULL REFERENCES result_revisions (id),
    participant_id   TEXT    NOT NULL REFERENCES participants (id),
    ordinal          INTEGER NOT NULL,
    bib              TEXT    NOT NULL,
    name             TEXT    NOT NULL,
    status           TEXT    NOT NULL,
    status_source    TEXT    NOT NULL,
    status_reason    TEXT,
    start_at_us      INTEGER,
    finish_at_us     INTEGER,
    gun_time_ms      INTEGER,
    chip_time_ms     INTEGER,
    scoring_time_ms  INTEGER,
    place            INTEGER,
    flags_json       TEXT    NOT NULL,
    timing_events_json TEXT  NOT NULL,
    PRIMARY KEY (revision_id, participant_id)
) STRICT;

CREATE INDEX result_entries_revision_idx ON result_entries (revision_id, ordinal);

CREATE TRIGGER result_revisions_no_update
BEFORE UPDATE ON result_revisions
BEGIN
    SELECT RAISE(ABORT, 'result_revisions is immutable: publish a new revision instead');
END;

CREATE TRIGGER result_revisions_no_delete
BEFORE DELETE ON result_revisions
BEGIN
    SELECT RAISE(ABORT, 'result_revisions is immutable: DELETE is not permitted');
END;

CREATE TRIGGER result_entries_no_update
BEFORE UPDATE ON result_entries
BEGIN
    SELECT RAISE(ABORT, 'result_entries is immutable: publish a new revision instead');
END;

CREATE TRIGGER result_entries_no_delete
BEFORE DELETE ON result_entries
BEGIN
    SELECT RAISE(ABORT, 'result_entries is immutable: DELETE is not permitted');
END;
";

const DEVICE_SETTINGS: &str = r"
-- Settings that belong to the *device* rather than to any race: the Pi this binary runs
-- on, not the event it happens to be timing. A free-space floor is a property of the SD
-- card; it does not become a different number because the 10K starts.
--
-- Mutable, and deliberately so (ADR-0014). This is configuration, not evidence, so it
-- carries no append-only triggers — the audit log records who changed it and to what,
-- which is where that history belongs.
--
-- Key/value rather than a column per setting: the alternative is a migration every time a
-- device-level knob appears, and the ones already visible on the roadmap (reader clock
-- trust defaults, alarm thresholds) are exactly that shape.
CREATE TABLE device_settings (
    key            TEXT    PRIMARY KEY,
    value          TEXT    NOT NULL,
    updated_at_us  INTEGER NOT NULL
) STRICT;
";

const CLOCK_STEPS: &str = r"
-- Wall-clock discontinuities, observed by comparing the wall clock against the monotonic
-- clock (docs/clock-and-time-discipline.md 10).
--
-- Evidence, so append-only like raw_reads (ADR-0005, ADR-0011). The device recording that
-- its own clock jumped is exactly the kind of fact somebody wants to dispute after a
-- result is questioned, and a row that can be edited answers nothing.
--
-- `seq` is the only ordering in this table that can be trusted, which is the point: every
-- other column here is a timestamp taken from the clock under suspicion.
CREATE TABLE clock_steps (
    seq                 INTEGER PRIMARY KEY AUTOINCREMENT,
    recorded_at_us      INTEGER NOT NULL,
    observed_before_us  INTEGER NOT NULL,
    observed_after_us   INTEGER NOT NULL,
    monotonic_ms        INTEGER NOT NULL,
    step_ms             INTEGER NOT NULL
) STRICT;

CREATE TRIGGER clock_steps_no_update
BEFORE UPDATE ON clock_steps
BEGIN
    SELECT RAISE(ABORT, 'clock_steps is append-only: UPDATE is not permitted');
END;

CREATE TRIGGER clock_steps_no_delete
BEFORE DELETE ON clock_steps
BEGIN
    SELECT RAISE(ABORT, 'clock_steps is append-only: DELETE is not permitted');
END;
";

const MANUAL_ENTRIES: &str = r"
-- What an operator writes down when the chip does not
-- (docs/timing-model.md 6).
--
-- Evidence, so append-only like raw_reads (ADR-0005, ADR-0011). An official writing a bib
-- on a clipboard at the finish because a chip failed is producing a primary record, and it
-- earns the same immutability as a reader report. A mistaken entry is corrected by
-- recording the correction, never by editing this row.
--
-- `at_us` is what the operator *claims*; `recorded_at_us` is when they typed it. Those can
-- be an hour apart, and a dispute needs both.
--
-- Deliberately not a result override. Results are derived and re-deriving must reproduce
-- them (ADR-0014), so the operator's claim has to enter as an input to derivation rather
-- than as an edit to its output.
CREATE TABLE manual_entries (
    seq             INTEGER PRIMARY KEY AUTOINCREMENT,
    id              TEXT    NOT NULL UNIQUE,
    race_id         TEXT    NOT NULL REFERENCES races(id),
    participant_id  TEXT    NOT NULL REFERENCES participants(id),
    checkpoint_id   TEXT    NOT NULL REFERENCES checkpoints(id),
    at_us           INTEGER NOT NULL,
    actor           TEXT    NOT NULL,
    reason          TEXT    NOT NULL,
    recorded_at_us  INTEGER NOT NULL
) STRICT;

CREATE INDEX manual_entries_race_idx ON manual_entries (race_id, at_us);

CREATE TRIGGER manual_entries_no_update
BEFORE UPDATE ON manual_entries
BEGIN
    SELECT RAISE(ABORT, 'manual_entries is append-only: UPDATE is not permitted');
END;

CREATE TRIGGER manual_entries_no_delete
BEFORE DELETE ON manual_entries
BEGIN
    SELECT RAISE(ABORT, 'manual_entries is append-only: DELETE is not permitted');
END;
";

#[cfg(test)]
mod tests {
    use super::{MIGRATIONS, SCHEMA_VERSION};

    #[test]
    fn the_schema_version_is_the_last_migration() {
        // The failure this catches is bumping one without the other. A constant that runs
        // ahead of the migrations makes every database look like it came from a newer
        // build and be refused; a constant that lags leaves the newest table unmigrated on
        // a device that already has it.
        let last = MIGRATIONS.last().expect("at least one migration");
        assert_eq!(
            SCHEMA_VERSION, last.version,
            "SCHEMA_VERSION is {SCHEMA_VERSION} but the last migration is {} ({})",
            last.version, last.name
        );
    }

    #[test]
    fn migrations_are_numbered_from_one_without_gaps() {
        // Applied in ascending order and never re-applied, so a gap or a repeat silently
        // changes which SQL a given database has run.
        for (index, migration) in MIGRATIONS.iter().enumerate() {
            let expected = i64::try_from(index + 1).expect("a small index");
            assert_eq!(
                migration.version, expected,
                "migration {:?} is numbered {} but sits at position {expected}",
                migration.name, migration.version
            );
        }
    }

    #[test]
    fn every_migration_has_a_distinct_name() {
        // The name is what `schema_migrations` records, and it is what somebody reads when
        // they are working out which version a device in a field is on.
        let mut names: Vec<&str> = MIGRATIONS.iter().map(|migration| migration.name).collect();
        names.sort_unstable();
        let total = names.len();
        names.dedup();
        assert_eq!(names.len(), total, "two migrations share a name");
    }
}
