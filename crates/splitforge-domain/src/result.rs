//! Results: statuses, placement, and the immutable revisions that publish them.
//!
//! # Three kinds of record, and why the distinction matters
//!
//! | | [`StatusDeclaration`] | [`ResultEntry`] | [`ResultRevision`] |
//! |---|---|---|---|
//! | What | An operator said so | What scoring concluded | A publication |
//! | Kind | Evidence | Derivation | Act |
//! | Mutability | Append-only | Recomputed freely | Frozen once published |
//!
//! A disqualification is not a conclusion SplitForge can reach on its own — somebody
//! decided it, and the record of that decision is evidence in exactly the way a reader
//! report is. So a DQ is never a column that gets overwritten; it is a row that gets added,
//! and reversing it adds another row rather than deleting the first.
//!
//! A revision is frozen for a blunter reason: it was published. Someone screenshotted it,
//! someone posted it, someone drove home believing it. Superseding that is honest;
//! rewriting it is not. See `docs/timing-model.md` § 7.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::derived::derived_uuid;
use crate::ids::{
    Bib, ParticipantId, RaceId, ResultRevisionId, StatusDeclarationId, TimingEventId,
};
use crate::policy::TimingPolicy;

/// How a participant's race ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultStatus {
    /// Crossed the finish line. The only status that receives a place.
    Finished,
    /// Did not start. Registered, but no crossing anywhere on the course.
    Dns,
    /// Did not finish. Started, but never reached the finish.
    Dnf,
    /// Disqualified. Only ever declared by an operator — never derived.
    Dq,
}

impl ResultStatus {
    /// Parses an operator-supplied name.
    ///
    /// # Errors
    ///
    /// Returns the accepted values if the input is not one of them. A status is the single
    /// most consequential field an operator types, so a near-miss is refused rather than
    /// guessed.
    pub fn parse(value: &str) -> Result<Self, &'static str> {
        match value.trim().to_ascii_lowercase().as_str() {
            "finished" => Ok(Self::Finished),
            "dns" => Ok(Self::Dns),
            "dnf" => Ok(Self::Dnf),
            "dq" => Ok(Self::Dq),
            _ => Err("expected one of: finished, dns, dnf, dq"),
        }
    }

    /// The stable name used in storage, JSON, and CSV.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Finished => "finished",
            Self::Dns => "dns",
            Self::Dnf => "dnf",
            Self::Dq => "dq",
        }
    }

    /// Whether this status competes for a place.
    ///
    /// Only [`ResultStatus::Finished`] does. A disqualified runner keeps their measured
    /// times — the clock did not lie about when they crossed — but takes no place, and
    /// everyone behind them moves up.
    #[must_use]
    pub const fn is_placed(self) -> bool {
        matches!(self, Self::Finished)
    }
}

/// Which clock a participant's time is measured from.
///
/// `wave` and `rolling` appear in `docs/timing-model.md` § 7 and are deliberately absent
/// here: waves are excluded from Milestone 4, and a variant that parses but scores like
/// `gun` would be a silent wrong answer. They arrive with the milestone that supports them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartMode {
    /// From the gun, for everyone alike. The conventional basis for placement.
    #[default]
    Gun,
    /// From each participant's own start-line detection.
    Chip,
}

impl StartMode {
    /// Parses an operator-supplied name.
    ///
    /// # Errors
    ///
    /// Returns the accepted values if the input is not one of them.
    pub fn parse(value: &str) -> Result<Self, &'static str> {
        match value.trim().to_ascii_lowercase().as_str() {
            "gun" => Ok(Self::Gun),
            "chip" => Ok(Self::Chip),
            _ => Err("expected one of: gun, chip"),
        }
    }

    /// The stable name used in storage and JSON.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Gun => "gun",
            Self::Chip => "chip",
        }
    }
}

/// Whether a revision is still expected to change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RevisionStatus {
    /// Published, but more corrections are expected.
    #[default]
    Provisional,
    /// Published as the finished article.
    ///
    /// Carries no enforcement: a later revision may still supersede it. Marking a revision
    /// final says what the organizer believed at the time, and a race that needs a
    /// correction after the fact needs it whether or not the software permits it.
    Final,
}

impl RevisionStatus {
    /// Parses an operator-supplied name.
    ///
    /// # Errors
    ///
    /// Returns the accepted values if the input is not one of them.
    pub fn parse(value: &str) -> Result<Self, &'static str> {
        match value.trim().to_ascii_lowercase().as_str() {
            "provisional" => Ok(Self::Provisional),
            "final" => Ok(Self::Final),
            _ => Err("expected one of: provisional, final"),
        }
    }

    /// The stable name used in storage and JSON.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Provisional => "provisional",
            Self::Final => "final",
        }
    }
}

/// An operator declaring how a participant's race ended.
///
/// **Evidence.** Append-only, minted once, never rewritten. A DQ is reversed by declaring a
/// new status, not by deleting this row: "bib 217 was disqualified at 11:04 and reinstated
/// at 11:31" is the true account, and it is one an organizer may have to defend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusDeclaration {
    /// Unique identifier, minted once.
    pub id: StatusDeclarationId,
    /// Storage order. The highest sequence for a participant is the one in force.
    pub seq: u64,
    /// The race this declaration applies to.
    pub race: RaceId,
    /// Who it is about.
    pub participant: ParticipantId,
    /// What was declared.
    pub status: ResultStatus,
    /// Why — required, because a status with no stated reason cannot be defended later.
    pub reason: String,
    /// Who declared it.
    pub actor: String,
    /// When it was declared.
    #[serde(with = "time::serde::rfc3339")]
    pub declared_at: OffsetDateTime,
}

/// The rules a revision was scored under, captured by value.
///
/// Snapshotted rather than referenced. "Revision 2 used a 5000 ms dedup window; revision 3
/// used 3000 ms" is a question that surfaces three weeks later, and a foreign key into a
/// settings table that has since been edited cannot answer it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScoringPolicy {
    /// Which clock placement is measured from.
    pub start_mode: StartMode,
    /// The deduplication and selection rules that produced the timing events.
    pub timing: TimingPolicy,
    /// The gun, as of this revision.
    #[serde(with = "time::serde::rfc3339::option")]
    pub gun_time: Option<OffsetDateTime>,
}

impl ScoringPolicy {
    /// A policy scoring from the gun with the default timing rules.
    #[must_use]
    pub fn gun(gun_time: Option<OffsetDateTime>) -> Self {
        Self {
            start_mode: StartMode::Gun,
            timing: TimingPolicy::default(),
            gun_time,
        }
    }
}

/// A condition that made a result less than straightforward.
///
/// Flags are attached to the entry rather than resolved silently. `docs/timing-model.md`
/// § 7 requires that a participant with no start read under a chip-time policy is "a
/// flagged condition with a defined fallback, not a null" — this is that flag, and the
/// fallback it names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "flag", rename_all = "snake_case")]
pub enum ResultFlag {
    /// Chip-time scoring, but the participant has no start-line detection.
    ///
    /// Scored from the gun instead. A missed start mat must not turn a finisher into a
    /// blank row, and it must not silently become a chip time that is really a gun time.
    NoStartReadUnderChipTime,
    /// No gun time is recorded and the race has no scheduled start, so no elapsed time
    /// could be computed at all.
    NoGunTime,
    /// The finish detection precedes the start the result is measured from.
    ///
    /// Physically impossible, so it is reported rather than published as a negative time.
    /// Usually a clock problem or a mis-mapped antenna.
    FinishBeforeStart,
    /// An operator declared this participant finished, but no finish crossing supports it.
    ///
    /// The declaration stands — an operator watching someone cross with a dead chip is a
    /// primary record — but the entry says the evidence is missing.
    DeclaredFinishedWithoutFinishRead,
}

impl ResultFlag {
    /// A stable machine-readable name, matching the serde tag.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::NoStartReadUnderChipTime => "no_start_read_under_chip_time",
            Self::NoGunTime => "no_gun_time",
            Self::FinishBeforeStart => "finish_before_start",
            Self::DeclaredFinishedWithoutFinishRead => "declared_finished_without_finish_read",
        }
    }
}

/// Where a participant's status came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusSource {
    /// Concluded from the crossings: finished, started but did not finish, or never seen.
    Derived,
    /// An operator declared it, overriding whatever the crossings implied.
    Declared,
}

/// One participant's line in a revision.
///
/// Identified by `(revision, participant)` — it carries no synthetic identifier of its own,
/// because there is nothing a UUID here would let anyone do that the pair does not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResultEntry {
    /// Who.
    pub participant: ParticipantId,
    /// Their bib, denormalized so a published revision stays readable after a roster edit.
    pub bib: Bib,
    /// Their name, denormalized for the same reason.
    pub name: String,
    /// How their race ended.
    pub status: ResultStatus,
    /// Whether that status was concluded or declared.
    pub status_source: StatusSource,
    /// The reason an operator gave, when the status was declared.
    pub status_reason: Option<String>,
    /// Their start-line detection, if they have one.
    #[serde(with = "time::serde::rfc3339::option")]
    pub start_at: Option<OffsetDateTime>,
    /// Their finish-line detection, if they have one.
    #[serde(with = "time::serde::rfc3339::option")]
    pub finish_at: Option<OffsetDateTime>,
    /// Finish minus the gun, in milliseconds.
    pub gun_time_ms: Option<i64>,
    /// Finish minus their own start detection, in milliseconds.
    pub chip_time_ms: Option<i64>,
    /// The time placement was computed from, per the revision's start mode.
    pub scoring_time_ms: Option<i64>,
    /// Overall place. `None` for anyone not [`ResultStatus::is_placed`].
    pub place: Option<u32>,
    /// Anything that made this result less than straightforward.
    pub flags: Vec<ResultFlag>,
    /// The timing events this entry rests on. The walk back to the evidence.
    pub timing_events: Vec<TimingEventId>,
}

/// An immutable, published set of results.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResultRevision {
    /// Unique identifier, minted once at publication.
    pub id: ResultRevisionId,
    /// The race scored.
    pub race: RaceId,
    /// Revision number, monotonically increasing within the race, starting at 1.
    pub number: u32,
    /// Whether more corrections are expected.
    pub status: RevisionStatus,
    /// When it was published.
    #[serde(with = "time::serde::rfc3339")]
    pub generated_at: OffsetDateTime,
    /// Who published it.
    pub actor: String,
    /// Why this revision exists — required, because "what changed and why" is the question
    /// a revision list has to answer.
    pub reason: String,
    /// The rules it was scored under, by value.
    pub policy: ScoringPolicy,
    /// The results, in placement order followed by the unplaced.
    pub entries: Vec<ResultEntry>,
}

impl ResultRevision {
    /// Looks up one participant's line.
    #[must_use]
    pub fn entry(&self, participant: ParticipantId) -> Option<&ResultEntry> {
        self.entries
            .iter()
            .find(|entry| entry.participant == participant)
    }

    /// Looks up a line by bib.
    #[must_use]
    pub fn entry_by_bib(&self, bib: &str) -> Option<&ResultEntry> {
        self.entries.iter().find(|entry| entry.bib.as_str() == bib)
    }

    /// How many entries hold each status.
    #[must_use]
    pub fn status_counts(&self) -> [(ResultStatus, usize); 4] {
        let count = |status: ResultStatus| {
            self.entries
                .iter()
                .filter(|entry| entry.status == status)
                .count()
        };
        [
            (ResultStatus::Finished, count(ResultStatus::Finished)),
            (ResultStatus::Dnf, count(ResultStatus::Dnf)),
            (ResultStatus::Dns, count(ResultStatus::Dns)),
            (ResultStatus::Dq, count(ResultStatus::Dq)),
        ]
    }

    /// A content fingerprint of the scored rows, ignoring publication metadata.
    ///
    /// Two revisions with the same digest published the same results. That is what makes
    /// "nothing actually changed" a detectable condition rather than a judgement call, and
    /// it is deliberately blind to `generated_at`, `actor`, and `reason` — republishing
    /// unchanged results a minute later is not a new result.
    #[must_use]
    pub fn digest(&self) -> String {
        let mut name = String::with_capacity(self.entries.len() * 48);
        for entry in &self.entries {
            name.push_str(entry.bib.as_str());
            name.push('\u{1f}');
            name.push_str(entry.status.as_str());
            name.push('\u{1f}');
            match entry.scoring_time_ms {
                Some(ms) => name.push_str(&ms.to_string()),
                None => name.push('-'),
            }
            name.push('\u{1f}');
            match entry.place {
                Some(place) => name.push_str(&place.to_string()),
                None => name.push('-'),
            }
            name.push('\u{1e}');
        }
        derived_uuid("result-digest", &[&name]).simple().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_round_trip_through_their_stored_names() {
        for status in [
            ResultStatus::Finished,
            ResultStatus::Dns,
            ResultStatus::Dnf,
            ResultStatus::Dq,
        ] {
            assert_eq!(ResultStatus::parse(status.as_str()), Ok(status));
        }
        assert_eq!(ResultStatus::parse(" DQ "), Ok(ResultStatus::Dq));
    }

    #[test]
    fn a_near_miss_status_is_refused_rather_than_guessed() {
        // "dsq" is a real abbreviation elsewhere in the sport. Accepting it here would mean
        // guessing, and the guess would disqualify somebody.
        assert!(ResultStatus::parse("dsq").is_err());
        assert!(ResultStatus::parse("").is_err());
        assert!(ResultStatus::parse("did not finish").is_err());
    }

    #[test]
    fn only_finishers_take_a_place() {
        assert!(ResultStatus::Finished.is_placed());
        for status in [ResultStatus::Dns, ResultStatus::Dnf, ResultStatus::Dq] {
            assert!(!status.is_placed(), "{status:?} must not be placed");
        }
    }

    #[test]
    fn start_modes_excluded_from_this_milestone_do_not_parse() {
        // Accepting `wave` and scoring it like `gun` would be a wrong answer that looks
        // like a right one.
        assert!(StartMode::parse("wave").is_err());
        assert!(StartMode::parse("rolling").is_err());
        assert_eq!(StartMode::parse("gun"), Ok(StartMode::Gun));
        assert_eq!(StartMode::parse("chip"), Ok(StartMode::Chip));
    }

    #[test]
    fn revision_statuses_round_trip() {
        for status in [RevisionStatus::Provisional, RevisionStatus::Final] {
            assert_eq!(RevisionStatus::parse(status.as_str()), Ok(status));
        }
        assert!(RevisionStatus::parse("draft").is_err());
    }

    #[test]
    fn the_scoring_policy_round_trips_through_json() {
        let policy = ScoringPolicy {
            start_mode: StartMode::Chip,
            timing: TimingPolicy::default(),
            gun_time: Some(OffsetDateTime::UNIX_EPOCH),
        };
        let json = serde_json::to_string(&policy).expect("serialize");
        let back: ScoringPolicy = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(policy, back);
    }

    fn entry(bib: &str, status: ResultStatus, ms: Option<i64>, place: Option<u32>) -> ResultEntry {
        ResultEntry {
            participant: ParticipantId::new(),
            bib: Bib::new(bib),
            name: format!("Runner {bib}"),
            status,
            status_source: StatusSource::Derived,
            status_reason: None,
            start_at: None,
            finish_at: None,
            gun_time_ms: ms,
            chip_time_ms: None,
            scoring_time_ms: ms,
            place,
            flags: Vec::new(),
            timing_events: Vec::new(),
        }
    }

    fn revision(entries: Vec<ResultEntry>) -> ResultRevision {
        ResultRevision {
            id: ResultRevisionId::new(),
            race: RaceId::new(),
            number: 1,
            status: RevisionStatus::Provisional,
            generated_at: OffsetDateTime::UNIX_EPOCH,
            actor: "operator".to_owned(),
            reason: "initial".to_owned(),
            policy: ScoringPolicy::gun(None),
            entries,
        }
    }

    #[test]
    fn the_digest_ignores_who_published_and_when() {
        let rows = || {
            vec![
                entry("101", ResultStatus::Finished, Some(1_000), Some(1)),
                entry("102", ResultStatus::Dnf, None, None),
            ]
        };
        let first = revision(rows());
        let mut second = revision(rows());
        second.number = 7;
        second.generated_at = OffsetDateTime::UNIX_EPOCH + time::Duration::days(3);
        second.actor = "someone-else".to_owned();
        second.reason = "republished".to_owned();
        second.status = RevisionStatus::Final;

        assert_eq!(
            first.digest(),
            second.digest(),
            "republishing identical results is not a new result"
        );
    }

    #[test]
    fn the_digest_moves_when_a_result_does() {
        let base = revision(vec![entry(
            "101",
            ResultStatus::Finished,
            Some(1_000),
            Some(1),
        )]);

        let dq = revision(vec![entry("101", ResultStatus::Dq, Some(1_000), None)]);
        assert_ne!(base.digest(), dq.digest(), "a DQ changes the results");

        let slower = revision(vec![entry(
            "101",
            ResultStatus::Finished,
            Some(1_001),
            Some(1),
        )]);
        assert_ne!(base.digest(), slower.digest(), "a changed time is a change");
    }

    #[test]
    fn status_counts_cover_every_status() {
        let published = revision(vec![
            entry("101", ResultStatus::Finished, Some(1), Some(1)),
            entry("102", ResultStatus::Finished, Some(2), Some(2)),
            entry("103", ResultStatus::Dnf, None, None),
            entry("104", ResultStatus::Dq, Some(3), None),
        ]);
        assert_eq!(
            published.status_counts(),
            [
                (ResultStatus::Finished, 2),
                (ResultStatus::Dnf, 1),
                (ResultStatus::Dns, 0),
                (ResultStatus::Dq, 1),
            ]
        );
    }

    #[test]
    fn entries_are_findable_by_bib() {
        let published = revision(vec![entry("101", ResultStatus::Finished, Some(1), Some(1))]);
        assert!(published.entry_by_bib("101").is_some());
        assert!(published.entry_by_bib("999").is_none());
    }
}
