//! The append-only raw read journal.

use std::path::Path;

use rusqlite::{Connection, Row, Transaction, params};
use sha2::{Digest, Sha256};
use splitforge_domain::{
    ChipId, DeviceClockState, FallbackReason, JournalError, RawRead, RawReadId, RawReadJournal,
    ReaderId, StoredRawRead, TimestampSource,
};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::StorageError;
use crate::connection::{self, from_micros, to_micros};

const SELECT_COLUMNS: &str = "seq, id, source, antenna, chip, reader_timestamp_us, \
     reader_uptime_us, received_at_us, received_at_monotonic_ns, rssi_dbm, \
     device_clock_state, timestamp_source, timestamp_fallback_reason, clock_offset_ms, \
     raw_payload, payload_sha256, recorded_at_us";

/// A SQLite-backed [`RawReadJournal`].
#[derive(Debug)]
pub struct SqliteJournal {
    conn: Connection,
}

impl SqliteJournal {
    /// Opens (creating if absent) a journal at `path`, applying any pending migrations.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the file cannot be opened, the pragmas cannot be set,
    /// or migrations fail.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        Ok(Self {
            conn: connection::open(path)?,
        })
    }

    /// Opens a private in-memory journal. For tests and dry runs only — nothing survives.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the database cannot be created or migrated.
    pub fn open_in_memory() -> Result<Self, StorageError> {
        Ok(Self {
            conn: connection::open_in_memory()?,
        })
    }

    /// The schema version currently applied to this database.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the migration table cannot be read.
    pub fn schema_version(&self) -> Result<i64, StorageError> {
        connection::schema_version(&self.conn)
    }

    fn insert_all(&mut self, reads: &[RawRead]) -> Result<Vec<StoredRawRead>, StorageError> {
        let recorded_at = OffsetDateTime::now_utc();
        let tx = self.conn.transaction()?;
        let mut stored = Vec::with_capacity(reads.len());
        for read in reads {
            stored.push(insert_one(&tx, read, recorded_at)?);
        }
        // Durability happens here: with synchronous = FULL the commit is fsynced, so
        // returning from this function means the reads survive a power cut.
        tx.commit()?;
        Ok(stored)
    }

    fn select(
        &self,
        sql: &str,
        params: &[&dyn rusqlite::ToSql],
    ) -> Result<Vec<StoredRawRead>, StorageError> {
        let mut statement = self.conn.prepare(sql)?;
        let mut rows = statement.query(params)?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(row_to_stored(row)?);
        }
        Ok(out)
    }
}

impl RawReadJournal for SqliteJournal {
    fn append(&mut self, read: &RawRead) -> Result<StoredRawRead, JournalError> {
        let mut stored = self.insert_all(std::slice::from_ref(read))?;
        stored
            .pop()
            .ok_or_else(|| JournalError::Backend("insert returned no row".to_owned()))
    }

    fn append_batch(&mut self, reads: &[RawRead]) -> Result<Vec<StoredRawRead>, JournalError> {
        Ok(self.insert_all(reads)?)
    }

    fn read_all(&self) -> Result<Vec<StoredRawRead>, JournalError> {
        let sql = format!("SELECT {SELECT_COLUMNS} FROM raw_reads ORDER BY seq");
        Ok(self.select(&sql, &[])?)
    }

    fn read_since(&self, after_seq: u64) -> Result<Vec<StoredRawRead>, JournalError> {
        let after = i64::try_from(after_seq).unwrap_or(i64::MAX);
        let sql = format!("SELECT {SELECT_COLUMNS} FROM raw_reads WHERE seq > ?1 ORDER BY seq");
        Ok(self.select(&sql, &[&after])?)
    }

    fn count(&self) -> Result<u64, JournalError> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM raw_reads", [], |row| row.get(0))
            .map_err(StorageError::from)?;
        Ok(u64::try_from(count).unwrap_or(0))
    }
}

fn insert_one(
    tx: &Transaction<'_>,
    read: &RawRead,
    recorded_at: OffsetDateTime,
) -> Result<StoredRawRead, StorageError> {
    let payload_sha256 = hex_lower(Sha256::digest(&read.raw_payload).as_slice());
    let (source_text, fallback_text) = match read.timestamp_source {
        TimestampSource::ReaderUtc => ("reader_utc", None),
        TimestampSource::DeviceReceipt { reason } => {
            ("device_receipt", Some(fallback_to_str(reason)))
        }
    };
    let reader_timestamp_us = read.reader_timestamp.map(to_micros).transpose()?;
    // SQLite integers are signed, and rusqlite deliberately refuses `u64` rather than
    // silently wrapping. Convert explicitly so an out-of-range value is an error we
    // report, not a negative number we store.
    let reader_uptime_us = read
        .reader_uptime_us
        .map(|value| u64_to_i64(value, "reader_uptime_us"))
        .transpose()?;
    let received_at_monotonic_ns = read
        .received_at_monotonic_ns
        .map(|value| u64_to_i64(value, "received_at_monotonic_ns"))
        .transpose()?;

    tx.execute(
        "INSERT INTO raw_reads (
            id, source, antenna, chip, reader_timestamp_us, reader_uptime_us,
            received_at_us, received_at_monotonic_ns, rssi_dbm, device_clock_state,
            timestamp_source, timestamp_fallback_reason, clock_offset_ms,
            raw_payload, payload_sha256, recorded_at_us
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
        params![
            read.id.to_string(),
            read.source.as_str(),
            read.antenna,
            read.chip.as_str(),
            reader_timestamp_us,
            reader_uptime_us,
            to_micros(read.received_at)?,
            received_at_monotonic_ns,
            read.rssi_dbm,
            clock_state_to_str(read.device_clock_state),
            source_text,
            fallback_text,
            read.clock_offset_ms,
            read.raw_payload,
            payload_sha256,
            to_micros(recorded_at)?,
        ],
    )?;

    let seq = u64::try_from(tx.last_insert_rowid())
        .map_err(|_| StorageError::Decode("negative row sequence".to_owned()))?;

    Ok(StoredRawRead {
        seq,
        recorded_at,
        payload_sha256,
        read: read.clone(),
    })
}

fn row_to_stored(row: &Row<'_>) -> Result<StoredRawRead, StorageError> {
    let seq: i64 = row.get("seq")?;
    let id_text: String = row.get("id")?;
    let id = RawReadId::from_uuid(
        Uuid::parse_str(&id_text)
            .map_err(|error| StorageError::Decode(format!("raw read id {id_text:?}: {error}")))?,
    );

    let antenna: Option<i64> = row.get("antenna")?;
    let antenna = antenna
        .map(u16::try_from)
        .transpose()
        .map_err(|_| StorageError::Decode("antenna out of range".to_owned()))?;

    let reader_timestamp: Option<i64> = row.get("reader_timestamp_us")?;
    let reader_timestamp = reader_timestamp.map(from_micros).transpose()?;

    let reader_uptime_us: Option<i64> = row.get("reader_uptime_us")?;
    let reader_uptime_us = reader_uptime_us
        .map(u64::try_from)
        .transpose()
        .map_err(|_| StorageError::Decode("reader uptime out of range".to_owned()))?;

    let received_at_monotonic_ns: Option<i64> = row.get("received_at_monotonic_ns")?;
    let received_at_monotonic_ns = received_at_monotonic_ns
        .map(u64::try_from)
        .transpose()
        .map_err(|_| StorageError::Decode("monotonic timestamp out of range".to_owned()))?;

    let rssi: Option<i64> = row.get("rssi_dbm")?;
    let rssi_dbm = rssi
        .map(i16::try_from)
        .transpose()
        .map_err(|_| StorageError::Decode("rssi out of range".to_owned()))?;

    let source_text: String = row.get("timestamp_source")?;
    let fallback_text: Option<String> = row.get("timestamp_fallback_reason")?;
    let timestamp_source = match source_text.as_str() {
        "reader_utc" => TimestampSource::ReaderUtc,
        "device_receipt" => {
            let reason = fallback_text.ok_or_else(|| {
                StorageError::Decode("device_receipt row has no fallback reason".to_owned())
            })?;
            TimestampSource::DeviceReceipt {
                reason: fallback_from_str(&reason)?,
            }
        }
        other => {
            return Err(StorageError::Decode(format!(
                "unknown timestamp_source {other:?}"
            )));
        }
    };

    let clock_state_text: String = row.get("device_clock_state")?;

    Ok(StoredRawRead {
        seq: u64::try_from(seq)
            .map_err(|_| StorageError::Decode("negative row sequence".to_owned()))?,
        recorded_at: from_micros(row.get("recorded_at_us")?)?,
        payload_sha256: row.get("payload_sha256")?,
        read: RawRead {
            id,
            source: ReaderId::new(row.get::<_, String>("source")?),
            antenna,
            chip: ChipId::new(row.get::<_, String>("chip")?),
            reader_timestamp,
            reader_uptime_us,
            received_at: from_micros(row.get("received_at_us")?)?,
            received_at_monotonic_ns,
            rssi_dbm,
            device_clock_state: clock_state_from_str(&clock_state_text)?,
            timestamp_source,
            clock_offset_ms: row.get("clock_offset_ms")?,
            raw_payload: row.get("raw_payload")?,
        },
    })
}

const fn clock_state_to_str(state: DeviceClockState) -> &'static str {
    match state {
        DeviceClockState::GpsLocked => "gps_locked",
        DeviceClockState::Rtc => "rtc",
        DeviceClockState::NtpSynced => "ntp_synced",
        DeviceClockState::Manual => "manual",
        DeviceClockState::Unsynced => "unsynced",
    }
}

fn clock_state_from_str(text: &str) -> Result<DeviceClockState, StorageError> {
    match text {
        "gps_locked" => Ok(DeviceClockState::GpsLocked),
        "rtc" => Ok(DeviceClockState::Rtc),
        "ntp_synced" => Ok(DeviceClockState::NtpSynced),
        "manual" => Ok(DeviceClockState::Manual),
        "unsynced" => Ok(DeviceClockState::Unsynced),
        other => Err(StorageError::Decode(format!(
            "unknown device_clock_state {other:?}"
        ))),
    }
}

const fn fallback_to_str(reason: FallbackReason) -> &'static str {
    match reason {
        FallbackReason::NoReaderTimestamp => "no_reader_timestamp",
        FallbackReason::ReaderUptimeOnly => "reader_uptime_only",
        FallbackReason::ReaderUntrusted => "reader_untrusted",
        FallbackReason::OffsetExceededThreshold => "offset_exceeded_threshold",
    }
}

fn fallback_from_str(text: &str) -> Result<FallbackReason, StorageError> {
    match text {
        "no_reader_timestamp" => Ok(FallbackReason::NoReaderTimestamp),
        "reader_uptime_only" => Ok(FallbackReason::ReaderUptimeOnly),
        "reader_untrusted" => Ok(FallbackReason::ReaderUntrusted),
        "offset_exceeded_threshold" => Ok(FallbackReason::OffsetExceededThreshold),
        other => Err(StorageError::Decode(format!(
            "unknown timestamp_fallback_reason {other:?}"
        ))),
    }
}

fn u64_to_i64(value: u64, field: &'static str) -> Result<i64, StorageError> {
    i64::try_from(value).map_err(|_| StorageError::Decode(format!("{field} exceeds i64 range")))
}

fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use time::Duration;

    fn sample_read(chip: &str, offset_secs: i64) -> RawRead {
        let received = OffsetDateTime::UNIX_EPOCH + Duration::seconds(1_700_000_000 + offset_secs);
        RawRead {
            id: RawReadId::new(),
            source: ReaderId::new("finish"),
            antenna: Some(2),
            chip: ChipId::new(chip),
            reader_timestamp: Some(received - Duration::milliseconds(35)),
            reader_uptime_us: None,
            received_at: received,
            received_at_monotonic_ns: Some(9_876_543_210),
            rssi_dbm: Some(-58),
            device_clock_state: DeviceClockState::GpsLocked,
            timestamp_source: TimestampSource::ReaderUtc,
            clock_offset_ms: Some(35),
            raw_payload: vec![0xDE, 0xAD, 0xBE, 0xEF],
        }
    }

    #[test]
    fn a_read_survives_a_round_trip_with_every_field_intact() {
        let mut journal = SqliteJournal::open_in_memory().expect("open");
        let read = sample_read("E280A", 0);
        let stored = journal.append(&read).expect("append");

        let all = journal.read_all().expect("read back");
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].read, read, "evidence must round-trip byte for byte");
        assert_eq!(all[0].seq, stored.seq);
        assert_eq!(
            all[0].payload_sha256,
            "5f78c33274e43fa9de5659265c1d917e25c03722dcb0b8d27db8d5feaa813953",
            "sha256 of DEADBEEF"
        );
    }

    #[test]
    fn sequence_numbers_are_monotonic() {
        let mut journal = SqliteJournal::open_in_memory().expect("open");
        let first = journal.append(&sample_read("A", 0)).expect("append");
        let second = journal.append(&sample_read("B", 1)).expect("append");
        assert!(second.seq > first.seq);
        assert_eq!(journal.count().expect("count"), 2);
    }

    #[test]
    fn read_since_returns_only_newer_reads() {
        let mut journal = SqliteJournal::open_in_memory().expect("open");
        let first = journal.append(&sample_read("A", 0)).expect("append");
        journal.append(&sample_read("B", 1)).expect("append");

        let newer = journal.read_since(first.seq).expect("read since");
        assert_eq!(newer.len(), 1);
        assert_eq!(newer[0].read.chip.as_str(), "B");
    }

    #[test]
    fn updating_a_raw_read_is_refused_by_the_database() {
        let mut journal = SqliteJournal::open_in_memory().expect("open");
        journal.append(&sample_read("A", 0)).expect("append");

        let error = journal
            .conn
            .execute("UPDATE raw_reads SET chip = 'TAMPERED'", [])
            .expect_err("append-only must be enforced by the database, not by convention");
        assert!(error.to_string().contains("append-only"), "got: {error}");
    }

    #[test]
    fn deleting_a_raw_read_is_refused_by_the_database() {
        let mut journal = SqliteJournal::open_in_memory().expect("open");
        journal.append(&sample_read("A", 0)).expect("append");

        let error = journal
            .conn
            .execute("DELETE FROM raw_reads", [])
            .expect_err("evidence must not be deletable");
        assert!(error.to_string().contains("append-only"), "got: {error}");
        assert_eq!(journal.count().expect("count"), 1);
    }

    #[test]
    fn a_batch_is_all_or_nothing() {
        let mut journal = SqliteJournal::open_in_memory().expect("open");
        let duplicate = sample_read("A", 0);
        // Same id twice violates the UNIQUE constraint, so the whole batch must roll back.
        let batch = vec![sample_read("B", 1), duplicate.clone(), duplicate];
        journal
            .append_batch(&batch)
            .expect_err("duplicate id must fail the batch");
        assert_eq!(
            journal.count().expect("count"),
            0,
            "a failed batch must leave nothing behind"
        );
    }

    #[test]
    fn reopening_the_database_preserves_every_read() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("event.db");

        let written = {
            let mut journal = SqliteJournal::open(&path).expect("open");
            journal
                .append_batch(&[
                    sample_read("A", 0),
                    sample_read("B", 1),
                    sample_read("C", 2),
                ])
                .expect("append batch")
        };

        // Drop the handle entirely, as a restart would.
        let reopened = SqliteJournal::open(&path).expect("reopen");
        let after = reopened.read_all().expect("read all");

        assert_eq!(after.len(), 3);
        assert_eq!(reopened.count().expect("count"), 3);
        for (before, after) in written.iter().zip(after.iter()) {
            assert_eq!(before.read, after.read);
            assert_eq!(before.seq, after.seq);
        }
    }

    #[test]
    fn the_journal_reports_the_schema_version_it_migrated_to() {
        let journal = SqliteJournal::open_in_memory().expect("open");
        assert_eq!(
            journal.schema_version().expect("version"),
            crate::SCHEMA_VERSION
        );
    }
}
