//! Opening, configuring, and migrating the event database.
//!
//! Two types now hold connections to the same file — [`crate::SqliteJournal`] for evidence
//! and [`crate::ConfigStore`] for configuration — and they must agree exactly on pragmas
//! and schema version. Sharing one open path is what stops "the journal set
//! `synchronous = FULL` but the config store didn't" from ever being a sentence anyone has
//! to debug at a race.
//!
//! Separate connections rather than a shared one, deliberately. WAL lets a reader and a
//! writer coexist, the durability contract belongs to the journal alone, and a shared
//! `&mut Connection` would put the config store in the transaction path of the read loop.

use std::path::Path;
use std::time::Duration as StdDuration;

use rusqlite::{Connection, OptionalExtension, params};
use time::OffsetDateTime;

use crate::StorageError;
use crate::migrations::{MIGRATIONS, SCHEMA_VERSION};

/// Opens (creating if absent) the event database at `path`, applying pending migrations.
///
/// # Errors
///
/// Returns [`StorageError`] if the file cannot be opened, the pragmas cannot be set, or a
/// migration fails.
pub(crate) fn open(path: impl AsRef<Path>) -> Result<Connection, StorageError> {
    prepare(Connection::open(path)?)
}

/// Opens a private in-memory database. For tests and dry runs only — nothing survives.
///
/// # Errors
///
/// Returns [`StorageError`] if the database cannot be created or migrated.
pub(crate) fn open_in_memory() -> Result<Connection, StorageError> {
    prepare(Connection::open_in_memory()?)
}

fn prepare(mut conn: Connection) -> Result<Connection, StorageError> {
    configure(&conn)?;
    migrate(&mut conn)?;
    Ok(conn)
}

/// The schema version currently applied to a database.
///
/// # Errors
///
/// Returns [`StorageError`] if the migration table cannot be read.
pub(crate) fn schema_version(conn: &Connection) -> Result<i64, StorageError> {
    Ok(conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| row.get(0),
    )?)
}

fn configure(conn: &Connection) -> Result<(), StorageError> {
    conn.busy_timeout(StdDuration::from_secs(5))?;

    // WAL so exports and `reads tail` never block the writer. An in-memory database
    // reports "memory" instead and that is fine — nothing durable is expected of it.
    //
    // Read the current mode first and only set it if it needs setting. `PRAGMA
    // journal_mode = WAL` takes an exclusive lock, and — unlike almost everything else in
    // SQLite — it does *not* invoke the busy handler, so `busy_timeout` above does not
    // cover it. Setting it unconditionally therefore makes opening a second connection
    // fail intermittently while a write is in flight, which is precisely the moment an
    // operator runs `reads tail` to check the timer is alive.
    let mode: String = conn.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") && !mode.eq_ignore_ascii_case("memory") {
        let _mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
    }
    // FULL, not NORMAL: an acknowledged read must survive an unannounced power cut. This
    // costs write latency and SD card wear, and both are the correct trade here.
    conn.pragma_update(None, "synchronous", "FULL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(())
}

fn migrate(conn: &mut Connection) -> Result<(), StorageError> {
    let has_migrations_table = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'schema_migrations'",
            [],
            |_| Ok(true),
        )
        .optional()?
        .unwrap_or(false);

    let current: i64 = if has_migrations_table {
        schema_version(conn)?
    } else {
        0
    };

    if current > SCHEMA_VERSION {
        // Refusing to run beats silently misreading a schema we do not understand.
        return Err(StorageError::SchemaTooNew {
            found: current,
            supported: SCHEMA_VERSION,
        });
    }

    for migration in MIGRATIONS.iter().filter(|m| m.version > current) {
        let tx = conn.transaction()?;
        tx.execute_batch(migration.sql)?;
        tx.execute(
            "INSERT INTO schema_migrations (version, name, applied_at_us) VALUES (?1, ?2, ?3)",
            params![
                migration.version,
                migration.name,
                to_micros(OffsetDateTime::now_utc())?
            ],
        )?;
        tx.commit()?;
    }
    Ok(())
}

/// Microseconds since the Unix epoch. Integers sort correctly, survive round-tripping
/// exactly, and match LLRP's native resolution.
pub(crate) fn to_micros(value: OffsetDateTime) -> Result<i64, StorageError> {
    // `div_euclid` rather than `/` so pre-epoch instants floor rather than truncate
    // toward zero. Not expected in a race, but silent asymmetry around a boundary is
    // exactly the kind of thing that surfaces once and is never explained.
    i64::try_from(value.unix_timestamp_nanos().div_euclid(1_000))
        .map_err(|_| StorageError::TimestampOutOfRange)
}

/// The inverse of [`to_micros`].
pub(crate) fn from_micros(micros: i64) -> Result<OffsetDateTime, StorageError> {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(micros) * 1_000)
        .map_err(|_| StorageError::TimestampOutOfRange)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use time::Duration;

    #[test]
    fn timestamps_survive_microsecond_round_trips() {
        let value = OffsetDateTime::UNIX_EPOCH + Duration::nanoseconds(1_700_000_000_123_456_000);
        let micros = to_micros(value).expect("to micros");
        assert_eq!(from_micros(micros).expect("from micros"), value);
    }

    #[test]
    fn migrations_are_idempotent_across_reopens() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("event.db");

        let first = open(&path).expect("open");
        assert_eq!(schema_version(&first).expect("version"), SCHEMA_VERSION);
        drop(first);

        let second = open(&path).expect("reopen");
        assert_eq!(schema_version(&second).expect("version"), SCHEMA_VERSION);
    }

    #[test]
    fn a_database_from_a_newer_build_is_refused_rather_than_misread() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("event.db");
        {
            let conn = open(&path).expect("open");
            conn.execute(
                "INSERT INTO schema_migrations (version, name, applied_at_us) \
                 VALUES (999, 'from the future', 0)",
                [],
            )
            .expect("insert future migration");
        }

        let error = open(&path).expect_err("must refuse a newer schema");
        assert!(matches!(
            error,
            StorageError::SchemaTooNew { found: 999, .. }
        ));
    }

    #[test]
    fn a_v1_database_migrates_forward_without_losing_its_reads() {
        // The realistic upgrade: a Pi that timed an event on the Milestone 1 binary and is
        // then handed a Milestone 2 one. The journal must survive untouched.
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("event.db");

        {
            let mut conn = Connection::open(&path).expect("open");
            configure(&conn).expect("configure");
            let tx = conn.transaction().expect("tx");
            tx.execute_batch(MIGRATIONS[0].sql).expect("v1 schema");
            tx.execute(
                "INSERT INTO schema_migrations (version, name, applied_at_us) VALUES (1, ?1, 0)",
                params![MIGRATIONS[0].name],
            )
            .expect("record v1");
            tx.execute(
                "INSERT INTO raw_reads (
                    id, source, chip, received_at_us, device_clock_state,
                    timestamp_source, raw_payload, payload_sha256, recorded_at_us
                 ) VALUES ('11111111-1111-4111-8111-111111111111', 'mat', 'E280', 0,
                           'gps_locked', 'reader_utc', x'00', 'abc', 0)",
                [],
            )
            .expect("seed a read");
            tx.commit().expect("commit");
        }

        let conn = open(&path).expect("migrate forward");
        assert_eq!(schema_version(&conn).expect("version"), SCHEMA_VERSION);

        let reads: i64 = conn
            .query_row("SELECT COUNT(*) FROM raw_reads", [], |row| row.get(0))
            .expect("count reads");
        assert_eq!(reads, 1, "migrating must never touch the evidence");

        let events: i64 = conn
            .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
            .expect("events table exists");
        assert_eq!(events, 0);
    }

    #[test]
    fn a_reader_cannot_hold_two_contradictory_wildcards() {
        // SQLite counts NULLs as distinct in a UNIQUE constraint, so this needs a partial
        // index to catch. Without it, crossings would resolve to whichever row the query
        // planner happened to reach first.
        let conn = open_in_memory().expect("open");
        conn.execute_batch(
            "INSERT INTO events (id, name, created_at_us) VALUES ('e', 'Day', 0);
             INSERT INTO races (id, event_id, name, expected_laps, created_at_us)
                 VALUES ('r', 'e', '5K', 1, 0);
             INSERT INTO checkpoints (id, race_id, name, kind, sequence)
                 VALUES ('c1', 'r', 'start', 'start', 0);
             INSERT INTO checkpoints (id, race_id, name, kind, sequence)
                 VALUES ('c2', 'r', 'finish', 'finish', 1);
             INSERT INTO readers (id, label, timestamp_trust, created_at_us)
                 VALUES ('mat', 'Mat', 'auto', 0);
             INSERT INTO reader_antennas (reader_id, antenna, checkpoint_id)
                 VALUES ('mat', NULL, 'c1');",
        )
        .expect("seed");

        conn.execute(
            "INSERT INTO reader_antennas (reader_id, antenna, checkpoint_id)
             VALUES ('mat', NULL, 'c2')",
            [],
        )
        .expect_err("a second wildcard for the same reader must be refused");
    }

    #[test]
    fn the_audit_log_cannot_be_rewritten() {
        let conn = open_in_memory().expect("open");
        conn.execute(
            "INSERT INTO audit_log (at_us, actor, action, recorded_at_us)
             VALUES (0, 'phillip', 'roster.import', 0)",
            [],
        )
        .expect("append");

        let error = conn
            .execute("UPDATE audit_log SET actor = 'somebody else'", [])
            .expect_err("the audit trail must not be editable");
        assert!(error.to_string().contains("append-only"), "got: {error}");

        let error = conn
            .execute("DELETE FROM audit_log", [])
            .expect_err("the audit trail must not be deletable");
        assert!(error.to_string().contains("append-only"), "got: {error}");
    }

    #[test]
    fn a_recorded_gun_time_cannot_be_rewritten() {
        let conn = open_in_memory().expect("open");
        conn.execute_batch(
            "INSERT INTO events (id, name, created_at_us) VALUES ('e', 'Day', 0);
             INSERT INTO races (id, event_id, name, expected_laps, created_at_us)
                 VALUES ('r', 'e', '5K', 1, 0);
             INSERT INTO race_sessions (race_id, action, at_us, actor, recorded_at_us)
                 VALUES ('r', 'start', 1000, 'phillip', 1000);",
        )
        .expect("seed");

        let error = conn
            .execute("UPDATE race_sessions SET at_us = 0", [])
            .expect_err("a published gun time must not be editable");
        assert!(error.to_string().contains("append-only"), "got: {error}");
    }
}
