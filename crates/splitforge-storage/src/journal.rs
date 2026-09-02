//! The append-only raw read journal.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension as _, Row, Transaction, params};
use sha2::{Digest, Sha256};
use splitforge_domain::{
    CheckpointId, ChipId, ClockStep, DeviceClockState, FallbackReason, GapDetection, GapEdge,
    JournalError, ManualEntry, ManualEntryId, ParticipantId, RaceId, RawRead, RawReadId,
    RawReadJournal, ReaderGap, ReaderId, StoredClockStep, StoredManualEntry, StoredRawRead,
    TimestampSource,
};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::StorageError;
use crate::connection::{self, from_micros, to_micros};
use crate::sidecar::{self, Sidecar, SidecarRead};

const SELECT_COLUMNS: &str = "seq, id, source, antenna, chip, reader_timestamp_us, \
     reader_uptime_us, received_at_us, received_at_monotonic_ns, rssi_dbm, \
     device_clock_state, timestamp_source, timestamp_fallback_reason, clock_offset_ms, \
     raw_payload, payload_sha256, recorded_at_us";

/// How far the database and its sidecar have drifted apart.
///
/// Produced without writing anything, so a read-only command can report the divergence
/// that a writer would repair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SidecarStatus {
    /// Reads the sidecar holds and verified.
    pub records: u64,
    /// Lines that failed their digest or could not be decoded. Real damage.
    pub corrupt_lines: u64,
    /// Trailing bytes from a write that never finished. Expected after a power cut.
    pub torn_tail_bytes: u64,
    /// Reads in the sidecar that the database does not have. The recoverable case.
    pub missing_from_database: u64,
    /// Reads in the database that the sidecar does not have. Evidence without a backstop.
    pub missing_from_sidecar: u64,
}

impl SidecarStatus {
    /// Whether the two agree and nothing is damaged.
    #[must_use]
    pub const fn is_clean(&self) -> bool {
        self.corrupt_lines == 0
            && self.torn_tail_bytes == 0
            && self.missing_from_database == 0
            && self.missing_from_sidecar == 0
    }
}

/// What reconciliation actually did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RecoveryReport {
    /// The divergence found before anything was repaired.
    pub found: SidecarStatus,
    /// Reads replayed from the sidecar into the database.
    pub replayed_into_database: u64,
    /// Reads copied from the database into the sidecar, restoring its superset property.
    pub backfilled_into_sidecar: u64,
}

impl RecoveryReport {
    /// Whether anything was moved in either direction.
    #[must_use]
    pub const fn repaired_anything(&self) -> bool {
        self.replayed_into_database > 0 || self.backfilled_into_sidecar > 0
    }
}

/// A SQLite-backed [`RawReadJournal`], with a write-ahead sidecar beside it.
#[derive(Debug)]
pub struct SqliteJournal {
    conn: Connection,
    /// `None` only for in-memory journals, which promise no durability of any kind.
    sidecar: Option<Sidecar>,
}

impl SqliteJournal {
    /// Opens (creating if absent) a journal at `path`, applying any pending migrations.
    ///
    /// Opening **does not** reconcile with the sidecar, deliberately. `reads --follow`
    /// opens a journal, and an observer that repairs the thing it is observing is an
    /// observer that writes to a database the operator believes it is only reading. Repair
    /// belongs to the writing process ([`SqliteJournal::open_recovering`]) and to
    /// [`SqliteJournal::reconcile`]; everyone else gets [`SqliteJournal::survey`] and
    /// reports what it finds.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the file cannot be opened, the pragmas cannot be set,
    /// the sidecar cannot be created, or migrations fail.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let path = path.as_ref();
        Self::open_with_sidecar(path, sidecar::path_for(path))
    }

    /// Opens a journal whose sidecar lives somewhere other than beside it.
    ///
    /// The recovery case: a fresh database pointed at the sidecar of a destroyed one.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if either file cannot be opened or migrations fail.
    pub fn open_with_sidecar(
        path: impl AsRef<Path>,
        sidecar: impl Into<PathBuf>,
    ) -> Result<Self, StorageError> {
        Ok(Self {
            conn: connection::open(path)?,
            sidecar: Some(Sidecar::open(sidecar.into())?),
        })
    }

    /// Opens a journal and reconciles it with its sidecar.
    ///
    /// For the process that appends reads. Running recovery on every start of the writer —
    /// rather than only when somebody notices a problem — is what keeps the recovery path
    /// exercised. A mechanism that only ever runs during a disaster is a mechanism whose
    /// first real execution is during a disaster.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the journal cannot be opened or the repair fails.
    pub fn open_recovering(path: impl AsRef<Path>) -> Result<(Self, RecoveryReport), StorageError> {
        let mut journal = Self::open(path)?;
        let report = journal.reconcile()?;
        Ok((journal, report))
    }

    /// Opens a private in-memory journal. For tests and dry runs only — nothing survives.
    ///
    /// No sidecar: an in-memory database has no durability to back up, and writing a file
    /// beside a database that does not exist would be worse than writing nothing.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the database cannot be created or migrated.
    pub fn open_in_memory() -> Result<Self, StorageError> {
        Ok(Self {
            conn: connection::open_in_memory()?,
            sidecar: None,
        })
    }

    /// Where this journal's sidecar lives, if it has one.
    #[must_use]
    pub fn sidecar_path(&self) -> Option<&Path> {
        self.sidecar.as_ref().map(Sidecar::path)
    }

    /// Compares the sidecar against the database without changing either.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the sidecar cannot be read or the database queried.
    pub fn survey(&self) -> Result<SidecarStatus, StorageError> {
        Ok(self
            .compare()?
            .map_or_else(SidecarStatus::default, |c| c.status))
    }

    /// Makes the database and the sidecar agree, in both directions.
    ///
    /// Reads the sidecar holds and the database lacks are inserted; reads the database
    /// holds and the sidecar lacks are appended. Taking the union rather than picking a
    /// winner is the only choice consistent with treating both as evidence — and the
    /// backfill direction is also how a database written before the sidecar existed
    /// acquires one.
    ///
    /// Idempotent: running it twice repairs nothing the second time.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if either side cannot be read or written.
    pub fn reconcile(&mut self) -> Result<RecoveryReport, StorageError> {
        let Some(comparison) = self.compare()? else {
            return Ok(RecoveryReport::default());
        };
        let Comparison {
            status,
            replay,
            backfill,
        } = comparison;

        let mut report = RecoveryReport {
            found: status,
            ..RecoveryReport::default()
        };

        if !replay.is_empty() {
            let tx = self.conn.transaction()?;
            for record in &replay {
                insert_one(&tx, &record.read, record.recorded_at)?;
            }
            tx.commit()?;
            report.replayed_into_database = replay.len() as u64;
        }

        if !backfill.is_empty()
            && let Some(sidecar) = self.sidecar.as_mut()
        {
            sidecar.append_recorded(&backfill)?;
            report.backfilled_into_sidecar = backfill.len() as u64;
        }

        Ok(report)
    }

    /// The shared half of [`SqliteJournal::survey`] and [`SqliteJournal::reconcile`], so
    /// that what is reported and what is repaired can never disagree.
    fn compare(&self) -> Result<Option<Comparison>, StorageError> {
        let scan = match self.sidecar.as_ref() {
            Some(sidecar) => sidecar.scan()?,
            None => return Ok(None),
        };
        let sidecar_ids = Sidecar::ids(&scan);

        let mut statement = self.conn.prepare("SELECT id FROM raw_reads")?;
        let mut rows = statement.query([])?;
        let mut stored_ids = HashSet::new();
        while let Some(row) = rows.next()? {
            stored_ids.insert(row.get::<_, String>(0)?);
        }

        let replay: Vec<SidecarRead> = scan
            .records
            .iter()
            .filter(|record| !stored_ids.contains(&record.read.id.to_string()))
            .cloned()
            .collect();

        // Only pay for loading every stored read when the sidecar is actually short. In
        // the normal case it is a superset by construction and this never runs.
        let backfill: Vec<SidecarRead> = if stored_ids.iter().all(|id| sidecar_ids.contains(id)) {
            Vec::new()
        } else {
            let sql = format!("SELECT {SELECT_COLUMNS} FROM raw_reads ORDER BY seq");
            self.select(&sql, &[])?
                .into_iter()
                .filter(|stored| !sidecar_ids.contains(&stored.read.id.to_string()))
                .map(|stored| SidecarRead {
                    read: stored.read,
                    recorded_at: stored.recorded_at,
                })
                .collect()
        };

        Ok(Some(Comparison {
            status: SidecarStatus {
                records: scan.records.len() as u64,
                corrupt_lines: scan.corrupt_lines,
                torn_tail_bytes: scan.torn_tail_bytes,
                missing_from_database: replay.len() as u64,
                missing_from_sidecar: backfill.len() as u64,
            },
            replay,
            backfill,
        }))
    }

    /// The schema version currently applied to this database.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the migration table cannot be read.
    pub fn schema_version(&self) -> Result<i64, StorageError> {
        connection::schema_version(&self.conn)
    }

    /// Records one observed wall-clock discontinuity.
    ///
    /// No sidecar. The sidecar exists so a read survives a database that dies between the
    /// reader acknowledging it and the commit landing (ADR-0018); a clock step is derived
    /// from state this process already holds, and a process that dies mid-write simply
    /// observes the next comparison against a fresh baseline. Paying a second fsync per
    /// step would buy nothing and would put the clock monitor on the same latency budget
    /// as the read path.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the row cannot be written.
    pub fn record_clock_step(&mut self, step: &ClockStep) -> Result<StoredClockStep, StorageError> {
        // Every timestamp is converted to microseconds, written, and converted back, so
        // the value returned is the value stored rather than the value passed in. The
        // column holds microseconds; `now_utc()` carries nanoseconds. Returning the
        // un-truncated original would hand the caller a `StoredClockStep` that no longer
        // compares equal to the row it describes.
        let recorded_us = connection::to_micros(OffsetDateTime::now_utc())?;
        let before_us = connection::to_micros(step.observed_before)?;
        let after_us = connection::to_micros(step.observed_after)?;

        self.conn.execute(
            "INSERT INTO clock_steps
                (recorded_at_us, observed_before_us, observed_after_us, monotonic_ms, step_ms)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                recorded_us,
                before_us,
                after_us,
                i64::try_from(step.monotonic_ms).unwrap_or(i64::MAX),
                step.step_ms,
            ],
        )?;

        let seq = u64::try_from(self.conn.last_insert_rowid()).unwrap_or(0);
        Ok(StoredClockStep {
            seq,
            recorded_at: connection::from_micros(recorded_us)?,
            step: ClockStep {
                observed_before: connection::from_micros(before_us)?,
                observed_after: connection::from_micros(after_us)?,
                ..*step
            },
        })
    }

    /// How many clock steps this device has observed.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the table cannot be queried.
    pub fn clock_step_count(&self) -> Result<u64, StorageError> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM clock_steps", [], |row| row.get(0))?;
        Ok(u64::try_from(count).unwrap_or(0))
    }

    /// The most recent clock steps, newest first, capped at `limit`.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the table cannot be read or decoded.
    pub fn recent_clock_steps(&self, limit: u32) -> Result<Vec<StoredClockStep>, StorageError> {
        let mut statement = self.conn.prepare(
            "SELECT seq, recorded_at_us, observed_before_us, observed_after_us,
                    monotonic_ms, step_ms
             FROM clock_steps ORDER BY seq DESC LIMIT ?1",
        )?;
        let mut rows = statement.query([limit])?;

        let mut steps = Vec::new();
        while let Some(row) = rows.next()? {
            steps.push(StoredClockStep {
                seq: u64::try_from(row.get::<_, i64>(0)?).unwrap_or(0),
                recorded_at: connection::from_micros(row.get(1)?)?,
                step: ClockStep {
                    observed_before: connection::from_micros(row.get(2)?)?,
                    observed_after: connection::from_micros(row.get(3)?)?,
                    monotonic_ms: u64::try_from(row.get::<_, i64>(4)?).unwrap_or(0),
                    step_ms: row.get(5)?,
                },
            });
        }
        Ok(steps)
    }

    /// Opens a gap for `reader_id`, or returns the one already open.
    ///
    /// **The first detection wins** (ADR-0026). A confirmed disconnection arriving while a
    /// suspected gap is already open does not open a second gap and does not upgrade the
    /// first: it is one outage noticed twice, and the alternative — closing the suspected gap
    /// to reopen it as confirmed — would write a "reads resumed" edge at a moment when
    /// nothing resumed.
    ///
    /// No sidecar, for the same reason a clock step has none (ADR-0018): a gap is derived
    /// from state this process already holds rather than taken off a wire once.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the row cannot be written or the table cannot be read.
    pub fn open_reader_gap(
        &mut self,
        reader_id: &ReaderId,
        detection: GapDetection,
        observed_at: OffsetDateTime,
        monotonic_ms: u64,
        detail: Option<&str>,
    ) -> Result<ReaderGap, StorageError> {
        if let Some(existing) = self.open_reader_gap_for(reader_id)? {
            return Ok(existing);
        }

        let observed_us = to_micros(observed_at)?;
        self.conn.execute(
            "INSERT INTO reader_gap_events
                (reader_id, edge, detection, observed_at_us, monotonic_ms, closes_seq, detail)
             VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6)",
            params![
                reader_id.as_str(),
                GapEdge::Opened.as_str(),
                detection.as_str(),
                observed_us,
                i64::try_from(monotonic_ms).unwrap_or(i64::MAX),
                detail,
            ],
        )?;

        let seq = u64::try_from(self.conn.last_insert_rowid()).unwrap_or(0);
        Ok(ReaderGap {
            opened_seq: seq,
            closed_seq: None,
            reader_id: reader_id.clone(),
            detection,
            started_at: from_micros(observed_us)?,
            ended_at: None,
            started_monotonic_ms: monotonic_ms,
            ended_monotonic_ms: None,
            detail: detail.map(str::to_owned),
        })
    }

    /// Closes the gap open for `reader_id`, returning it, or `None` if none was open.
    ///
    /// Closing is idempotent in the only sense that matters on a reconnect loop: a reader
    /// that was never recorded as gone produces no row, so a provider that reconnects
    /// without ever having failed does not litter the table with empty gaps.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the row cannot be written or the table cannot be read.
    pub fn close_reader_gap(
        &mut self,
        reader_id: &ReaderId,
        observed_at: OffsetDateTime,
        monotonic_ms: u64,
        detail: Option<&str>,
    ) -> Result<Option<ReaderGap>, StorageError> {
        let Some(open) = self.open_reader_gap_for(reader_id)? else {
            return Ok(None);
        };

        let observed_us = to_micros(observed_at)?;
        self.conn.execute(
            "INSERT INTO reader_gap_events
                (reader_id, edge, detection, observed_at_us, monotonic_ms, closes_seq, detail)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                reader_id.as_str(),
                GapEdge::Closed.as_str(),
                open.detection.as_str(),
                observed_us,
                i64::try_from(monotonic_ms).unwrap_or(i64::MAX),
                i64::try_from(open.opened_seq).unwrap_or(i64::MAX),
                detail,
            ],
        )?;

        let seq = u64::try_from(self.conn.last_insert_rowid()).unwrap_or(0);
        Ok(Some(ReaderGap {
            closed_seq: Some(seq),
            ended_at: Some(from_micros(observed_us)?),
            ended_monotonic_ms: Some(monotonic_ms),
            ..open
        }))
    }

    /// The gap currently open for one reader, if it has one.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the table cannot be read or a row cannot be decoded.
    pub fn open_reader_gap_for(
        &self,
        reader_id: &ReaderId,
    ) -> Result<Option<ReaderGap>, StorageError> {
        let mut statement = self.conn.prepare(&Self::gap_query(
            "AND closed.seq IS NULL AND opened.reader_id = ?1",
        ))?;
        let mut rows = statement.query(params![reader_id.as_str()])?;
        rows.next()?.map(Self::gap_from_row).transpose()
    }

    /// Every gap still open, across all readers, oldest first.
    ///
    /// This is what degrades `/health`: an open gap is a reportable state rather than an
    /// omission (ADR-0025), and it answers correctly after a restart because the opening row
    /// was on disk before the power went.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the table cannot be read or a row cannot be decoded.
    pub fn open_reader_gaps(&self) -> Result<Vec<ReaderGap>, StorageError> {
        let mut statement = self.conn.prepare(&Self::gap_query(
            "AND closed.seq IS NULL ORDER BY opened.seq",
        ))?;
        let mut rows = statement.query([])?;

        let mut gaps = Vec::new();
        while let Some(row) = rows.next()? {
            gaps.push(Self::gap_from_row(row)?);
        }
        Ok(gaps)
    }

    /// The most recent gaps, newest first, open and closed alike, capped at `limit`.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the table cannot be read or a row cannot be decoded.
    pub fn recent_reader_gaps(&self, limit: u32) -> Result<Vec<ReaderGap>, StorageError> {
        let mut statement = self
            .conn
            .prepare(&Self::gap_query("ORDER BY opened.seq DESC LIMIT ?1"))?;
        let mut rows = statement.query([limit])?;

        let mut gaps = Vec::new();
        while let Some(row) = rows.next()? {
            gaps.push(Self::gap_from_row(row)?);
        }
        Ok(gaps)
    }

    /// How many gaps this device has recorded, open and closed alike.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the table cannot be queried.
    pub fn reader_gap_count(&self) -> Result<u64, StorageError> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM reader_gap_events WHERE edge = ?1",
            [GapEdge::Opened.as_str()],
            |row| row.get(0),
        )?;
        Ok(u64::try_from(count).unwrap_or(0))
    }

    /// The pairing every gap query shares.
    ///
    /// Written once and reused because it is the join a caller would otherwise forget:
    /// selecting from this table without it returns *edges*, which look enough like gaps to
    /// be mistaken for them and are twice as numerous.
    fn gap_query(tail: &str) -> String {
        let opened = GapEdge::Opened.as_str();
        format!(
            "SELECT opened.seq, closed.seq, opened.reader_id, opened.detection,
                    opened.observed_at_us, closed.observed_at_us,
                    opened.monotonic_ms, closed.monotonic_ms, opened.detail
             FROM reader_gap_events AS opened
             LEFT JOIN reader_gap_events AS closed ON closed.closes_seq = opened.seq
             WHERE opened.edge = '{opened}' {tail}"
        )
    }

    /// Decodes one paired row.
    fn gap_from_row(row: &Row<'_>) -> Result<ReaderGap, StorageError> {
        let detection_token: String = row.get(3)?;
        let detection = GapDetection::parse(&detection_token).ok_or_else(|| {
            StorageError::Decode(format!("unknown gap detection {detection_token:?}"))
        })?;

        let ended_us: Option<i64> = row.get(5)?;
        let ended_monotonic: Option<i64> = row.get(7)?;

        Ok(ReaderGap {
            opened_seq: u64::try_from(row.get::<_, i64>(0)?).unwrap_or(0),
            closed_seq: row
                .get::<_, Option<i64>>(1)?
                .map(|seq| u64::try_from(seq).unwrap_or(0)),
            reader_id: ReaderId::new(row.get::<_, String>(2)?),
            detection,
            started_at: from_micros(row.get(4)?)?,
            ended_at: ended_us.map(from_micros).transpose()?,
            started_monotonic_ms: u64::try_from(row.get::<_, i64>(6)?).unwrap_or(0),
            ended_monotonic_ms: ended_monotonic.map(|ms| u64::try_from(ms).unwrap_or(0)),
            detail: row.get(8)?,
        })
    }

    /// Records one operator's claim that a participant was at a checkpoint at a time.
    ///
    /// No sidecar, for the same reason clock steps have none: the sidecar exists so a read
    /// survives a crash between the reader acknowledging it and the commit landing
    /// (ADR-0018). A manual entry is typed by a person who is still standing there and can
    /// retype it, and the transaction either commits or the CLI reports that it did not.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the row cannot be written — including when the
    /// participant or checkpoint does not exist, which the foreign keys refuse.
    pub fn record_manual_entry(
        &mut self,
        entry: &ManualEntry,
    ) -> Result<StoredManualEntry, StorageError> {
        // Written and read back through the same microsecond conversion, so the value
        // returned is the value stored rather than the value passed in.
        let recorded_us = connection::to_micros(OffsetDateTime::now_utc())?;
        let at_us = connection::to_micros(entry.at)?;

        self.conn.execute(
            "INSERT INTO manual_entries
                (id, race_id, participant_id, checkpoint_id, at_us, actor, reason,
                 recorded_at_us)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                entry.id.to_string(),
                entry.race.to_string(),
                entry.participant.to_string(),
                entry.checkpoint.to_string(),
                at_us,
                entry.actor,
                entry.reason,
                recorded_us,
            ],
        )?;

        let seq = u64::try_from(self.conn.last_insert_rowid()).unwrap_or(0);
        Ok(StoredManualEntry {
            seq,
            recorded_at: connection::from_micros(recorded_us)?,
            entry: ManualEntry {
                at: connection::from_micros(at_us)?,
                ..entry.clone()
            },
        })
    }

    /// Every manual entry for one race, oldest first.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the table cannot be read or decoded.
    pub fn manual_entries(&self, race: RaceId) -> Result<Vec<StoredManualEntry>, StorageError> {
        let mut statement = self.conn.prepare(
            "SELECT seq, id, race_id, participant_id, checkpoint_id, at_us, actor, reason,
                    recorded_at_us
             FROM manual_entries WHERE race_id = ?1 ORDER BY seq",
        )?;
        let mut rows = statement.query([race.to_string()])?;

        let mut entries = Vec::new();
        while let Some(row) = rows.next()? {
            entries.push(StoredManualEntry {
                seq: u64::try_from(row.get::<_, i64>(0)?).unwrap_or(0),
                recorded_at: connection::from_micros(row.get(8)?)?,
                entry: ManualEntry {
                    id: ManualEntryId::from_uuid(parse_uuid(
                        &row.get::<_, String>(1)?,
                        "manual entry id",
                    )?),
                    race: RaceId::from_uuid(parse_uuid(&row.get::<_, String>(2)?, "race id")?),
                    participant: ParticipantId::from_uuid(parse_uuid(
                        &row.get::<_, String>(3)?,
                        "participant id",
                    )?),
                    checkpoint: CheckpointId::from_uuid(parse_uuid(
                        &row.get::<_, String>(4)?,
                        "checkpoint id",
                    )?),
                    at: connection::from_micros(row.get(5)?)?,
                    actor: row.get(6)?,
                    reason: row.get(7)?,
                },
            });
        }
        Ok(entries)
    }

    /// How many manual entries this database holds, across every race.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the table cannot be queried.
    pub fn manual_entry_count(&self) -> Result<u64, StorageError> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM manual_entries", [], |row| row.get(0))?;
        Ok(u64::try_from(count).unwrap_or(0))
    }

    /// The largest step ever observed, by magnitude, ignoring direction.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the table cannot be queried.
    pub fn largest_clock_step_ms(&self) -> Result<Option<i64>, StorageError> {
        let largest: Option<i64> = self
            .conn
            .query_row(
                "SELECT step_ms FROM clock_steps ORDER BY ABS(step_ms) DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        Ok(largest)
    }

    fn insert_all(&mut self, reads: &[RawRead]) -> Result<Vec<StoredRawRead>, StorageError> {
        let recorded_at = OffsetDateTime::now_utc();

        // Write-ahead. The sidecar is appended and fsynced *before* the database
        // transaction opens, which is what makes it a superset of the journal at every
        // instant rather than a copy that races it. If the process dies between here and
        // the commit below, the read is on disk and the next writer start replays it; if
        // the order were reversed, the same crash would leave a read in a database nothing
        // else has a copy of — which is precisely the situation this file exists to prevent.
        if let Some(sidecar) = self.sidecar.as_mut() {
            sidecar.append(reads, recorded_at)?;
        }

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

/// The divergence, plus the work that would close it. Never leaves this module.
struct Comparison {
    status: SidecarStatus,
    replay: Vec<SidecarRead>,
    backfill: Vec<SidecarRead>,
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

/// Parses one stored UUID, naming which column failed rather than just "invalid UUID".
fn parse_uuid(text: &str, what: &str) -> Result<Uuid, StorageError> {
    Uuid::parse_str(text).map_err(|error| StorageError::Decode(format!("{what} {text:?}: {error}")))
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

pub(crate) const fn clock_state_to_str(state: DeviceClockState) -> &'static str {
    match state {
        DeviceClockState::GpsLocked => "gps_locked",
        DeviceClockState::Rtc => "rtc",
        DeviceClockState::NtpSynced => "ntp_synced",
        DeviceClockState::Manual => "manual",
        DeviceClockState::Unsynced => "unsynced",
    }
}

pub(crate) fn clock_state_from_str(text: &str) -> Result<DeviceClockState, StorageError> {
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

pub(crate) const fn fallback_to_str(reason: FallbackReason) -> &'static str {
    match reason {
        FallbackReason::NoReaderTimestamp => "no_reader_timestamp",
        FallbackReason::ReaderUptimeOnly => "reader_uptime_only",
        FallbackReason::ReaderUntrusted => "reader_untrusted",
        FallbackReason::OffsetExceededThreshold => "offset_exceeded_threshold",
    }
}

pub(crate) fn fallback_from_str(text: &str) -> Result<FallbackReason, StorageError> {
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

pub(crate) fn hex_lower(bytes: &[u8]) -> String {
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
    fn every_appended_read_is_in_the_sidecar_as_well_as_the_database() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("event.db");

        let mut journal = SqliteJournal::open(&path).expect("open");
        journal
            .append_batch(&[sample_read("A", 0), sample_read("B", 1)])
            .expect("append");

        let status = journal.survey().expect("survey");
        assert_eq!(status.records, 2);
        assert!(status.is_clean(), "got: {status:?}");
    }

    #[test]
    fn a_database_that_is_lost_entirely_is_rebuilt_from_the_sidecar() {
        // The disaster this whole mechanism exists for: the SQLite file is gone or
        // unreadable, and the only surviving copy of the evidence is a text file.
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
                .expect("append");
            // Read back rather than trusting what `append_batch` returned: the value it
            // hands back still carries nanosecond precision, while both the database and
            // the sidecar persist microseconds. The claim under test is that what was
            // stored comes back, so the comparison has to start from what was stored.
            journal.read_all().expect("read all")
        };

        // Destroy the database and everything SQLite keeps beside it. The sidecar stays.
        for suffix in ["", "-wal", "-shm"] {
            let mut victim = path.as_os_str().to_owned();
            victim.push(suffix);
            let _ = std::fs::remove_file(PathBuf::from(victim));
        }
        assert!(!path.exists());

        let (recovered, report) = SqliteJournal::open_recovering(&path).expect("recover");
        assert_eq!(report.replayed_into_database, 3);
        assert_eq!(report.backfilled_into_sidecar, 0);
        assert_eq!(report.found.missing_from_database, 3);

        let after = recovered.read_all().expect("read all");
        assert_eq!(after.len(), 3);
        for (before, after) in written.iter().zip(after.iter()) {
            assert_eq!(
                before.read, after.read,
                "a recovered read must be the read that was written, not an approximation"
            );
            assert_eq!(
                before.recorded_at, after.recorded_at,
                "recovery must not restamp evidence with the time of the recovery"
            );
        }
        // Sequence numbers are reassigned by the fresh database, and contiguously — which
        // is what `doctor`'s journal.sequence check needs to stay meaningful.
        assert_eq!(
            after.iter().map(|s| s.seq).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
    }

    #[test]
    fn a_database_written_before_the_sidecar_existed_acquires_one() {
        // The upgrade path: a Pi that timed an event on a Milestone 4 binary, handed a
        // build that expects a sidecar. The evidence must gain a backstop, not a warning.
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("event.db");

        {
            let mut journal = SqliteJournal::open(&path).expect("open");
            journal
                .append_batch(&[sample_read("A", 0), sample_read("B", 1)])
                .expect("append");
        }
        std::fs::remove_file(sidecar::path_for(&path)).expect("remove sidecar");

        let journal = SqliteJournal::open(&path).expect("reopen");
        let status = journal.survey().expect("survey");
        assert_eq!(status.missing_from_sidecar, 2);
        assert_eq!(status.missing_from_database, 0);
        assert!(!status.is_clean());

        let (recovered, report) = SqliteJournal::open_recovering(&path).expect("recover");
        assert_eq!(report.backfilled_into_sidecar, 2);
        assert_eq!(report.replayed_into_database, 0);
        assert!(
            recovered.survey().expect("survey").is_clean(),
            "after backfill the two must agree"
        );
    }

    #[test]
    fn reconciling_twice_repairs_nothing_the_second_time() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("event.db");
        {
            let mut journal = SqliteJournal::open(&path).expect("open");
            journal.append(&sample_read("A", 0)).expect("append");
        }
        std::fs::remove_file(sidecar::path_for(&path)).expect("remove sidecar");

        let (mut journal, first) = SqliteJournal::open_recovering(&path).expect("recover");
        assert!(first.repaired_anything());

        let second = journal.reconcile().expect("reconcile again");
        assert!(
            !second.repaired_anything(),
            "recovery must be idempotent, or running it twice doubles the evidence"
        );
        assert_eq!(journal.count().expect("count"), 1);
    }

    #[test]
    fn surveying_a_divergence_does_not_repair_it() {
        // `reads --follow` opens a journal. An observer that writes is not an observer.
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("event.db");
        {
            let mut journal = SqliteJournal::open(&path).expect("open");
            journal.append(&sample_read("A", 0)).expect("append");
        }
        std::fs::remove_file(&path).expect("remove database");

        let journal = SqliteJournal::open(&path).expect("reopen");
        assert_eq!(journal.survey().expect("survey").missing_from_database, 1);
        assert_eq!(
            journal.count().expect("count"),
            0,
            "surveying must leave the database exactly as it found it"
        );
        assert_eq!(
            journal.survey().expect("survey").missing_from_database,
            1,
            "and the divergence must still be there to report"
        );
    }

    #[test]
    fn a_journal_that_cannot_write_its_sidecar_refuses_to_open() {
        // Failing loud, per docs/threat-model.md § 5: a process that quietly ran on without
        // its backstop would keep timing while advertising a guarantee it no longer has,
        // and nobody would find out until the day the database broke.
        let dir = tempdir().expect("tempdir");
        let database = dir.path().join("event.db");
        let unreachable = dir.path().join("no-such-directory").join("reads.jsonl");

        let error = SqliteJournal::open_with_sidecar(&database, unreachable)
            .expect_err("a journal without a sidecar must not open");
        assert!(
            matches!(error, StorageError::Sidecar(_)),
            "the failure must name the sidecar, not arrive as a generic error: {error}"
        );
    }

    #[test]
    fn an_in_memory_journal_has_no_sidecar_and_says_so() {
        let journal = SqliteJournal::open_in_memory().expect("open");
        assert!(journal.sidecar_path().is_none());
        assert_eq!(journal.survey().expect("survey"), SidecarStatus::default());
    }

    #[test]
    fn the_journal_reports_the_schema_version_it_migrated_to() {
        let journal = SqliteJournal::open_in_memory().expect("open");
        assert_eq!(
            journal.schema_version().expect("version"),
            crate::SCHEMA_VERSION
        );
    }

    fn a_step(step_ms: i64) -> ClockStep {
        let before = OffsetDateTime::from_unix_timestamp(1_775_000_000).expect("timestamp");
        ClockStep {
            observed_before: before,
            observed_after: before + time::Duration::milliseconds(10_000 + step_ms),
            monotonic_ms: 10_000,
            step_ms,
        }
    }

    #[test]
    fn a_clock_step_survives_a_round_trip_through_the_database() {
        let mut journal = SqliteJournal::open_in_memory().expect("open");
        assert_eq!(journal.clock_step_count().expect("count"), 0);

        let step = a_step(3_600_000);
        let stored = journal.record_clock_step(&step).expect("record");

        assert_eq!(stored.seq, 1);
        assert_eq!(stored.step, step);
        assert_eq!(journal.clock_step_count().expect("count"), 1);

        let recent = journal.recent_clock_steps(10).expect("recent");
        assert_eq!(recent, vec![stored]);
    }

    #[test]
    fn the_newest_step_comes_back_first() {
        let mut journal = SqliteJournal::open_in_memory().expect("open");
        for step_ms in [1_000, -2_000, 500_000] {
            journal.record_clock_step(&a_step(step_ms)).expect("record");
        }

        let recent = journal.recent_clock_steps(2).expect("recent");
        assert_eq!(recent.len(), 2, "the limit is honored");
        assert_eq!(recent[0].step.step_ms, 500_000);
        assert_eq!(recent[1].step.step_ms, -2_000);
    }

    #[test]
    fn the_largest_step_is_by_magnitude_and_keeps_its_sign() {
        let mut journal = SqliteJournal::open_in_memory().expect("open");
        assert_eq!(journal.largest_clock_step_ms().expect("largest"), None);

        // The biggest jump is backwards, which is the dangerous direction. Reporting it as
        // +1000 because that is the larger signed number would hide exactly the case that
        // matters most.
        for step_ms in [1_000, -40_000, 700] {
            journal.record_clock_step(&a_step(step_ms)).expect("record");
        }

        assert_eq!(
            journal.largest_clock_step_ms().expect("largest"),
            Some(-40_000)
        );
    }

    #[test]
    fn clock_steps_are_append_only_at_the_database() {
        // Same guarantee as raw_reads, and for the same reason (ADR-0011): the device
        // recording that its own clock jumped is the kind of fact somebody disputes after
        // a result is questioned, and a row that can be quietly edited answers nothing.
        let mut journal = SqliteJournal::open_in_memory().expect("open");
        journal.record_clock_step(&a_step(1_000)).expect("record");

        let update = journal
            .conn
            .execute("UPDATE clock_steps SET step_ms = 0", [])
            .expect_err("UPDATE must be refused");
        assert!(
            update.to_string().contains("append-only"),
            "the refusal should say why: {update}"
        );

        let delete = journal
            .conn
            .execute("DELETE FROM clock_steps", [])
            .expect_err("DELETE must be refused");
        assert!(
            delete.to_string().contains("append-only"),
            "the refusal should say why: {delete}"
        );

        assert_eq!(journal.clock_step_count().expect("count"), 1);
    }

    /// A race with one entrant and one checkpoint, so the foreign keys have something to
    /// point at. Manual entries are the first evidence table that references configuration.
    fn a_race_with_one_entrant(journal: &SqliteJournal) -> (RaceId, ParticipantId, CheckpointId) {
        let (event, race) = (Uuid::new_v4(), RaceId::new());
        let (participant, checkpoint) = (ParticipantId::new(), CheckpointId::new());

        journal
            .conn
            .execute(
                "INSERT INTO events (id, name, created_at_us) VALUES (?1, 'Spring Series', 0)",
                params![event.to_string()],
            )
            .expect("event");
        journal
            .conn
            .execute(
                "INSERT INTO races (id, event_id, name, expected_laps, created_at_us)
                 VALUES (?1, ?2, '5K', 1, 0)",
                params![race.to_string(), event.to_string()],
            )
            .expect("race");
        journal
            .conn
            .execute(
                "INSERT INTO participants (id, race_id, bib, name)
                 VALUES (?1, ?2, '109', 'Runner 109')",
                params![participant.to_string(), race.to_string()],
            )
            .expect("participant");
        journal
            .conn
            .execute(
                "INSERT INTO checkpoints (id, race_id, name, kind, sequence)
                 VALUES (?1, ?2, 'finish', 'finish', 2)",
                params![checkpoint.to_string(), race.to_string()],
            )
            .expect("checkpoint");

        (race, participant, checkpoint)
    }

    fn an_entry(
        race: RaceId,
        participant: ParticipantId,
        checkpoint: CheckpointId,
        offset_secs: i64,
    ) -> ManualEntry {
        ManualEntry {
            id: ManualEntryId::new(),
            race,
            participant,
            checkpoint,
            at: OffsetDateTime::UNIX_EPOCH + Duration::seconds(1_700_000_000 + offset_secs),
            actor: "marshal".to_owned(),
            reason: "chip failed at the start".to_owned(),
        }
    }

    #[test]
    fn a_manual_entry_survives_a_round_trip_with_every_field_intact() {
        let mut journal = SqliteJournal::open_in_memory().expect("open");
        let (race, participant, checkpoint) = a_race_with_one_entrant(&journal);
        let entry = an_entry(race, participant, checkpoint, 0);

        let stored = journal.record_manual_entry(&entry).expect("record");
        let all = journal.manual_entries(race).expect("read back");

        assert_eq!(all.len(), 1);
        assert_eq!(all[0].entry, entry, "an operator's claim must round-trip");
        assert_eq!(all[0].seq, stored.seq);
        assert_eq!(all[0].recorded_at, stored.recorded_at);
    }

    #[test]
    fn the_claimed_time_and_the_time_it_was_typed_are_stored_separately() {
        // The whole reason both columns exist. `at` is April; the row is written now.
        let mut journal = SqliteJournal::open_in_memory().expect("open");
        let (race, participant, checkpoint) = a_race_with_one_entrant(&journal);
        let entry = an_entry(race, participant, checkpoint, 0);
        let claimed = entry.at;

        let stored = journal.record_manual_entry(&entry).expect("record");

        assert_eq!(stored.entry.at, claimed);
        assert!(
            stored.recorded_at > claimed,
            "the data-entry time must not be overwritten by the claimed time: {stored:?}"
        );
    }

    #[test]
    fn an_entry_for_a_participant_who_does_not_exist_is_refused() {
        // The roster is configuration and the entry points at it. A typo in a bib must fail
        // at the database rather than producing evidence about nobody.
        let mut journal = SqliteJournal::open_in_memory().expect("open");
        let (race, participant, checkpoint) = a_race_with_one_entrant(&journal);

        let stranger = ManualEntry {
            participant: ParticipantId::new(),
            ..an_entry(race, participant, checkpoint, 0)
        };

        journal
            .record_manual_entry(&stranger)
            .expect_err("a foreign key must refuse an entry for somebody who is not entered");
    }

    #[test]
    fn updating_a_manual_entry_is_refused_by_the_database() {
        let mut journal = SqliteJournal::open_in_memory().expect("open");
        let (race, participant, checkpoint) = a_race_with_one_entrant(&journal);
        journal
            .record_manual_entry(&an_entry(race, participant, checkpoint, 0))
            .expect("record");

        let error = journal
            .conn
            .execute("UPDATE manual_entries SET reason = 'TAMPERED'", [])
            .expect_err("append-only must be enforced by the database, not by convention");
        assert!(error.to_string().contains("append-only"), "got: {error}");
    }

    #[test]
    fn deleting_a_manual_entry_is_refused_by_the_database() {
        // An operator who enters the wrong bib corrects it by entering another row. The
        // mistake stays visible, because the results published in between depended on it.
        let mut journal = SqliteJournal::open_in_memory().expect("open");
        let (race, participant, checkpoint) = a_race_with_one_entrant(&journal);
        journal
            .record_manual_entry(&an_entry(race, participant, checkpoint, 0))
            .expect("record");

        let error = journal
            .conn
            .execute("DELETE FROM manual_entries", [])
            .expect_err("evidence must not be deletable");
        assert!(error.to_string().contains("append-only"), "got: {error}");
        assert_eq!(journal.manual_entry_count().expect("count"), 1);
    }

    #[test]
    fn entries_come_back_in_insertion_order_and_only_for_the_race_asked_for() {
        let mut journal = SqliteJournal::open_in_memory().expect("open");
        let (race, participant, checkpoint) = a_race_with_one_entrant(&journal);

        // Recorded newest-claim-first, to prove the ordering is by `seq` and not by `at`.
        journal
            .record_manual_entry(&an_entry(race, participant, checkpoint, 600))
            .expect("record");
        journal
            .record_manual_entry(&an_entry(race, participant, checkpoint, 60))
            .expect("record");

        let entries = journal.manual_entries(race).expect("read back");
        assert_eq!(entries.len(), 2);
        assert!(
            entries[0].seq < entries[1].seq,
            "insertion order is the only ordering no clock can disturb"
        );
        assert!(entries[0].entry.at > entries[1].entry.at);

        assert_eq!(journal.manual_entry_count().expect("count"), 2);
        assert!(
            journal
                .manual_entries(RaceId::new())
                .expect("read back")
                .is_empty(),
            "another race's entries must not leak into this one"
        );
    }

    #[test]
    fn reopening_the_database_preserves_every_manual_entry() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("event.db");

        let (race, written) = {
            let mut journal = SqliteJournal::open(&path).expect("open");
            let (race, participant, checkpoint) = a_race_with_one_entrant(&journal);
            let stored = journal
                .record_manual_entry(&an_entry(race, participant, checkpoint, 0))
                .expect("record");
            (race, stored)
        };

        let journal = SqliteJournal::open(&path).expect("reopen");
        let entries = journal.manual_entries(race).expect("read back");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0], written, "a restart must not alter the claim");
    }

    fn a_gap_at(seconds: i64) -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(1_775_000_000 + seconds).expect("a valid timestamp")
    }

    #[test]
    fn a_gap_opens_unpaired_and_closes_into_a_bounded_one() {
        let mut journal = SqliteJournal::open_in_memory().expect("open");
        let mat = ReaderId::new("mat");

        let opened = journal
            .open_reader_gap(
                &mat,
                GapDetection::Confirmed,
                a_gap_at(0),
                1_000,
                Some("no such device"),
            )
            .expect("open a gap");
        assert!(opened.is_open(), "a gap with no closing row is open");
        assert_eq!(opened.duration_ms(), None);
        assert_eq!(journal.open_reader_gaps().expect("open gaps").len(), 1);

        let closed = journal
            .close_reader_gap(&mat, a_gap_at(30), 31_000, Some("port reopened"))
            .expect("close a gap")
            .expect("a gap was open");
        assert!(!closed.is_open());
        assert_eq!(closed.opened_seq, opened.opened_seq);
        assert_eq!(closed.duration_ms(), Some(30_000));
        assert_eq!(closed.detection, GapDetection::Confirmed);
        assert!(journal.open_reader_gaps().expect("open gaps").is_empty());
    }

    #[test]
    fn an_open_gap_is_found_again_by_a_process_that_did_not_open_it() {
        // The property that makes two rows worth the trouble: nothing is held in memory, so
        // a service that died mid-gap and restarted still knows the reader was gone.
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("event.db");
        {
            let mut journal = SqliteJournal::open(&path).expect("open");
            journal
                .open_reader_gap(
                    &ReaderId::new("mat"),
                    GapDetection::Confirmed,
                    a_gap_at(0),
                    1_000,
                    None,
                )
                .expect("open a gap");
        }

        let reopened = SqliteJournal::open(&path).expect("reopen");
        let open = reopened.open_reader_gaps().expect("open gaps");
        assert_eq!(open.len(), 1, "the gap outlived the process that opened it");
        assert!(open[0].is_open());
    }

    #[test]
    fn the_first_detection_wins_and_no_second_gap_opens() {
        // ADR-0026: one outage noticed twice is one gap. Upgrading `suspected` to
        // `confirmed` is forbidden by ADR-0025, and closing the suspected gap to reopen it
        // would write a "reads resumed" edge at a moment when nothing resumed.
        let mut journal = SqliteJournal::open_in_memory().expect("open");
        let mat = ReaderId::new("mat");

        let first = journal
            .open_reader_gap(&mat, GapDetection::Suspected, a_gap_at(0), 1_000, None)
            .expect("open a gap");
        let second = journal
            .open_reader_gap(&mat, GapDetection::Confirmed, a_gap_at(5), 6_000, None)
            .expect("the same gap back");

        assert_eq!(second.opened_seq, first.opened_seq);
        assert_eq!(second.detection, GapDetection::Suspected);
        assert_eq!(journal.reader_gap_count().expect("count"), 1);
    }

    #[test]
    fn two_readers_keep_their_gaps_apart() {
        let mut journal = SqliteJournal::open_in_memory().expect("open");
        let start = ReaderId::new("start");
        let finish = ReaderId::new("finish");

        journal
            .open_reader_gap(&start, GapDetection::Confirmed, a_gap_at(0), 1_000, None)
            .expect("open");
        journal
            .open_reader_gap(&finish, GapDetection::Suspected, a_gap_at(1), 2_000, None)
            .expect("open");

        assert_eq!(journal.open_reader_gaps().expect("open gaps").len(), 2);

        journal
            .close_reader_gap(&start, a_gap_at(10), 11_000, None)
            .expect("close")
            .expect("start had a gap");

        let still_open = journal.open_reader_gaps().expect("open gaps");
        assert_eq!(still_open.len(), 1);
        assert_eq!(still_open[0].reader_id, finish);
    }

    #[test]
    fn closing_a_reader_that_was_never_gone_records_nothing() {
        // A provider that reconnects without ever having failed must not litter the table
        // with empty gaps.
        let mut journal = SqliteJournal::open_in_memory().expect("open");
        let closed = journal
            .close_reader_gap(&ReaderId::new("mat"), a_gap_at(0), 1_000, None)
            .expect("close");
        assert!(closed.is_none());
        assert_eq!(journal.reader_gap_count().expect("count"), 0);
    }

    #[test]
    fn a_gap_cannot_be_closed_twice() {
        // The unique index is the guard: two closing rows for one opening row would give a
        // gap two different durations depending on which row a query happened to find.
        let mut journal = SqliteJournal::open_in_memory().expect("open");
        let mat = ReaderId::new("mat");
        let opened = journal
            .open_reader_gap(&mat, GapDetection::Confirmed, a_gap_at(0), 1_000, None)
            .expect("open");

        journal
            .close_reader_gap(&mat, a_gap_at(10), 11_000, None)
            .expect("close")
            .expect("a gap was open");

        let second = journal.conn.execute(
            "INSERT INTO reader_gap_events
                (reader_id, edge, detection, observed_at_us, monotonic_ms, closes_seq, detail)
             VALUES ('mat', 'closed', 'confirmed', 1, 1, ?1, NULL)",
            [i64::try_from(opened.opened_seq).expect("a small seq")],
        );
        assert!(second.is_err(), "a second closing row must be refused");
    }

    #[test]
    fn an_edge_that_pairs_with_nothing_cannot_be_written() {
        // The CHECK constraint, stated as a test: a closing row without `closes_seq` would
        // be a gap end belonging to no gap, and an opening row with one would be a gap that
        // ends itself.
        let journal = SqliteJournal::open_in_memory().expect("open");

        let dangling_close = journal.conn.execute(
            "INSERT INTO reader_gap_events
                (reader_id, edge, detection, observed_at_us, monotonic_ms, closes_seq, detail)
             VALUES ('mat', 'closed', 'confirmed', 1, 1, NULL, NULL)",
            [],
        );
        assert!(dangling_close.is_err(), "a closing row needs closes_seq");

        let self_closing_open = journal.conn.execute(
            "INSERT INTO reader_gap_events
                (reader_id, edge, detection, observed_at_us, monotonic_ms, closes_seq, detail)
             VALUES ('mat', 'opened', 'confirmed', 1, 1, 1, NULL)",
            [],
        );
        assert!(
            self_closing_open.is_err(),
            "an opening row takes no closes_seq"
        );
    }

    #[test]
    fn reader_gaps_are_append_only_at_the_database() {
        // Same guarantee as raw_reads and clock_steps, and the reason ADR-0026 made a gap
        // two rows rather than one: an end written into the opening row would be an UPDATE,
        // and this is what an UPDATE meets.
        let mut journal = SqliteJournal::open_in_memory().expect("open");
        journal
            .open_reader_gap(
                &ReaderId::new("mat"),
                GapDetection::Confirmed,
                a_gap_at(0),
                1_000,
                None,
            )
            .expect("open");

        let update = journal
            .conn
            .execute("UPDATE reader_gap_events SET detection = 'suspected'", [])
            .expect_err("UPDATE must be refused");
        assert!(
            update.to_string().contains("append-only"),
            "the refusal should say why: {update}"
        );

        let delete = journal
            .conn
            .execute("DELETE FROM reader_gap_events", [])
            .expect_err("DELETE must be refused");
        assert!(
            delete.to_string().contains("append-only"),
            "the refusal should say why: {delete}"
        );

        assert_eq!(journal.reader_gap_count().expect("count"), 1);
    }

    #[test]
    fn recent_gaps_come_back_newest_first_open_and_closed_alike() {
        let mut journal = SqliteJournal::open_in_memory().expect("open");
        let mat = ReaderId::new("mat");

        journal
            .open_reader_gap(&mat, GapDetection::Confirmed, a_gap_at(0), 1_000, None)
            .expect("open");
        journal
            .close_reader_gap(&mat, a_gap_at(10), 11_000, None)
            .expect("close");
        journal
            .open_reader_gap(&mat, GapDetection::Suspected, a_gap_at(20), 21_000, None)
            .expect("open again");

        let recent = journal.recent_reader_gaps(10).expect("recent");
        assert_eq!(recent.len(), 2, "two gaps, not four edges");
        assert!(recent[0].is_open(), "newest first");
        assert_eq!(recent[0].detection, GapDetection::Suspected);
        assert!(!recent[1].is_open());
        assert_eq!(recent[1].duration_ms(), Some(10_000));
    }
}
