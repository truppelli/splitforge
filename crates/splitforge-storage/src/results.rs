//! Published results and the operator declarations that shape them.
//!
//! The third store in this crate, alongside [`crate::SqliteJournal`] (evidence that never
//! changes) and [`crate::ConfigStore`] (configuration that changes constantly). Results are
//! neither: they are **derivations that get frozen at the moment somebody publishes them**.
//!
//! Publishing is therefore the only write. There is no `update_revision`, no
//! `set_place`, no `correct_entry` — not as an oversight, but because the absence of those
//! methods is the guarantee. A correction is a new revision, and the old one stays exactly
//! as it was published (`docs/timing-model.md` § 7).

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, Row, params};
use splitforge_domain::{
    Bib, ParticipantId, RaceId, ResultEntry, ResultFlag, ResultRevision, ResultRevisionId,
    ResultStatus, RevisionStatus, ScoringPolicy, StatusDeclaration, StatusDeclarationId,
    StatusSource, TimingEventId,
};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::StorageError;
use crate::connection::{self, from_micros, to_micros};

/// A revision's header, without loading its entries.
///
/// What `splitforge results list` shows: enough to see the shape of the race's history
/// without pulling every row of every revision into memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevisionSummary {
    /// Unique identifier.
    pub id: ResultRevisionId,
    /// Revision number within the race.
    pub number: u32,
    /// Whether more corrections are expected.
    pub status: RevisionStatus,
    /// When it was published.
    pub generated_at: OffsetDateTime,
    /// Who published it.
    pub actor: String,
    /// Why.
    pub reason: String,
    /// Fingerprint of the scored rows.
    pub digest: String,
    /// How many entries it holds.
    pub entries: usize,
}

/// Reads and writes results.
#[derive(Debug)]
pub struct ResultStore {
    conn: Connection,
}

impl ResultStore {
    /// Opens the store at `path`, creating and migrating the database as needed.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the database cannot be opened or migrated.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        Ok(Self {
            conn: connection::open(path)?,
        })
    }

    /// Opens a private in-memory store, for tests.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the database cannot be created.
    pub fn open_in_memory() -> Result<Self, StorageError> {
        Ok(Self {
            conn: connection::open_in_memory()?,
        })
    }

    // ---- declarations -----------------------------------------------------------

    /// Records an operator's status declaration.
    ///
    /// Always an append. Reversing an earlier declaration means calling this again with the
    /// new status — the first row stays, and scoring uses the newest.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the write fails.
    pub fn declare_status(
        &mut self,
        race: RaceId,
        participant: ParticipantId,
        status: ResultStatus,
        reason: &str,
        actor: &str,
        declared_at: OffsetDateTime,
    ) -> Result<StatusDeclaration, StorageError> {
        let id = StatusDeclarationId::new();
        let recorded_at = OffsetDateTime::now_utc();
        self.conn.execute(
            "INSERT INTO status_declarations
                 (id, race_id, participant_id, status, reason, actor, declared_at_us,
                  recorded_at_us)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                id.to_string(),
                race.to_string(),
                participant.to_string(),
                status.as_str(),
                reason,
                actor,
                to_micros(declared_at)?,
                to_micros(recorded_at)?,
            ],
        )?;
        let seq = u64::try_from(self.conn.last_insert_rowid())
            .map_err(|_| StorageError::Decode("negative declaration sequence".to_owned()))?;

        Ok(StatusDeclaration {
            id,
            seq,
            race,
            participant,
            status,
            reason: reason.to_owned(),
            actor: actor.to_owned(),
            declared_at,
        })
    }

    /// Every declaration for a race, oldest first.
    ///
    /// All of them, not just the ones in force. The superseded rows are the audit trail
    /// that explains how a result reached its current state.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if a row cannot be decoded.
    pub fn declarations(&self, race: RaceId) -> Result<Vec<StatusDeclaration>, StorageError> {
        let mut statement = self.conn.prepare(
            "SELECT seq, id, race_id, participant_id, status, reason, actor, declared_at_us
             FROM status_declarations WHERE race_id = ?1 ORDER BY seq",
        )?;
        let mut rows = statement.query([race.to_string()])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            let status: String = row.get("status")?;
            out.push(StatusDeclaration {
                id: StatusDeclarationId::from_uuid(uuid_column(row, "id")?),
                seq: u64::try_from(row.get::<_, i64>("seq")?).map_err(|_| {
                    StorageError::Decode("negative declaration sequence".to_owned())
                })?,
                race: RaceId::from_uuid(uuid_column(row, "race_id")?),
                participant: ParticipantId::from_uuid(uuid_column(row, "participant_id")?),
                status: ResultStatus::parse(&status).map_err(|expected| {
                    StorageError::Decode(format!("result status {status:?}: {expected}"))
                })?,
                reason: row.get("reason")?,
                actor: row.get("actor")?,
                declared_at: from_micros(row.get("declared_at_us")?)?,
            });
        }
        Ok(out)
    }

    // ---- revisions --------------------------------------------------------------

    /// The number the next revision of this race will take, starting at 1.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the query fails.
    pub fn next_revision_number(&self, race: RaceId) -> Result<u32, StorageError> {
        let highest: Option<i64> = self.conn.query_row(
            "SELECT MAX(number) FROM result_revisions WHERE race_id = ?1",
            [race.to_string()],
            |row| row.get(0),
        )?;
        Ok(u32::try_from(highest.unwrap_or(0))
            .map_err(|_| StorageError::Decode("negative revision number".to_owned()))?
            .saturating_add(1))
    }

    /// Writes a revision and its entries.
    ///
    /// One transaction: a half-written results sheet is worse than none, because it looks
    /// like a complete one.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the write fails, including when the revision number
    /// already exists for that race.
    pub fn publish(&mut self, revision: &ResultRevision) -> Result<(), StorageError> {
        let policy_json = serde_json::to_string(&revision.policy).map_err(|error| {
            StorageError::Decode(format!("serializing scoring policy: {error}"))
        })?;
        let digest = revision.digest();
        let recorded_at = to_micros(OffsetDateTime::now_utc())?;

        let transaction = self.conn.transaction()?;
        transaction.execute(
            "INSERT INTO result_revisions
                 (id, race_id, number, status, generated_at_us, actor, reason, policy_json,
                  digest, recorded_at_us)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                revision.id.to_string(),
                revision.race.to_string(),
                i64::from(revision.number),
                revision.status.as_str(),
                to_micros(revision.generated_at)?,
                revision.actor,
                revision.reason,
                policy_json,
                digest,
                recorded_at,
            ],
        )?;

        for (ordinal, entry) in revision.entries.iter().enumerate() {
            let flags_json = serde_json::to_string(&entry.flags)
                .map_err(|error| StorageError::Decode(format!("serializing flags: {error}")))?;
            let events_json = serde_json::to_string(&entry.timing_events).map_err(|error| {
                StorageError::Decode(format!("serializing timing events: {error}"))
            })?;
            transaction.execute(
                "INSERT INTO result_entries
                     (revision_id, participant_id, ordinal, bib, name, status, status_source,
                      status_reason, start_at_us, finish_at_us, gun_time_ms, chip_time_ms,
                      scoring_time_ms, place, flags_json, timing_events_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
                params![
                    revision.id.to_string(),
                    entry.participant.to_string(),
                    i64::try_from(ordinal).unwrap_or(i64::MAX),
                    entry.bib.as_str(),
                    entry.name,
                    entry.status.as_str(),
                    source_to_str(entry.status_source),
                    entry.status_reason,
                    entry.start_at.map(to_micros).transpose()?,
                    entry.finish_at.map(to_micros).transpose()?,
                    entry.gun_time_ms,
                    entry.chip_time_ms,
                    entry.scoring_time_ms,
                    entry.place.map(i64::from),
                    flags_json,
                    events_json,
                ],
            )?;
        }

        transaction.commit()?;
        Ok(())
    }

    /// Every revision of a race, oldest first, headers only.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if a row cannot be decoded.
    pub fn revisions(&self, race: RaceId) -> Result<Vec<RevisionSummary>, StorageError> {
        let mut statement = self.conn.prepare(
            "SELECT r.id, r.number, r.status, r.generated_at_us, r.actor, r.reason, r.digest,
                    (SELECT COUNT(*) FROM result_entries e WHERE e.revision_id = r.id) AS entries
             FROM result_revisions r WHERE r.race_id = ?1 ORDER BY r.number",
        )?;
        let mut rows = statement.query([race.to_string()])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(summary_from_row(row)?);
        }
        Ok(out)
    }

    /// One revision by number, with its entries.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if a row cannot be decoded.
    pub fn revision(
        &self,
        race: RaceId,
        number: u32,
    ) -> Result<Option<ResultRevision>, StorageError> {
        let header = self
            .conn
            .query_row(
                "SELECT id, race_id, number, status, generated_at_us, actor, reason, policy_json
                 FROM result_revisions WHERE race_id = ?1 AND number = ?2",
                params![race.to_string(), i64::from(number)],
                |row| {
                    Ok((
                        row.get::<_, String>("id")?,
                        row.get::<_, String>("status")?,
                        row.get::<_, i64>("generated_at_us")?,
                        row.get::<_, String>("actor")?,
                        row.get::<_, String>("reason")?,
                        row.get::<_, String>("policy_json")?,
                    ))
                },
            )
            .optional()?;

        let Some((id, status, generated_at_us, actor, reason, policy_json)) = header else {
            return Ok(None);
        };

        let status = RevisionStatus::parse(&status).map_err(|expected| {
            StorageError::Decode(format!("revision status {status:?}: {expected}"))
        })?;
        let policy: ScoringPolicy = serde_json::from_str(&policy_json).map_err(|error| {
            StorageError::Decode(format!("stored scoring policy is unreadable: {error}"))
        })?;
        let id = ResultRevisionId::from_uuid(
            Uuid::parse_str(&id)
                .map_err(|error| StorageError::Decode(format!("revision id {id:?}: {error}")))?,
        );

        Ok(Some(ResultRevision {
            id,
            race,
            number,
            status,
            generated_at: from_micros(generated_at_us)?,
            actor,
            reason,
            policy,
            entries: self.entries(id)?,
        }))
    }

    /// The most recently published revision of a race, with its entries.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if a row cannot be decoded.
    pub fn latest_revision(&self, race: RaceId) -> Result<Option<ResultRevision>, StorageError> {
        let highest: Option<i64> = self.conn.query_row(
            "SELECT MAX(number) FROM result_revisions WHERE race_id = ?1",
            [race.to_string()],
            |row| row.get(0),
        )?;
        match highest {
            Some(number) => {
                let number = u32::try_from(number)
                    .map_err(|_| StorageError::Decode("negative revision number".to_owned()))?;
                self.revision(race, number)
            }
            None => Ok(None),
        }
    }

    /// The entries of one revision, in publication order.
    fn entries(&self, revision: ResultRevisionId) -> Result<Vec<ResultEntry>, StorageError> {
        let mut statement = self.conn.prepare(
            "SELECT participant_id, bib, name, status, status_source, status_reason,
                    start_at_us, finish_at_us, gun_time_ms, chip_time_ms, scoring_time_ms,
                    place, flags_json, timing_events_json
             FROM result_entries WHERE revision_id = ?1 ORDER BY ordinal",
        )?;
        let mut rows = statement.query([revision.to_string()])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(entry_from_row(row)?);
        }
        Ok(out)
    }
}

fn summary_from_row(row: &Row<'_>) -> Result<RevisionSummary, StorageError> {
    let status: String = row.get("status")?;
    Ok(RevisionSummary {
        id: ResultRevisionId::from_uuid(uuid_column(row, "id")?),
        number: u32::try_from(row.get::<_, i64>("number")?)
            .map_err(|_| StorageError::Decode("negative revision number".to_owned()))?,
        status: RevisionStatus::parse(&status).map_err(|expected| {
            StorageError::Decode(format!("revision status {status:?}: {expected}"))
        })?,
        generated_at: from_micros(row.get("generated_at_us")?)?,
        actor: row.get("actor")?,
        reason: row.get("reason")?,
        digest: row.get("digest")?,
        entries: usize::try_from(row.get::<_, i64>("entries")?)
            .map_err(|_| StorageError::Decode("negative entry count".to_owned()))?,
    })
}

fn entry_from_row(row: &Row<'_>) -> Result<ResultEntry, StorageError> {
    let status: String = row.get("status")?;
    let source: String = row.get("status_source")?;
    let flags_json: String = row.get("flags_json")?;
    let events_json: String = row.get("timing_events_json")?;

    let flags: Vec<ResultFlag> = serde_json::from_str(&flags_json)
        .map_err(|error| StorageError::Decode(format!("stored flags are unreadable: {error}")))?;
    let timing_events: Vec<TimingEventId> =
        serde_json::from_str(&events_json).map_err(|error| {
            StorageError::Decode(format!("stored timing events are unreadable: {error}"))
        })?;

    let start_at: Option<i64> = row.get("start_at_us")?;
    let finish_at: Option<i64> = row.get("finish_at_us")?;
    let place: Option<i64> = row.get("place")?;

    Ok(ResultEntry {
        participant: ParticipantId::from_uuid(uuid_column(row, "participant_id")?),
        bib: Bib::new(row.get::<_, String>("bib")?),
        name: row.get("name")?,
        status: ResultStatus::parse(&status).map_err(|expected| {
            StorageError::Decode(format!("result status {status:?}: {expected}"))
        })?,
        status_source: parse_source(&source)?,
        status_reason: row.get("status_reason")?,
        start_at: start_at.map(from_micros).transpose()?,
        finish_at: finish_at.map(from_micros).transpose()?,
        gun_time_ms: row.get("gun_time_ms")?,
        chip_time_ms: row.get("chip_time_ms")?,
        scoring_time_ms: row.get("scoring_time_ms")?,
        place: place
            .map(|value| {
                u32::try_from(value).map_err(|_| StorageError::Decode("negative place".to_owned()))
            })
            .transpose()?,
        flags,
        timing_events,
    })
}

// Written out rather than reusing serde's rename, so that changing the JSON shape of
// `StatusSource` cannot silently make every stored row unreadable.
const fn source_to_str(source: StatusSource) -> &'static str {
    match source {
        StatusSource::Derived => "derived",
        StatusSource::Declared => "declared",
    }
}

fn parse_source(value: &str) -> Result<StatusSource, StorageError> {
    match value {
        "derived" => Ok(StatusSource::Derived),
        "declared" => Ok(StatusSource::Declared),
        other => Err(StorageError::Decode(format!(
            "status source {other:?}: expected one of: derived, declared"
        ))),
    }
}

fn uuid_column(row: &Row<'_>, column: &str) -> Result<Uuid, StorageError> {
    let text: String = row.get(column)?;
    Uuid::parse_str(&text)
        .map_err(|error| StorageError::Decode(format!("{column} {text:?}: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use splitforge_domain::{ParticipantId, StartMode};

    fn revision(race: RaceId, number: u32, entries: Vec<ResultEntry>) -> ResultRevision {
        ResultRevision {
            id: ResultRevisionId::new(),
            race,
            number,
            status: RevisionStatus::Provisional,
            generated_at: OffsetDateTime::UNIX_EPOCH,
            actor: "operator".to_owned(),
            reason: "initial".to_owned(),
            policy: ScoringPolicy {
                start_mode: StartMode::Chip,
                timing: splitforge_domain::TimingPolicy::default(),
                gun_time: Some(OffsetDateTime::UNIX_EPOCH),
            },
            entries,
        }
    }

    fn entry(participant: ParticipantId, bib: &str) -> ResultEntry {
        ResultEntry {
            participant,
            bib: Bib::new(bib),
            name: format!("Runner {bib}"),
            status: ResultStatus::Finished,
            status_source: StatusSource::Derived,
            status_reason: None,
            start_at: Some(OffsetDateTime::UNIX_EPOCH),
            finish_at: Some(OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(1_200)),
            gun_time_ms: Some(1_200_000),
            chip_time_ms: Some(1_190_000),
            scoring_time_ms: Some(1_190_000),
            place: Some(1),
            flags: vec![ResultFlag::NoStartReadUnderChipTime],
            timing_events: vec![TimingEventId::new()],
        }
    }

    /// A store with one race and one participant, satisfying the foreign keys.
    fn store_with_race() -> (ResultStore, RaceId, ParticipantId) {
        let store = ResultStore::open_in_memory().expect("open");
        let race = RaceId::new();
        let event = splitforge_domain::EventId::new();
        let participant = ParticipantId::new();
        store
            .conn
            .execute(
                "INSERT INTO events (id, name, created_at_us) VALUES (?1, 'day', 0)",
                [event.to_string()],
            )
            .expect("event");
        store
            .conn
            .execute(
                "INSERT INTO races (id, event_id, name, expected_laps, created_at_us)
                 VALUES (?1, ?2, '5K', 1, 0)",
                params![race.to_string(), event.to_string()],
            )
            .expect("race");
        store
            .conn
            .execute(
                "INSERT INTO participants (id, race_id, bib, name) VALUES (?1, ?2, '101', 'R')",
                params![participant.to_string(), race.to_string()],
            )
            .expect("participant");
        (store, race, participant)
    }

    #[test]
    fn a_published_revision_round_trips_exactly() {
        let (mut store, race, participant) = store_with_race();
        let published = revision(race, 1, vec![entry(participant, "101")]);

        store.publish(&published).expect("publish");
        let back = store
            .revision(race, 1)
            .expect("read")
            .expect("revision exists");

        assert_eq!(back, published, "a revision must survive storage unchanged");
    }

    #[test]
    fn revision_numbers_start_at_one_and_increment() {
        let (mut store, race, participant) = store_with_race();
        assert_eq!(store.next_revision_number(race).expect("first"), 1);

        store
            .publish(&revision(race, 1, vec![entry(participant, "101")]))
            .expect("publish 1");
        assert_eq!(store.next_revision_number(race).expect("second"), 2);

        store
            .publish(&revision(race, 2, vec![entry(participant, "101")]))
            .expect("publish 2");
        assert_eq!(store.next_revision_number(race).expect("third"), 3);
    }

    #[test]
    fn a_published_revision_cannot_be_rewritten() {
        let (mut store, race, participant) = store_with_race();
        store
            .publish(&revision(race, 1, vec![entry(participant, "101")]))
            .expect("publish");

        // The whole point of the milestone: revision 1 was published, and nothing may
        // quietly change what it said.
        let update = store.conn.execute(
            "UPDATE result_revisions SET reason = 'rewritten' WHERE race_id = ?1",
            [race.to_string()],
        );
        assert!(update.is_err(), "a published revision must not be editable");
        assert!(
            update.unwrap_err().to_string().contains("immutable"),
            "the error should say why"
        );

        let entry_update = store
            .conn
            .execute("UPDATE result_entries SET place = 99 WHERE bib = '101'", []);
        assert!(entry_update.is_err(), "nor may a single place be edited");

        let delete = store.conn.execute(
            "DELETE FROM result_revisions WHERE race_id = ?1",
            [race.to_string()],
        );
        assert!(delete.is_err(), "nor deleted");
    }

    #[test]
    fn two_revisions_cannot_share_a_number() {
        let (mut store, race, participant) = store_with_race();
        store
            .publish(&revision(race, 1, vec![entry(participant, "101")]))
            .expect("publish");
        assert!(
            store
                .publish(&revision(race, 1, vec![entry(participant, "101")]))
                .is_err(),
            "'revision 1' must name one thing forever"
        );
    }

    #[test]
    fn declarations_accumulate_rather_than_replace() {
        let (mut store, race, participant) = store_with_race();
        store
            .declare_status(
                race,
                participant,
                ResultStatus::Dq,
                "course cut",
                "referee",
                OffsetDateTime::UNIX_EPOCH,
            )
            .expect("dq");
        store
            .declare_status(
                race,
                participant,
                ResultStatus::Finished,
                "appeal upheld",
                "referee",
                OffsetDateTime::UNIX_EPOCH + time::Duration::minutes(27),
            )
            .expect("reinstate");

        let declarations = store.declarations(race).expect("read");
        assert_eq!(
            declarations.len(),
            2,
            "reversing a DQ appends; it does not erase"
        );
        assert_eq!(declarations[0].status, ResultStatus::Dq);
        assert_eq!(declarations[1].status, ResultStatus::Finished);
        assert!(
            declarations[0].seq < declarations[1].seq,
            "sequence decides which is in force"
        );
    }

    #[test]
    fn a_declaration_cannot_be_rewritten() {
        let (mut store, race, participant) = store_with_race();
        store
            .declare_status(
                race,
                participant,
                ResultStatus::Dq,
                "course cut",
                "referee",
                OffsetDateTime::UNIX_EPOCH,
            )
            .expect("dq");

        let update = store
            .conn
            .execute("UPDATE status_declarations SET reason = 'never mind'", []);
        assert!(update.is_err(), "somebody said this; it stands as evidence");
        assert!(
            store
                .conn
                .execute("DELETE FROM status_declarations", [])
                .is_err()
        );
    }

    #[test]
    fn revisions_are_listed_oldest_first_with_their_entry_counts() {
        let (mut store, race, participant) = store_with_race();
        store
            .publish(&revision(race, 1, vec![entry(participant, "101")]))
            .expect("publish 1");
        let mut second = revision(race, 2, vec![entry(participant, "101")]);
        second.status = RevisionStatus::Final;
        second.reason = "bib 101 disqualified".to_owned();
        store.publish(&second).expect("publish 2");

        let listed = store.revisions(race).expect("list");
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].number, 1);
        assert_eq!(listed[0].status, RevisionStatus::Provisional);
        assert_eq!(listed[0].entries, 1);
        assert_eq!(listed[1].number, 2);
        assert_eq!(listed[1].status, RevisionStatus::Final);
        assert_eq!(listed[1].reason, "bib 101 disqualified");
    }

    #[test]
    fn the_latest_revision_is_the_highest_numbered_one() {
        let (mut store, race, participant) = store_with_race();
        assert!(store.latest_revision(race).expect("none yet").is_none());

        store
            .publish(&revision(race, 1, vec![entry(participant, "101")]))
            .expect("publish 1");
        let mut second = revision(race, 2, vec![entry(participant, "101")]);
        second.reason = "correction".to_owned();
        store.publish(&second).expect("publish 2");

        let latest = store.latest_revision(race).expect("read").expect("exists");
        assert_eq!(latest.number, 2);
        assert_eq!(latest.reason, "correction");
    }

    #[test]
    fn an_unknown_revision_number_is_none_rather_than_an_error() {
        let (store, race, _) = store_with_race();
        assert!(store.revision(race, 7).expect("query succeeds").is_none());
    }
}
