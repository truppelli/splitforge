//! Event configuration: everything an operator sets up before a race.
//!
//! The counterpart to [`crate::SqliteJournal`]. The journal holds evidence and never
//! changes; this holds configuration and changes constantly. Keeping them in separate types
//! against separate connections makes the distinction hard to lose track of, and keeps the
//! config store out of the read path's transaction.
//!
//! ## Identifiers come from the caller
//!
//! Nothing here mints an id. Every `save_*` method persists the id already on the entity it
//! is given. That is what lets `splitforge fixture load` write a fixture's deterministic
//! UUIDv5 identifiers verbatim, so a fixture-driven test can compare derived output across
//! two different databases — see `splitforge-testkit`.
//!
//! ## Re-import preserves identity
//!
//! Importing a roster twice updates the people already there rather than replacing them.
//! `docs/architecture.md` § 4 requires it: a re-import must produce a new derivation, not a
//! destroyed journal. If re-import minted fresh participant ids, every chip assignment
//! would dangle and every already-derived timing event would point at somebody who no
//! longer exists.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, Row, params};
use splitforge_domain::{
    AntennaMap, Bib, Checkpoint, CheckpointId, CheckpointKind, ChipAssignment, ChipId,
    DEFAULT_SILENCE_THRESHOLD_MS, Event, EventId, Participant, ParticipantId, Race, RaceConfig,
    RaceId, RaceSession, Reader, ReaderId, SessionAction, StartMode, TimestampTrust, TimingPolicy,
};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::StorageError;
use crate::connection::{self, from_micros, to_micros};
use crate::space::DEFAULT_MIN_FREE_BYTES;

/// Key under which the free-space floor is stored in `device_settings`.
const MIN_FREE_BYTES_KEY: &str = "min_free_bytes";

/// Key under which the reader-silence threshold is stored in `device_settings`.
const READER_SILENCE_MS_KEY: &str = "reader_silence_threshold_ms";

/// What an import changed.
///
/// Reported separately rather than as one total, because "imported 412 participants" is
/// reassuring and wrong when 400 of them were already there under different names.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ImportSummary {
    /// Rows that did not exist before.
    pub inserted: usize,
    /// Rows that existed and were changed.
    pub updated: usize,
    /// Rows that existed and already matched.
    pub unchanged: usize,
}

impl ImportSummary {
    /// Total rows considered.
    #[must_use]
    pub const fn total(&self) -> usize {
        self.inserted + self.updated + self.unchanged
    }
}

/// One recorded operator action.
///
/// Lives here rather than in the domain because nothing derives anything from it: it exists
/// so that a finished event can explain itself (`docs/timing-model.md` § 8). The domain
/// stays the set of things scoring actually reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditEntry {
    /// Monotonic sequence within the database.
    pub seq: u64,
    /// When the action happened.
    pub at: OffsetDateTime,
    /// Who did it.
    pub actor: String,
    /// A dotted machine-readable name, such as `roster.import` or `policy.set`.
    pub action: String,
    /// What it applied to, when that is meaningful.
    pub subject: Option<String>,
    /// Structured before/after detail, as JSON.
    pub detail: Option<String>,
}

/// Which race a command should act on, when several exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RaceSelection {
    /// Exactly one race matched.
    One(Box<Race>),
    /// Nothing has been configured yet.
    None,
    /// The selector was ambiguous. Carries the candidates, so the caller can list them.
    Ambiguous(Vec<Race>),
}

/// Read/write access to event configuration.
#[derive(Debug)]
pub struct ConfigStore {
    conn: Connection,
}

impl ConfigStore {
    /// Opens (creating if absent) the configuration store at `path`.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the file cannot be opened or migrations fail.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        Ok(Self {
            conn: connection::open(path)?,
        })
    }

    /// Opens a private in-memory store. For tests only — nothing survives.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the database cannot be created or migrated.
    pub fn open_in_memory() -> Result<Self, StorageError> {
        Ok(Self {
            conn: connection::open_in_memory()?,
        })
    }

    /// The schema version currently applied.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the migration table cannot be read.
    pub fn schema_version(&self) -> Result<i64, StorageError> {
        connection::schema_version(&self.conn)
    }

    // ---- events -----------------------------------------------------------------

    /// Creates or updates an event.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the write fails.
    pub fn save_event(&mut self, event: &Event) -> Result<(), StorageError> {
        self.conn.execute(
            "INSERT INTO events (id, name, created_at_us) VALUES (?1, ?2, ?3)
             ON CONFLICT (id) DO UPDATE SET name = excluded.name",
            params![
                event.id.to_string(),
                event.name,
                to_micros(OffsetDateTime::now_utc())?
            ],
        )?;
        Ok(())
    }

    /// Every event, oldest first.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if a row cannot be decoded.
    pub fn events(&self) -> Result<Vec<Event>, StorageError> {
        let mut statement = self
            .conn
            .prepare("SELECT id, name FROM events ORDER BY created_at_us, name")?;
        let mut rows = statement.query([])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(Event {
                id: EventId::from_uuid(uuid_column(row, "id")?),
                name: row.get("name")?,
            });
        }
        Ok(out)
    }

    // ---- races ------------------------------------------------------------------

    /// Creates or updates a race.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the write fails, including when the event does not exist.
    pub fn save_race(&mut self, race: &Race) -> Result<(), StorageError> {
        let scheduled = race.scheduled_start.map(to_micros).transpose()?;
        self.conn.execute(
            "INSERT INTO races (id, event_id, name, scheduled_start_us, expected_laps, created_at_us)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (id) DO UPDATE SET
                 name = excluded.name,
                 scheduled_start_us = excluded.scheduled_start_us,
                 expected_laps = excluded.expected_laps",
            params![
                race.id.to_string(),
                race.event.to_string(),
                race.name,
                scheduled,
                i64::from(race.expected_laps),
                to_micros(OffsetDateTime::now_utc())?
            ],
        )?;
        Ok(())
    }

    /// Every race, oldest first.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if a row cannot be decoded.
    pub fn races(&self) -> Result<Vec<Race>, StorageError> {
        let mut statement = self.conn.prepare(
            "SELECT id, event_id, name, scheduled_start_us, expected_laps
             FROM races ORDER BY created_at_us, name",
        )?;
        let mut rows = statement.query([])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(race_from_row(row)?);
        }
        Ok(out)
    }

    /// Resolves an optional `--race` selector against what is configured.
    ///
    /// With one race configured, naming it is noise, so an absent selector picks it. With
    /// several, an absent selector is genuinely ambiguous and the caller is told the
    /// candidates rather than given the first one — operating the wrong race is a failure
    /// that is not noticed until the results are wrong.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the races cannot be read.
    pub fn resolve_race(&self, selector: Option<&str>) -> Result<RaceSelection, StorageError> {
        let races = self.races()?;
        let Some(selector) = selector else {
            return Ok(match races.len() {
                0 => RaceSelection::None,
                1 => races
                    .into_iter()
                    .next()
                    .map_or(RaceSelection::None, |race| {
                        RaceSelection::One(Box::new(race))
                    }),
                _ => RaceSelection::Ambiguous(races),
            });
        };

        let matched: Vec<Race> = races
            .into_iter()
            .filter(|race| {
                race.name.eq_ignore_ascii_case(selector) || race.id.to_string() == selector
            })
            .collect();

        Ok(match matched.len() {
            0 => RaceSelection::None,
            1 => matched
                .into_iter()
                .next()
                .map_or(RaceSelection::None, |race| {
                    RaceSelection::One(Box::new(race))
                }),
            _ => RaceSelection::Ambiguous(matched),
        })
    }

    // ---- checkpoints ------------------------------------------------------------

    /// Creates or updates a checkpoint.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the write fails, including when the race does not exist.
    pub fn save_checkpoint(&mut self, checkpoint: &Checkpoint) -> Result<(), StorageError> {
        self.conn.execute(
            "INSERT INTO checkpoints (id, race_id, name, kind, sequence) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (id) DO UPDATE SET
                 name = excluded.name, kind = excluded.kind, sequence = excluded.sequence",
            params![
                checkpoint.id.to_string(),
                checkpoint.race.to_string(),
                checkpoint.name,
                kind_to_str(checkpoint.kind),
                i64::from(checkpoint.sequence),
            ],
        )?;
        Ok(())
    }

    /// A race's checkpoints, in course order.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if a row cannot be decoded.
    pub fn checkpoints(&self, race: RaceId) -> Result<Vec<Checkpoint>, StorageError> {
        let mut statement = self.conn.prepare(
            "SELECT id, race_id, name, kind, sequence FROM checkpoints
             WHERE race_id = ?1 ORDER BY sequence, name",
        )?;
        let mut rows = statement.query([race.to_string()])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(checkpoint_from_row(row)?);
        }
        Ok(out)
    }

    /// Every checkpoint in the database, whichever race it belongs to.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if a row cannot be decoded.
    pub fn all_checkpoints(&self) -> Result<Vec<Checkpoint>, StorageError> {
        let mut statement = self.conn.prepare(
            "SELECT id, race_id, name, kind, sequence FROM checkpoints ORDER BY sequence",
        )?;
        let mut rows = statement.query([])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(checkpoint_from_row(row)?);
        }
        Ok(out)
    }

    // ---- roster -----------------------------------------------------------------

    /// Imports a roster, preserving the identity of anybody already entered.
    ///
    /// Matching is by `(race, bib)`. A participant whose name changed is updated in place
    /// and keeps their id, so existing chip assignments and derived timing events stay
    /// valid.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the transaction fails.
    pub fn import_participants(
        &mut self,
        participants: &[Participant],
    ) -> Result<ImportSummary, StorageError> {
        let tx = self.conn.transaction()?;
        let mut summary = ImportSummary::default();

        for participant in participants {
            let existing: Option<(String, String)> = tx
                .query_row(
                    "SELECT id, name FROM participants WHERE race_id = ?1 AND bib = ?2",
                    params![participant.race.to_string(), participant.bib.as_str()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;

            match existing {
                Some((_, name)) if name == participant.name => summary.unchanged += 1,
                Some((id, _)) => {
                    tx.execute(
                        "UPDATE participants SET name = ?1 WHERE id = ?2",
                        params![participant.name, id],
                    )?;
                    summary.updated += 1;
                }
                None => {
                    tx.execute(
                        "INSERT INTO participants (id, race_id, bib, name) VALUES (?1, ?2, ?3, ?4)",
                        params![
                            participant.id.to_string(),
                            participant.race.to_string(),
                            participant.bib.as_str(),
                            participant.name,
                        ],
                    )?;
                    summary.inserted += 1;
                }
            }
        }

        tx.commit()?;
        Ok(summary)
    }

    /// A race's roster, in bib order.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if a row cannot be decoded.
    pub fn participants(&self, race: RaceId) -> Result<Vec<Participant>, StorageError> {
        let mut statement = self.conn.prepare(
            "SELECT id, race_id, bib, name FROM participants WHERE race_id = ?1 ORDER BY bib",
        )?;
        let mut rows = statement.query([race.to_string()])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(Participant {
                id: ParticipantId::from_uuid(uuid_column(row, "id")?),
                race: RaceId::from_uuid(uuid_column(row, "race_id")?),
                bib: Bib::new(row.get::<_, String>("bib")?),
                name: row.get("name")?,
            });
        }
        Ok(out)
    }

    // ---- chip assignments -------------------------------------------------------

    /// Imports chip assignments, keyed on `(race, chip, valid_from)`.
    ///
    /// Re-importing the same file is a no-op. Handing the same chip to somebody else for
    /// the same window is an update, which is the correct behavior for the actual
    /// operational case: a chip fails at the start line and is swapped.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the transaction fails.
    pub fn import_assignments(
        &mut self,
        assignments: &[ChipAssignment],
    ) -> Result<ImportSummary, StorageError> {
        let tx = self.conn.transaction()?;
        let mut summary = ImportSummary::default();

        for assignment in assignments {
            let valid_from = to_micros(assignment.valid_from)?;
            let valid_until = assignment.valid_until.map(to_micros).transpose()?;

            let existing: Option<(i64, String, Option<i64>)> = tx
                .query_row(
                    "SELECT id, participant_id, valid_until_us FROM chip_assignments
                     WHERE race_id = ?1 AND chip = ?2 AND valid_from_us = ?3",
                    params![
                        assignment.race.to_string(),
                        assignment.chip.as_str(),
                        valid_from
                    ],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?;

            match existing {
                Some((_, participant, until))
                    if participant == assignment.participant.to_string()
                        && until == valid_until =>
                {
                    summary.unchanged += 1;
                }
                Some((id, _, _)) => {
                    tx.execute(
                        "UPDATE chip_assignments SET participant_id = ?1, valid_until_us = ?2
                         WHERE id = ?3",
                        params![assignment.participant.to_string(), valid_until, id],
                    )?;
                    summary.updated += 1;
                }
                None => {
                    tx.execute(
                        "INSERT INTO chip_assignments
                             (chip, participant_id, race_id, valid_from_us, valid_until_us)
                         VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![
                            assignment.chip.as_str(),
                            assignment.participant.to_string(),
                            assignment.race.to_string(),
                            valid_from,
                            valid_until,
                        ],
                    )?;
                    summary.inserted += 1;
                }
            }
        }

        tx.commit()?;
        Ok(summary)
    }

    /// A race's chip assignments, oldest window first.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if a row cannot be decoded.
    pub fn assignments(&self, race: RaceId) -> Result<Vec<ChipAssignment>, StorageError> {
        let mut statement = self.conn.prepare(
            "SELECT chip, participant_id, race_id, valid_from_us, valid_until_us
             FROM chip_assignments WHERE race_id = ?1 ORDER BY valid_from_us, chip",
        )?;
        let mut rows = statement.query([race.to_string()])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            let until: Option<i64> = row.get("valid_until_us")?;
            out.push(ChipAssignment {
                chip: ChipId::new(row.get::<_, String>("chip")?),
                participant: ParticipantId::from_uuid(uuid_column(row, "participant_id")?),
                race: RaceId::from_uuid(uuid_column(row, "race_id")?),
                valid_from: from_micros(row.get("valid_from_us")?)?,
                valid_until: until.map(from_micros).transpose()?,
            });
        }
        Ok(out)
    }

    // ---- readers ----------------------------------------------------------------

    /// Creates or updates a reader.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the write fails.
    pub fn save_reader(&mut self, reader: &Reader) -> Result<(), StorageError> {
        self.conn.execute(
            "INSERT INTO readers (id, label, endpoint, timestamp_trust, created_at_us)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (id) DO UPDATE SET
                 label = excluded.label,
                 endpoint = excluded.endpoint,
                 timestamp_trust = excluded.timestamp_trust",
            params![
                reader.id.as_str(),
                reader.label,
                reader.endpoint,
                reader.timestamp_trust.as_str(),
                to_micros(OffsetDateTime::now_utc())?
            ],
        )?;
        Ok(())
    }

    /// Every configured reader, oldest first.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if a row cannot be decoded.
    pub fn readers(&self) -> Result<Vec<Reader>, StorageError> {
        let mut statement = self.conn.prepare(
            "SELECT id, label, endpoint, timestamp_trust FROM readers ORDER BY created_at_us, id",
        )?;
        let mut rows = statement.query([])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            let trust: String = row.get("timestamp_trust")?;
            out.push(Reader {
                id: ReaderId::new(row.get::<_, String>("id")?),
                label: row.get("label")?,
                endpoint: row.get("endpoint")?,
                timestamp_trust: TimestampTrust::parse(&trust).map_err(|expected| {
                    StorageError::Decode(format!("timestamp_trust {trust:?}: {expected}"))
                })?,
            });
        }
        Ok(out)
    }

    /// Maps a reader, or one antenna on it, to the checkpoint it covers.
    ///
    /// `antenna: None` is the reader-wide wildcard.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the write fails, including when the reader or checkpoint
    /// does not exist.
    pub fn map_antenna(
        &mut self,
        reader: &ReaderId,
        antenna: Option<u16>,
        checkpoint: CheckpointId,
    ) -> Result<(), StorageError> {
        // Delete-then-insert rather than an upsert: the two uniqueness rules live in
        // partial indexes (one for exact entries, one for the wildcard), and `ON CONFLICT`
        // needs a single named conflict target it can point at.
        let tx = self.conn.transaction()?;
        match antenna {
            Some(antenna) => {
                tx.execute(
                    "DELETE FROM reader_antennas WHERE reader_id = ?1 AND antenna = ?2",
                    params![reader.as_str(), i64::from(antenna)],
                )?;
            }
            None => {
                tx.execute(
                    "DELETE FROM reader_antennas WHERE reader_id = ?1 AND antenna IS NULL",
                    params![reader.as_str()],
                )?;
            }
        }
        tx.execute(
            "INSERT INTO reader_antennas (reader_id, antenna, checkpoint_id) VALUES (?1, ?2, ?3)",
            params![
                reader.as_str(),
                antenna.map(i64::from),
                checkpoint.to_string()
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// The complete reader and antenna to checkpoint mapping, across every race.
    ///
    /// For display. Derivation wants [`ConfigStore::antenna_map_for`] instead.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if a row cannot be decoded.
    pub fn antenna_map(&self) -> Result<AntennaMap, StorageError> {
        self.read_antenna_map(
            "SELECT reader_id, antenna, checkpoint_id FROM reader_antennas",
            &[],
        )
    }

    /// The mapping restricted to checkpoints belonging to one race.
    ///
    /// Scoped, not global. A database holding a 5K and a criterium has antennas pointing at
    /// both, and an unscoped map would resolve a criterium read to a criterium checkpoint
    /// while deriving the 5K — producing a crossing at a checkpoint that race does not
    /// have. Restricting the map means the read is reported as `unmapped_reader` instead,
    /// which withholds interpretation rather than inventing one.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if a row cannot be decoded.
    pub fn antenna_map_for(&self, race: RaceId) -> Result<AntennaMap, StorageError> {
        self.read_antenna_map(
            "SELECT m.reader_id, m.antenna, m.checkpoint_id
             FROM reader_antennas m
             JOIN checkpoints c ON c.id = m.checkpoint_id
             WHERE c.race_id = ?1",
            &[&race.to_string()],
        )
    }

    fn read_antenna_map(
        &self,
        sql: &str,
        params: &[&dyn rusqlite::ToSql],
    ) -> Result<AntennaMap, StorageError> {
        let mut statement = self.conn.prepare(sql)?;
        let mut rows = statement.query(params)?;
        let mut map = AntennaMap::new();
        while let Some(row) = rows.next()? {
            let reader = ReaderId::new(row.get::<_, String>("reader_id")?);
            let checkpoint = CheckpointId::from_uuid(uuid_column(row, "checkpoint_id")?);
            let antenna: Option<i64> = row.get("antenna")?;
            match antenna {
                Some(antenna) => {
                    let antenna = u16::try_from(antenna)
                        .map_err(|_| StorageError::Decode("antenna out of range".to_owned()))?;
                    map.map_antenna(reader, antenna, checkpoint);
                }
                None => map.map_reader(reader, checkpoint),
            }
        }
        Ok(map)
    }

    // ---- policy -----------------------------------------------------------------

    /// Stores the timing policy for a race.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the policy cannot be serialized or the write fails.
    pub fn set_policy(&mut self, race: RaceId, policy: &TimingPolicy) -> Result<(), StorageError> {
        let json = serde_json::to_string(policy)
            .map_err(|error| StorageError::Decode(format!("serializing timing policy: {error}")))?;
        self.conn.execute(
            "INSERT INTO timing_policies (race_id, policy_json, updated_at_us) VALUES (?1, ?2, ?3)
             ON CONFLICT (race_id) DO UPDATE SET
                 policy_json = excluded.policy_json, updated_at_us = excluded.updated_at_us",
            params![
                race.to_string(),
                json,
                to_micros(OffsetDateTime::now_utc())?
            ],
        )?;
        Ok(())
    }

    /// The timing policy for a race, or the default when none has been set.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the stored JSON cannot be decoded — which means the
    /// database was written by a build with a different policy shape, and silently falling
    /// back to the default would change how every read is deduplicated.
    pub fn policy(&self, race: RaceId) -> Result<TimingPolicy, StorageError> {
        let json: Option<String> = self
            .conn
            .query_row(
                "SELECT policy_json FROM timing_policies WHERE race_id = ?1",
                [race.to_string()],
                |row| row.get(0),
            )
            .optional()?;

        match json {
            Some(json) => serde_json::from_str(&json).map_err(|error| {
                StorageError::Decode(format!("stored timing policy for race {race}: {error}"))
            }),
            None => Ok(TimingPolicy::default()),
        }
    }

    /// Sets which clock a race's placement is measured from.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the write fails.
    pub fn set_start_mode(&mut self, race: RaceId, mode: StartMode) -> Result<(), StorageError> {
        // The row may not exist yet: an operator can choose chip timing before touching any
        // dedup setting. Insert the default policy alongside it rather than silently doing
        // nothing.
        let policy_json = serde_json::to_string(&TimingPolicy::default())
            .map_err(|error| StorageError::Decode(format!("serializing timing policy: {error}")))?;
        self.conn.execute(
            "INSERT INTO timing_policies (race_id, policy_json, updated_at_us, start_mode)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (race_id) DO UPDATE SET
                 start_mode = excluded.start_mode, updated_at_us = excluded.updated_at_us",
            params![
                race.to_string(),
                policy_json,
                to_micros(OffsetDateTime::now_utc())?,
                mode.as_str(),
            ],
        )?;
        Ok(())
    }

    /// Which clock a race's placement is measured from. Defaults to the gun.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the stored value is not a mode this build understands.
    pub fn start_mode(&self, race: RaceId) -> Result<StartMode, StorageError> {
        let stored: Option<String> = self
            .conn
            .query_row(
                "SELECT start_mode FROM timing_policies WHERE race_id = ?1",
                [race.to_string()],
                |row| row.get(0),
            )
            .optional()?;

        match stored {
            Some(value) => StartMode::parse(&value).map_err(|expected| {
                StorageError::Decode(format!("start mode {value:?}: {expected}"))
            }),
            None => Ok(StartMode::default()),
        }
    }

    // ---- race sessions ----------------------------------------------------------

    /// Records a start or a stop. Append-only.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the write fails.
    pub fn record_session(
        &mut self,
        race: RaceId,
        action: SessionAction,
        at: OffsetDateTime,
        actor: &str,
        note: Option<&str>,
    ) -> Result<RaceSession, StorageError> {
        let recorded_at = OffsetDateTime::now_utc();
        self.conn.execute(
            "INSERT INTO race_sessions (race_id, action, at_us, actor, note, recorded_at_us)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                race.to_string(),
                action.as_str(),
                to_micros(at)?,
                actor,
                note,
                to_micros(recorded_at)?
            ],
        )?;
        let seq = u64::try_from(self.conn.last_insert_rowid())
            .map_err(|_| StorageError::Decode("negative session sequence".to_owned()))?;

        Ok(RaceSession {
            seq,
            race,
            action,
            at,
            actor: actor.to_owned(),
            note: note.map(ToOwned::to_owned),
            recorded_at,
        })
    }

    /// A race's recorded starts and stops, oldest first.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if a row cannot be decoded.
    pub fn sessions(&self, race: RaceId) -> Result<Vec<RaceSession>, StorageError> {
        let mut statement = self.conn.prepare(
            "SELECT seq, race_id, action, at_us, actor, note, recorded_at_us
             FROM race_sessions WHERE race_id = ?1 ORDER BY seq",
        )?;
        let mut rows = statement.query([race.to_string()])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            let action: String = row.get("action")?;
            out.push(RaceSession {
                seq: u64::try_from(row.get::<_, i64>("seq")?)
                    .map_err(|_| StorageError::Decode("negative session sequence".to_owned()))?,
                race: RaceId::from_uuid(uuid_column(row, "race_id")?),
                action: SessionAction::parse(&action).map_err(|expected| {
                    StorageError::Decode(format!("session action {action:?}: {expected}"))
                })?,
                at: from_micros(row.get("at_us")?)?,
                actor: row.get("actor")?,
                note: row.get("note")?,
                recorded_at: from_micros(row.get("recorded_at_us")?)?,
            });
        }
        Ok(out)
    }

    // ---- device settings --------------------------------------------------------

    /// The free-space floor below which this device refuses to start a race, in bytes.
    ///
    /// Falls back to [`DEFAULT_MIN_FREE_BYTES`] when unset, and — importantly — also when
    /// the stored value cannot be parsed. A device that has been configured with nonsense
    /// should apply the conservative default rather than no floor at all: the failure mode
    /// of "unreadable setting means unlimited" is a race that fills the card.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the settings table cannot be read.
    pub fn min_free_bytes(&self) -> Result<u64, StorageError> {
        Ok(self
            .device_setting(MIN_FREE_BYTES_KEY)?
            .and_then(|raw| raw.parse::<u64>().ok())
            .unwrap_or(DEFAULT_MIN_FREE_BYTES))
    }

    /// Sets the free-space floor, in bytes.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the write fails.
    pub fn set_min_free_bytes(&mut self, bytes: u64) -> Result<(), StorageError> {
        self.set_device_setting(MIN_FREE_BYTES_KEY, &bytes.to_string())
    }

    /// How long a reader may be silent before the watchdog presumes it gone, in milliseconds.
    ///
    /// Falls back to [`DEFAULT_SILENCE_THRESHOLD_MS`] when unset or unparseable, on the same
    /// reasoning as the free-space floor: a device configured with nonsense should apply the
    /// conservative default rather than switch the check off.
    ///
    /// **Zero disables the watchdog**, and is the one value that means something other than a
    /// duration. An operator who has decided that a false gap is worse than a late one on
    /// their course needs a way to say so, and turning it off explicitly is better than
    /// setting it to a week and hoping.
    ///
    /// The number itself is [Q14](../../../docs/open-questions.md#q14-reader-silence-threshold)
    /// and has no measurement behind it.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the settings table cannot be read.
    pub fn reader_silence_threshold_ms(&self) -> Result<u64, StorageError> {
        Ok(self
            .device_setting(READER_SILENCE_MS_KEY)?
            .and_then(|raw| raw.parse::<u64>().ok())
            .unwrap_or(DEFAULT_SILENCE_THRESHOLD_MS))
    }

    /// Sets the reader-silence threshold, in milliseconds. Zero disables the watchdog.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the write fails.
    pub fn set_reader_silence_threshold_ms(&mut self, ms: u64) -> Result<(), StorageError> {
        self.set_device_setting(READER_SILENCE_MS_KEY, &ms.to_string())
    }

    /// Whether a race has been started and not since stopped.
    ///
    /// The same question [`RaceConfig::is_running`] answers, asked without loading the roster
    /// and the chip assignments to answer it — the silence watchdog asks it on every tick, and
    /// loading an entire race configuration to read one flag would put the whole event on the
    /// journal lock once a minute.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the table cannot be read.
    pub fn race_is_running(&self, race: RaceId) -> Result<bool, StorageError> {
        let last: Option<String> = self
            .conn
            .query_row(
                "SELECT action FROM race_sessions WHERE race_id = ?1 ORDER BY seq DESC LIMIT 1",
                params![race.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        Ok(last.as_deref() == Some(SessionAction::Start.as_str()))
    }

    /// Reads one device setting.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the row cannot be read.
    pub fn device_setting(&self, key: &str) -> Result<Option<String>, StorageError> {
        Ok(self
            .conn
            .query_row(
                "SELECT value FROM device_settings WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Writes one device setting, replacing any previous value.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the write fails.
    pub fn set_device_setting(&mut self, key: &str, value: &str) -> Result<(), StorageError> {
        let now = to_micros(OffsetDateTime::now_utc())?;
        self.conn.execute(
            "INSERT INTO device_settings (key, value, updated_at_us) VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value,
                                            updated_at_us = excluded.updated_at_us",
            params![key, value, now],
        )?;
        Ok(())
    }

    // ---- audit ------------------------------------------------------------------

    /// Appends to the audit trail.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the write fails.
    pub fn record_audit(
        &mut self,
        actor: &str,
        action: &str,
        subject: Option<&str>,
        detail: Option<&str>,
    ) -> Result<(), StorageError> {
        let now = to_micros(OffsetDateTime::now_utc())?;
        self.conn.execute(
            "INSERT INTO audit_log (at_us, actor, action, subject, detail_json, recorded_at_us)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![now, actor, action, subject, detail, now],
        )?;
        Ok(())
    }

    /// The audit trail, newest first.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if a row cannot be decoded.
    pub fn audit_trail(&self, limit: usize) -> Result<Vec<AuditEntry>, StorageError> {
        let mut statement = self.conn.prepare(
            "SELECT seq, at_us, actor, action, subject, detail_json
             FROM audit_log ORDER BY seq DESC LIMIT ?1",
        )?;
        let mut rows = statement.query([i64::try_from(limit).unwrap_or(i64::MAX)])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(AuditEntry {
                seq: u64::try_from(row.get::<_, i64>("seq")?)
                    .map_err(|_| StorageError::Decode("negative audit sequence".to_owned()))?,
                at: from_micros(row.get("at_us")?)?,
                actor: row.get("actor")?,
                action: row.get("action")?,
                subject: row.get("subject")?,
                detail: row.get("detail_json")?,
            });
        }
        Ok(out)
    }

    // ---- the aggregate ----------------------------------------------------------

    /// Assembles everything needed to derive one race's results.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the race does not exist or any part cannot be decoded.
    pub fn load(&self, race: RaceId) -> Result<RaceConfig, StorageError> {
        let race: Race = self
            .conn
            .query_row(
                "SELECT id, event_id, name, scheduled_start_us, expected_laps FROM races
                 WHERE id = ?1",
                [race.to_string()],
                |row| Ok(race_from_row(row)),
            )
            .optional()?
            .transpose()?
            .ok_or_else(|| StorageError::Decode(format!("no race with id {race}")))?;

        let event = self
            .conn
            .query_row(
                "SELECT id, name FROM events WHERE id = ?1",
                [race.event.to_string()],
                |row| {
                    Ok(uuid_column(row, "id").map(|id| Event {
                        id: EventId::from_uuid(id),
                        name: row.get("name").unwrap_or_default(),
                    }))
                },
            )
            .optional()?
            .transpose()?
            .ok_or_else(|| StorageError::Decode(format!("race {} has no event", race.id)))?;

        Ok(RaceConfig {
            checkpoints: self.checkpoints(race.id)?,
            participants: self.participants(race.id)?,
            assignments: self.assignments(race.id)?,
            readers: self.readers()?,
            antennas: self.antenna_map_for(race.id)?,
            policy: self.policy(race.id)?,
            start_mode: self.start_mode(race.id)?,
            sessions: self.sessions(race.id)?,
            event,
            race,
        })
    }

    /// Runs SQLite's own integrity check.
    ///
    /// Returns the messages SQLite produced; an empty vector means the file is sound.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the check cannot be run at all.
    pub fn integrity_check(&self) -> Result<Vec<String>, StorageError> {
        let mut statement = self.conn.prepare("PRAGMA integrity_check")?;
        let mut rows = statement.query([])?;
        let mut problems = Vec::new();
        while let Some(row) = rows.next()? {
            let message: String = row.get(0)?;
            if !message.eq_ignore_ascii_case("ok") {
                problems.push(message);
            }
        }
        Ok(problems)
    }

    /// Runs SQLite's foreign-key check.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the check cannot be run.
    pub fn foreign_key_check(&self) -> Result<Vec<String>, StorageError> {
        let mut statement = self.conn.prepare("PRAGMA foreign_key_check")?;
        let mut rows = statement.query([])?;
        let mut problems = Vec::new();
        while let Some(row) = rows.next()? {
            let table: String = row.get(0)?;
            let parent: String = row.get(2)?;
            problems.push(format!("{table} has a row referencing a missing {parent}"));
        }
        Ok(problems)
    }

    /// Borrows the underlying connection, for the backup path.
    pub(crate) const fn connection(&self) -> &Connection {
        &self.conn
    }
}

fn race_from_row(row: &Row<'_>) -> Result<Race, StorageError> {
    let scheduled: Option<i64> = row.get("scheduled_start_us")?;
    let laps: i64 = row.get("expected_laps")?;
    Ok(Race {
        id: RaceId::from_uuid(uuid_column(row, "id")?),
        event: EventId::from_uuid(uuid_column(row, "event_id")?),
        name: row.get("name")?,
        scheduled_start: scheduled.map(from_micros).transpose()?,
        expected_laps: u16::try_from(laps)
            .map_err(|_| StorageError::Decode("expected_laps out of range".to_owned()))?,
    })
}

fn checkpoint_from_row(row: &Row<'_>) -> Result<Checkpoint, StorageError> {
    let kind: String = row.get("kind")?;
    let sequence: i64 = row.get("sequence")?;
    Ok(Checkpoint {
        id: CheckpointId::from_uuid(uuid_column(row, "id")?),
        race: RaceId::from_uuid(uuid_column(row, "race_id")?),
        name: row.get("name")?,
        kind: kind_from_str(&kind)?,
        sequence: u16::try_from(sequence)
            .map_err(|_| StorageError::Decode("checkpoint sequence out of range".to_owned()))?,
    })
}

fn uuid_column(row: &Row<'_>, column: &str) -> Result<Uuid, StorageError> {
    let text: String = row.get(column)?;
    Uuid::parse_str(&text)
        .map_err(|error| StorageError::Decode(format!("{column} {text:?}: {error}")))
}

// Written out rather than reusing serde's rename, so that changing the JSON shape of
// `CheckpointKind` cannot silently make every stored row unreadable.
const fn kind_to_str(kind: CheckpointKind) -> &'static str {
    match kind {
        CheckpointKind::Start => "start",
        CheckpointKind::Split => "split",
        CheckpointKind::Finish => "finish",
        CheckpointKind::Lap => "lap",
    }
}

fn kind_from_str(text: &str) -> Result<CheckpointKind, StorageError> {
    match text {
        "start" => Ok(CheckpointKind::Start),
        "split" => Ok(CheckpointKind::Split),
        "finish" => Ok(CheckpointKind::Finish),
        "lap" => Ok(CheckpointKind::Lap),
        other => Err(StorageError::Decode(format!(
            "unknown checkpoint kind {other:?}"
        ))),
    }
}

/// Parses an operator-supplied checkpoint kind.
///
/// # Errors
///
/// Returns the accepted values if the input is not one of them.
pub fn parse_checkpoint_kind(value: &str) -> Result<CheckpointKind, &'static str> {
    kind_from_str(&value.trim().to_ascii_lowercase())
        .map_err(|_| "expected one of: start, split, finish, lap")
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::Duration;
    use time::macros::datetime;

    const START: OffsetDateTime = datetime!(2026-04-11 08:00:00 UTC);

    #[test]
    fn an_unset_free_space_floor_is_the_conservative_default() {
        let store = ConfigStore::open_in_memory().expect("open");
        assert_eq!(
            store.min_free_bytes().expect("read"),
            DEFAULT_MIN_FREE_BYTES
        );
    }

    #[test]
    fn the_free_space_floor_round_trips_and_replaces_rather_than_accumulating() {
        let mut store = ConfigStore::open_in_memory().expect("open");
        store.set_min_free_bytes(512 * 1024 * 1024).expect("set");
        assert_eq!(store.min_free_bytes().expect("read"), 512 * 1024 * 1024);

        store
            .set_min_free_bytes(64 * 1024 * 1024)
            .expect("set again");
        assert_eq!(
            store.min_free_bytes().expect("read"),
            64 * 1024 * 1024,
            "a setting is a value, not a log"
        );
    }

    #[test]
    fn an_unparseable_free_space_floor_falls_back_to_the_default_rather_than_to_none() {
        // The dangerous reading of a corrupt setting is "no floor at all", which turns a
        // typo into a race that fills the card. It must fail conservative.
        let mut store = ConfigStore::open_in_memory().expect("open");
        store
            .set_device_setting(MIN_FREE_BYTES_KEY, "plenty")
            .expect("set");
        assert_eq!(
            store.min_free_bytes().expect("read"),
            DEFAULT_MIN_FREE_BYTES
        );
    }

    fn seeded() -> (ConfigStore, Race, Checkpoint) {
        let mut store = ConfigStore::open_in_memory().expect("open");
        let event = Event {
            id: EventId::new(),
            name: "Fixture Day".to_owned(),
        };
        store.save_event(&event).expect("save event");

        let race = Race {
            id: RaceId::new(),
            event: event.id,
            name: "5K".to_owned(),
            scheduled_start: Some(START),
            expected_laps: 1,
        };
        store.save_race(&race).expect("save race");

        let finish = Checkpoint {
            id: CheckpointId::new(),
            race: race.id,
            name: "finish".to_owned(),
            kind: CheckpointKind::Finish,
            sequence: 1,
        };
        store.save_checkpoint(&finish).expect("save checkpoint");

        (store, race, finish)
    }

    fn participant(race: RaceId, bib: &str, name: &str) -> Participant {
        Participant {
            id: ParticipantId::new(),
            race,
            bib: Bib::new(bib),
            name: name.to_owned(),
        }
    }

    #[test]
    fn a_race_survives_a_round_trip_with_every_field_intact() {
        let (store, race, _) = seeded();
        let loaded = store.load(race.id).expect("load");

        assert_eq!(loaded.race, race);
        assert_eq!(loaded.event.name, "Fixture Day");
        assert_eq!(loaded.checkpoints.len(), 1);
        assert_eq!(loaded.policy, TimingPolicy::default());
    }

    #[test]
    fn re_importing_a_roster_keeps_participant_identity() {
        // The property that matters: a late registration must not invalidate the chip
        // assignments and derived timing events of everybody already entered.
        let (mut store, race, _) = seeded();
        store
            .import_participants(&[
                participant(race.id, "101", "Ada Lovelace"),
                participant(race.id, "102", "Alan Turing"),
            ])
            .expect("first import");

        let before = store.participants(race.id).expect("read back");
        let ada_id = before[0].id;

        let summary = store
            .import_participants(&[
                // Same bib, corrected spelling, and a fresh id the caller happened to mint.
                participant(race.id, "101", "Ada Byron Lovelace"),
                participant(race.id, "102", "Alan Turing"),
                participant(race.id, "103", "Grace Hopper"),
            ])
            .expect("second import");

        assert_eq!(summary.inserted, 1);
        assert_eq!(summary.updated, 1);
        assert_eq!(summary.unchanged, 1);
        assert_eq!(summary.total(), 3);

        let after = store.participants(race.id).expect("read back");
        assert_eq!(after.len(), 3);
        assert_eq!(after[0].id, ada_id, "bib 101 must keep the same identity");
        assert_eq!(after[0].name, "Ada Byron Lovelace");
    }

    #[test]
    fn re_importing_the_same_chips_changes_nothing() {
        let (mut store, race, _) = seeded();
        store
            .import_participants(&[participant(race.id, "101", "Ada")])
            .expect("roster");
        let ada = store.participants(race.id).expect("read")[0].id;

        let assignment = ChipAssignment {
            chip: ChipId::new("E280"),
            participant: ada,
            race: race.id,
            valid_from: START - Duration::hours(1),
            valid_until: None,
        };

        let first = store
            .import_assignments(std::slice::from_ref(&assignment))
            .expect("import");
        assert_eq!(first.inserted, 1);

        let second = store
            .import_assignments(std::slice::from_ref(&assignment))
            .expect("re-import");
        assert_eq!(second.unchanged, 1);
        assert_eq!(second.inserted, 0);
        assert_eq!(store.assignments(race.id).expect("read").len(), 1);
    }

    #[test]
    fn swapping_a_failed_chip_updates_rather_than_duplicates() {
        let (mut store, race, _) = seeded();
        store
            .import_participants(&[
                participant(race.id, "101", "Ada"),
                participant(race.id, "102", "Alan"),
            ])
            .expect("roster");
        let roster = store.participants(race.id).expect("read");

        let mut assignment = ChipAssignment {
            chip: ChipId::new("E280"),
            participant: roster[0].id,
            race: race.id,
            valid_from: START,
            valid_until: None,
        };
        store
            .import_assignments(std::slice::from_ref(&assignment))
            .expect("import");

        assignment.participant = roster[1].id;
        let summary = store
            .import_assignments(std::slice::from_ref(&assignment))
            .expect("reassign");

        assert_eq!(summary.updated, 1);
        let stored = store.assignments(race.id).expect("read");
        assert_eq!(
            stored.len(),
            1,
            "reassignment must not duplicate the window"
        );
        assert_eq!(stored[0].participant, roster[1].id);
    }

    #[test]
    fn the_antenna_map_round_trips_wildcards_and_exact_entries() {
        let (mut store, race, finish) = seeded();
        let start = Checkpoint {
            id: CheckpointId::new(),
            race: race.id,
            name: "start".to_owned(),
            kind: CheckpointKind::Start,
            sequence: 0,
        };
        store.save_checkpoint(&start).expect("save");

        let reader = ReaderId::new("mat");
        store
            .save_reader(&Reader {
                id: reader.clone(),
                label: "Finish mat".to_owned(),
                endpoint: None,
                timestamp_trust: TimestampTrust::Auto,
            })
            .expect("save reader");

        store
            .map_antenna(&reader, None, start.id)
            .expect("wildcard");
        store
            .map_antenna(&reader, Some(2), finish.id)
            .expect("exact");

        let map = store.antenna_map().expect("read map");
        assert_eq!(map.len(), 2);
        assert_eq!(map.resolve(&reader, Some(2)), Some(finish.id));
        assert_eq!(map.resolve(&reader, Some(1)), Some(start.id));
        assert_eq!(map.resolve(&reader, None), Some(start.id));
    }

    #[test]
    fn remapping_an_antenna_replaces_rather_than_accumulates() {
        let (mut store, race, finish) = seeded();
        let start = Checkpoint {
            id: CheckpointId::new(),
            race: race.id,
            name: "start".to_owned(),
            kind: CheckpointKind::Start,
            sequence: 0,
        };
        store.save_checkpoint(&start).expect("save");

        let reader = ReaderId::new("mat");
        store
            .save_reader(&Reader {
                id: reader.clone(),
                label: "Mat".to_owned(),
                endpoint: None,
                timestamp_trust: TimestampTrust::Auto,
            })
            .expect("save reader");

        store.map_antenna(&reader, Some(1), start.id).expect("map");
        store
            .map_antenna(&reader, Some(1), finish.id)
            .expect("remap");
        store
            .map_antenna(&reader, None, start.id)
            .expect("wildcard");
        store.map_antenna(&reader, None, finish.id).expect("rewild");

        let map = store.antenna_map().expect("read map");
        assert_eq!(map.len(), 2, "one exact entry and one wildcard, not four");
        assert_eq!(map.resolve(&reader, Some(1)), Some(finish.id));
        assert_eq!(map.resolve(&reader, None), Some(finish.id));
    }

    #[test]
    fn a_policy_survives_a_round_trip_through_json() {
        use splitforge_domain::{CheckpointPolicy, SelectionRule};

        let (mut store, race, finish) = seeded();
        let policy = TimingPolicy::default().with_checkpoint(
            finish.id,
            CheckpointPolicy {
                min_interval_ms: Some(4_500),
                selection_rule: Some(SelectionRule::FirstAboveRssi { floor_dbm: -62 }),
            },
        );
        store.set_policy(race.id, &policy).expect("set");

        assert_eq!(store.policy(race.id).expect("read"), policy);
        assert_eq!(store.load(race.id).expect("load").policy, policy);
    }

    #[test]
    fn the_gun_time_is_the_most_recent_start() {
        let (mut store, race, _) = seeded();
        let config = store.load(race.id).expect("load");
        assert_eq!(
            config.gun_time(),
            Some(START),
            "with nothing recorded, the scheduled start stands in"
        );
        assert!(!config.is_running());

        store
            .record_session(race.id, SessionAction::Start, START, "phillip", None)
            .expect("start");
        let restart = START + Duration::minutes(12);
        store
            .record_session(
                race.id,
                SessionAction::Start,
                restart,
                "phillip",
                Some("false start, field recalled"),
            )
            .expect("restart");

        let config = store.load(race.id).expect("reload");
        assert_eq!(config.gun_time(), Some(restart));
        assert!(config.is_running());
        assert_eq!(
            config.sessions.len(),
            2,
            "the first start is superseded, never erased"
        );

        store
            .record_session(
                race.id,
                SessionAction::Stop,
                restart + Duration::hours(1),
                "phillip",
                None,
            )
            .expect("stop");
        assert!(!store.load(race.id).expect("reload").is_running());
    }

    #[test]
    fn the_audit_trail_reads_newest_first() {
        let (mut store, _, _) = seeded();
        store
            .record_audit("phillip", "roster.import", Some("5K"), None)
            .expect("audit");
        store
            .record_audit("phillip", "policy.set", Some("5K"), Some(r#"{"a":1}"#))
            .expect("audit");

        let trail = store.audit_trail(10).expect("read");
        assert_eq!(trail.len(), 2);
        assert_eq!(trail[0].action, "policy.set");
        assert_eq!(trail[0].detail.as_deref(), Some(r#"{"a":1}"#));
        assert_eq!(trail[1].action, "roster.import");
    }

    #[test]
    fn resolving_a_race_refuses_to_guess_between_several() {
        let (mut store, race, _) = seeded();
        assert!(matches!(
            store.resolve_race(None).expect("resolve"),
            RaceSelection::One(_)
        ));

        store
            .save_race(&Race {
                id: RaceId::new(),
                event: race.event,
                name: "10K".to_owned(),
                scheduled_start: None,
                expected_laps: 1,
            })
            .expect("second race");

        assert!(matches!(
            store.resolve_race(None).expect("resolve"),
            RaceSelection::Ambiguous(races) if races.len() == 2
        ));
        assert!(matches!(
            store.resolve_race(Some("10K")).expect("resolve"),
            RaceSelection::One(race) if race.name == "10K"
        ));
        assert!(matches!(
            store.resolve_race(Some("5k")).expect("resolve"),
            RaceSelection::One(race) if race.name == "5K"
        ));
        assert!(matches!(
            store.resolve_race(Some("marathon")).expect("resolve"),
            RaceSelection::None
        ));
    }

    #[test]
    fn a_checkpoint_on_a_race_that_does_not_exist_is_refused() {
        let (mut store, _, _) = seeded();
        // Foreign keys are ON, so this is caught by the database rather than surfacing
        // later as a crossing that resolves to nothing.
        store
            .save_checkpoint(&Checkpoint {
                id: CheckpointId::new(),
                race: RaceId::new(),
                name: "phantom".to_owned(),
                kind: CheckpointKind::Split,
                sequence: 0,
            })
            .expect_err("an orphan checkpoint must be refused");
    }

    #[test]
    fn a_fresh_database_passes_its_own_integrity_checks() {
        let (store, _, _) = seeded();
        assert!(store.integrity_check().expect("integrity").is_empty());
        assert!(store.foreign_key_check().expect("foreign keys").is_empty());
    }

    #[test]
    fn checkpoint_kinds_round_trip_through_their_stored_names() {
        for kind in [
            CheckpointKind::Start,
            CheckpointKind::Split,
            CheckpointKind::Finish,
            CheckpointKind::Lap,
        ] {
            assert_eq!(parse_checkpoint_kind(kind_to_str(kind)), Ok(kind));
        }
        assert!(parse_checkpoint_kind("halfway").is_err());
    }

    #[test]
    fn an_unset_silence_threshold_is_the_conservative_default() {
        let store = ConfigStore::open_in_memory().expect("open");
        assert_eq!(
            store.reader_silence_threshold_ms().expect("read"),
            DEFAULT_SILENCE_THRESHOLD_MS
        );
    }

    #[test]
    fn the_silence_threshold_round_trips_and_zero_survives_as_zero() {
        // Zero is the one value that means something other than a duration — the operator
        // switching the check off — so it must not be mistaken for "unset" and replaced by
        // the default on the way back out.
        let mut store = ConfigStore::open_in_memory().expect("open");
        store.set_reader_silence_threshold_ms(30_000).expect("set");
        assert_eq!(store.reader_silence_threshold_ms().expect("read"), 30_000);

        store.set_reader_silence_threshold_ms(0).expect("disable");
        assert_eq!(store.reader_silence_threshold_ms().expect("read"), 0);
    }

    #[test]
    fn an_unparseable_silence_threshold_falls_back_rather_than_switching_the_check_off() {
        // The same rule as the free-space floor: nonsense in the settings table must not
        // become "no check at all", because that failure is silent for a whole event.
        let mut store = ConfigStore::open_in_memory().expect("open");
        store
            .set_device_setting("reader_silence_threshold_ms", "soon")
            .expect("write nonsense");
        assert_eq!(
            store.reader_silence_threshold_ms().expect("read"),
            DEFAULT_SILENCE_THRESHOLD_MS
        );
    }

    #[test]
    fn a_race_is_running_only_between_a_start_and_the_next_stop() {
        // The cheap query the silence watchdog asks every tick, checked against the same
        // answer `RaceConfig::is_running` gives after loading the whole configuration.
        let (mut store, race, _) = seeded();
        assert!(!store.race_is_running(race.id).expect("read"));

        store
            .record_session(race.id, SessionAction::Start, START, "phillip", None)
            .expect("start");
        assert!(store.race_is_running(race.id).expect("read"));
        assert_eq!(
            store.race_is_running(race.id).expect("read"),
            store.load(race.id).expect("load").is_running(),
            "the cheap query and the loaded one must not disagree"
        );

        store
            .record_session(
                race.id,
                SessionAction::Stop,
                START + Duration::hours(1),
                "phillip",
                None,
            )
            .expect("stop");
        assert!(!store.race_is_running(race.id).expect("read"));
        assert_eq!(
            store.race_is_running(race.id).expect("read"),
            store.load(race.id).expect("load").is_running()
        );
    }
}
