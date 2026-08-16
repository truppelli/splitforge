//! Backups.
//!
//! `VACUUM INTO` rather than a file copy. Copying a live SQLite database is a well-known
//! way to produce a file that looks fine and is not: the `-wal` sidecar holds committed
//! pages the main file has not absorbed yet, so a copy taken mid-race can be missing the
//! most recent reads — exactly the ones somebody is asking about.
//!
//! `VACUUM INTO` runs inside a read transaction, so it sees a consistent snapshot including
//! the WAL, and it can run **while the race is running**. A backup that requires stopping
//! the timer is a backup nobody takes.
//!
//! Restore is deliberately not here. Restoring is a Milestone 5 concern and a rehearsed
//! procedure, not a subcommand — see the roadmap.

use std::path::Path;

use rusqlite::Connection;

use crate::StorageError;
use crate::config::ConfigStore;

/// What a backup produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupReport {
    /// Where the snapshot was written.
    pub path: String,
    /// Size of the snapshot in bytes.
    pub bytes: u64,
    /// Raw reads captured in it.
    ///
    /// The number worth checking: a backup with fewer reads than the journal had is a
    /// backup that was taken against the wrong file.
    pub raw_reads: u64,
}

/// Writes a consistent snapshot of `store`'s database to `destination`.
///
/// # Errors
///
/// Returns [`StorageError`] if the destination already exists, cannot be written, or the
/// snapshot cannot be verified afterwards.
pub fn create(
    store: &ConfigStore,
    destination: impl AsRef<Path>,
) -> Result<BackupReport, StorageError> {
    let destination = destination.as_ref();

    // `VACUUM INTO` refuses to overwrite, and so do we — but failing here gives the
    // operator the path in the message instead of SQLite's generic complaint.
    if destination.exists() {
        return Err(StorageError::Decode(format!(
            "{} already exists; backups never overwrite",
            destination.display()
        )));
    }

    let path = destination.to_string_lossy().replace('\'', "''");
    store
        .connection()
        .execute_batch(&format!("VACUUM INTO '{path}'"))?;

    // Open the snapshot and count what landed in it. A backup nobody verified is a backup
    // whose failure is discovered during a restore, which is the worst possible moment.
    let snapshot = Connection::open(destination)?;
    let raw_reads: i64 =
        snapshot.query_row("SELECT COUNT(*) FROM raw_reads", [], |row| row.get(0))?;

    let bytes = std::fs::metadata(destination)
        .map_err(|error| StorageError::Decode(format!("stat {}: {error}", destination.display())))?
        .len();

    Ok(BackupReport {
        path: destination.display().to_string(),
        bytes,
        raw_reads: u64::try_from(raw_reads).unwrap_or(0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SqliteJournal;
    use splitforge_domain::{
        ChipId, DeviceClockState, RawRead, RawReadId, RawReadJournal, ReaderId, TimestampSource,
    };
    use tempfile::tempdir;
    use time::{Duration, OffsetDateTime};

    fn read(chip: &str) -> RawRead {
        RawRead {
            id: RawReadId::new(),
            source: ReaderId::new("mat"),
            antenna: Some(1),
            chip: ChipId::new(chip),
            reader_timestamp: None,
            reader_uptime_us: None,
            received_at: OffsetDateTime::UNIX_EPOCH + Duration::seconds(1_700_000_000),
            received_at_monotonic_ns: None,
            rssi_dbm: Some(-50),
            device_clock_state: DeviceClockState::GpsLocked,
            timestamp_source: TimestampSource::DeviceReceipt {
                reason: splitforge_domain::FallbackReason::NoReaderTimestamp,
            },
            clock_offset_ms: None,
            raw_payload: vec![1, 2, 3],
        }
    }

    #[test]
    fn a_backup_captures_every_read_and_can_be_reopened() {
        let dir = tempdir().expect("tempdir");
        let live = dir.path().join("event.db");
        let snapshot = dir.path().join("backup.db");

        {
            let mut journal = SqliteJournal::open(&live).expect("open journal");
            journal
                .append_batch(&[read("A"), read("B"), read("C")])
                .expect("append");
        }

        let store = ConfigStore::open(&live).expect("open store");
        let report = create(&store, &snapshot).expect("backup");

        assert_eq!(report.raw_reads, 3);
        assert!(report.bytes > 0);

        // The real test: the snapshot is a working database, not just a file of the right
        // size. Opening it also migrates it, which would fail on a torn copy.
        let reopened = SqliteJournal::open(&snapshot).expect("reopen snapshot");
        assert_eq!(reopened.count().expect("count"), 3);
    }

    #[test]
    fn a_backup_taken_while_the_journal_is_open_for_writing_is_still_consistent() {
        // The operational case: `backup create` run from a second terminal mid-race.
        let dir = tempdir().expect("tempdir");
        let live = dir.path().join("event.db");
        let snapshot = dir.path().join("backup.db");

        let mut journal = SqliteJournal::open(&live).expect("open journal");
        journal
            .append_batch(&[read("A"), read("B")])
            .expect("append");

        let store = ConfigStore::open(&live).expect("open store");
        let report = create(&store, &snapshot).expect("backup while writer is open");
        assert_eq!(report.raw_reads, 2);

        // The writer keeps going afterwards, and the snapshot does not follow it.
        journal.append(&read("C")).expect("append after backup");
        assert_eq!(journal.count().expect("count"), 3);

        let reopened = SqliteJournal::open(&snapshot).expect("reopen");
        assert_eq!(reopened.count().expect("count"), 2);
    }

    #[test]
    fn a_backup_never_overwrites() {
        let dir = tempdir().expect("tempdir");
        let live = dir.path().join("event.db");
        let snapshot = dir.path().join("backup.db");

        let store = ConfigStore::open(&live).expect("open");
        create(&store, &snapshot).expect("first backup");

        let error = create(&store, &snapshot).expect_err("a second backup must refuse");
        assert!(error.to_string().contains("already exists"), "got: {error}");
    }
}
