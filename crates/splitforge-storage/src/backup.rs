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
//! ## Restore
//!
//! [`restore`] is the other half, and it is the half that matters: a backup nobody has
//! ever restored is a hypothesis. It never deletes — a displaced database is renamed, not
//! removed, because the file you are replacing during an incident is the file whose
//! contents you may need to explain afterwards.
//!
//! It also never touches the sidecar ([`crate::sidecar`]). That is deliberate and it is
//! the point of the pairing: the snapshot brings back the configuration as of the last
//! backup, and the sidecar — untouched, holding every read including the ones written
//! after that backup — brings back the evidence. Moving the sidecar aside during a restore
//! would throw away the only copy of exactly the reads the snapshot is missing.

use std::path::{Path, PathBuf};

use rusqlite::Connection;
use time::OffsetDateTime;

use crate::StorageError;
use crate::config::ConfigStore;
use crate::migrations::SCHEMA_VERSION;
use crate::{connection, sidecar};

/// Where the write-ahead sidecar for `database` lives.
#[must_use]
pub fn sidecar_path_for(database: &Path) -> PathBuf {
    sidecar::path_for(database)
}

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

/// What a restore did, and to what.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreReport {
    /// The snapshot that was restored.
    pub source: String,
    /// Where it now lives.
    pub destination: String,
    /// Files renamed out of the way rather than deleted, in the order they were moved.
    pub displaced: Vec<String>,
    /// Size of the restored database.
    pub bytes: u64,
    /// Raw reads the snapshot contained.
    ///
    /// The number to compare against the sidecar afterwards: the gap between them is
    /// exactly the evidence the snapshot predates, and exactly what `reconcile` replays.
    pub raw_reads: u64,
}

/// Restores `snapshot` over `destination`.
///
/// Verifies the snapshot **before** anything is displaced: a restore that destroys a
/// working database and then discovers the backup was corrupt has made the incident worse.
///
/// # Errors
///
/// Returns [`StorageError`] if the snapshot is missing, fails its integrity check, was
/// written by a newer build, or if `destination` exists and `replace` is not set.
pub fn restore(
    snapshot: &Path,
    destination: &Path,
    replace: bool,
) -> Result<RestoreReport, StorageError> {
    if !snapshot.exists() {
        return Err(StorageError::Decode(format!(
            "{} does not exist",
            snapshot.display()
        )));
    }

    let raw_reads = verify(snapshot)?;

    if destination.exists() && !replace {
        return Err(StorageError::Decode(format!(
            "{} already exists; pass --replace to move it aside and restore over it",
            destination.display()
        )));
    }

    // Rename rather than remove, and rename the WAL and shared-memory files with it. A
    // stale `-wal` left beside a restored database would be replayed into it on the next
    // open, quietly grafting the corrupt database's tail onto the good one.
    let mut displaced = Vec::new();
    if destination.exists() {
        let stamp = OffsetDateTime::now_utc().unix_timestamp();
        for suffix in ["", "-wal", "-shm"] {
            let mut from = destination.as_os_str().to_owned();
            from.push(suffix);
            let from = PathBuf::from(from);
            if !from.exists() {
                continue;
            }
            let mut to = from.as_os_str().to_owned();
            to.push(format!(".superseded.{stamp}"));
            let to = PathBuf::from(to);
            std::fs::rename(&from, &to).map_err(|error| {
                StorageError::Decode(format!(
                    "moving {} aside to {}: {error}",
                    from.display(),
                    to.display()
                ))
            })?;
            displaced.push(to.display().to_string());
        }
    }

    std::fs::copy(snapshot, destination).map_err(|error| {
        StorageError::Decode(format!(
            "copying {} to {}: {error}",
            snapshot.display(),
            destination.display()
        ))
    })?;

    // Open it properly — pragmas and migrations — so a restore that produced something
    // unusable fails here rather than at the next command.
    let restored = connection::open(destination)?;
    let reads: i64 = restored.query_row("SELECT COUNT(*) FROM raw_reads", [], |row| row.get(0))?;

    let bytes = std::fs::metadata(destination)
        .map_err(|error| StorageError::Decode(format!("stat {}: {error}", destination.display())))?
        .len();

    Ok(RestoreReport {
        source: snapshot.display().to_string(),
        destination: destination.display().to_string(),
        displaced,
        bytes,
        raw_reads: u64::try_from(reads).unwrap_or(raw_reads),
    })
}

/// Checks a snapshot without modifying it, and returns the reads it holds.
///
/// A bare connection deliberately: [`connection::open`] would migrate the snapshot, and a
/// verification step that writes to the thing it is verifying is not a verification step.
fn verify(snapshot: &Path) -> Result<u64, StorageError> {
    let conn = Connection::open(snapshot)?;

    let integrity: String = conn.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if !integrity.eq_ignore_ascii_case("ok") {
        return Err(StorageError::Decode(format!(
            "{} fails its integrity check: {integrity}",
            snapshot.display()
        )));
    }

    let version: i64 = conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| row.get(0),
    )?;
    if version > SCHEMA_VERSION {
        return Err(StorageError::SchemaTooNew {
            found: version,
            supported: SCHEMA_VERSION,
        });
    }

    let reads: i64 = conn.query_row("SELECT COUNT(*) FROM raw_reads", [], |row| row.get(0))?;
    Ok(u64::try_from(reads).unwrap_or(0))
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
    fn a_restore_refuses_to_overwrite_a_database_unless_asked() {
        let dir = tempdir().expect("tempdir");
        let live = dir.path().join("event.db");
        let snapshot = dir.path().join("backup.db");

        let store = ConfigStore::open(&live).expect("open");
        create(&store, &snapshot).expect("backup");
        drop(store);

        let error = restore(&snapshot, &live, false).expect_err("must refuse");
        assert!(error.to_string().contains("--replace"), "got: {error}");
    }

    #[test]
    fn a_restore_moves_the_old_database_aside_rather_than_deleting_it() {
        // The file being replaced during an incident is the file whose contents may have
        // to be explained afterwards. It is renamed, never removed.
        let dir = tempdir().expect("tempdir");
        let live = dir.path().join("event.db");
        let snapshot = dir.path().join("backup.db");

        {
            let mut journal = SqliteJournal::open(&live).expect("open");
            journal.append(&read("A")).expect("append");
            let store = ConfigStore::open(&live).expect("open store");
            create(&store, &snapshot).expect("backup");
            journal.append(&read("B")).expect("append after backup");
        }

        let report = restore(&snapshot, &live, true).expect("restore");
        assert_eq!(report.raw_reads, 1, "the snapshot predates the second read");
        assert!(!report.displaced.is_empty(), "the old database was kept");
        for displaced in &report.displaced {
            assert!(
                Path::new(displaced).exists(),
                "{displaced} must still be on disk"
            );
        }
    }

    #[test]
    fn a_corrupt_snapshot_is_refused_before_anything_is_displaced() {
        // A restore that destroys a working database and *then* discovers the backup was
        // unreadable has made the incident worse than it was.
        let dir = tempdir().expect("tempdir");
        let live = dir.path().join("event.db");
        let snapshot = dir.path().join("backup.db");

        {
            let mut journal = SqliteJournal::open(&live).expect("open");
            journal.append(&read("A")).expect("append");
        }
        std::fs::write(&snapshot, b"this is not a database").expect("write junk");

        let before = std::fs::metadata(&live).expect("stat").len();
        restore(&snapshot, &live, true).expect_err("a junk snapshot must be refused");
        assert_eq!(
            std::fs::metadata(&live).expect("stat").len(),
            before,
            "the live database must be untouched when the snapshot is bad"
        );
        assert_eq!(
            SqliteJournal::open(&live)
                .expect("reopen")
                .count()
                .expect("count"),
            1
        );
    }

    #[test]
    fn a_snapshot_and_a_sidecar_together_recover_everything() {
        // The whole design in one test. The snapshot carries the configuration as of the
        // last backup; the sidecar carries every read, including the ones written after
        // it. Neither is sufficient. Together they lose nothing.
        let dir = tempdir().expect("tempdir");
        let live = dir.path().join("event.db");
        let snapshot = dir.path().join("pre-race.db");

        {
            let mut journal = SqliteJournal::open(&live).expect("open");
            journal.append(&read("BEFORE")).expect("append");

            let store = ConfigStore::open(&live).expect("open store");
            create(&store, &snapshot).expect("pre-race snapshot");

            // The race happens after the backup, which is the point of a pre-race snapshot.
            journal
                .append_batch(&[read("DURING-1"), read("DURING-2")])
                .expect("append during");
        }

        let report = restore(&snapshot, &live, true).expect("restore");
        assert_eq!(
            report.raw_reads, 1,
            "the snapshot alone knows about one read"
        );

        let (recovered, recovery) = SqliteJournal::open_recovering(&live).expect("recover");
        assert_eq!(
            recovery.replayed_into_database, 2,
            "the sidecar supplies exactly the reads the snapshot predates"
        );

        let chips: Vec<String> = recovered
            .read_all()
            .expect("read all")
            .iter()
            .map(|stored| stored.read.chip.as_str().to_owned())
            .collect();
        assert_eq!(chips, vec!["BEFORE", "DURING-1", "DURING-2"]);
        assert!(recovered.survey().expect("survey").is_clean());
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
