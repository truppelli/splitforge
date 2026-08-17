//! Event configuration: races, checkpoints, participants, and chip assignments.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::error::DomainError;
use crate::ids::{Bib, CheckpointId, ChipId, EventId, ParticipantId, RaceId, ReaderId};
use crate::policy::{TimestampTrust, TimingPolicy};
use crate::result::StartMode;

/// A race day, which may contain several races.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    /// Unique identifier.
    pub id: EventId,
    /// Human-readable name.
    pub name: String,
}

/// A single race within an event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Race {
    /// Unique identifier.
    pub id: RaceId,
    /// The event this race belongs to.
    pub event: EventId,
    /// Human-readable name.
    pub name: String,
    /// Scheduled start. The gun time under a `gun` start policy.
    #[serde(with = "time::serde::rfc3339::option")]
    pub scheduled_start: Option<OffsetDateTime>,
    /// Laps a participant is expected to complete. `1` for a point-to-point race.
    pub expected_laps: u16,
}

/// What role a checkpoint plays on the course.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointKind {
    /// Start line.
    Start,
    /// An intermediate split.
    Split,
    /// Finish line.
    Finish,
    /// A lap counter, crossed repeatedly.
    Lap,
}

/// A point on the course where crossings are detected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    /// Unique identifier.
    pub id: CheckpointId,
    /// The race this checkpoint belongs to.
    pub race: RaceId,
    /// Human-readable name.
    pub name: String,
    /// Role on the course.
    pub kind: CheckpointKind,
    /// Position in the expected checkpoint sequence, starting at zero.
    pub sequence: u16,
}

/// Someone taking part.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Participant {
    /// Unique identifier.
    pub id: ParticipantId,
    /// The race entered.
    pub race: RaceId,
    /// Bib number.
    pub bib: Bib,
    /// Display name.
    pub name: String,
}

/// A chip worn by a participant for a bounded window of time.
///
/// Chips are reused across races in a day, reassigned after a DNS, and swapped when one
/// fails at the start line. Resolution is therefore a function of *when* the read
/// happened, never a lookup of current state — getting this wrong is one of the most
/// common causes of incorrect chip timing results.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChipAssignment {
    /// The chip.
    pub chip: ChipId,
    /// Who was wearing it.
    pub participant: ParticipantId,
    /// The race it was assigned for.
    pub race: RaceId,
    /// Inclusive start of validity.
    #[serde(with = "time::serde::rfc3339")]
    pub valid_from: OffsetDateTime,
    /// Exclusive end of validity. `None` means open-ended.
    #[serde(with = "time::serde::rfc3339::option")]
    pub valid_until: Option<OffsetDateTime>,
}

impl ChipAssignment {
    /// Whether this assignment covers the given instant.
    #[must_use]
    pub fn covers(&self, at: OffsetDateTime) -> bool {
        at >= self.valid_from && self.valid_until.is_none_or(|until| at < until)
    }

    /// Half-open intervals `[from, until)` overlap when each starts before the other ends.
    /// An open-ended assignment (`valid_until: None`) never ends.
    fn overlaps(&self, other: &Self) -> bool {
        let starts_before_other_ends = other.valid_until.is_none_or(|end| self.valid_from < end);
        let other_starts_before_self_ends =
            self.valid_until.is_none_or(|end| other.valid_from < end);
        starts_before_other_ends && other_starts_before_self_ends
    }
}

/// A configured reader.
///
/// Identity is the operator-assigned [`ReaderId`], not a network address: a reader that
/// picks up a new DHCP lease is still the same reader, and evidence already in the journal
/// must not become ambiguous because the network changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reader {
    /// Operator-assigned identifier, recorded on every read this reader produces.
    pub id: ReaderId,
    /// Human-readable description, for the operator rather than the machine.
    pub label: String,
    /// Where to reach it, once there is something to reach.
    ///
    /// Unused until Milestone 3 — no reader protocol exists yet, and a stored address that
    /// nothing connects to would be a claim this project has not earned.
    pub endpoint: Option<String>,
    /// How far this reader's own clock is trusted.
    pub timestamp_trust: TimestampTrust,
}

/// What an operator did to a race clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionAction {
    /// The gun went off.
    Start,
    /// The race was closed.
    Stop,
}

impl SessionAction {
    /// Parses an operator-supplied name.
    ///
    /// # Errors
    ///
    /// Returns the accepted values if the input is not one of them.
    pub fn parse(value: &str) -> Result<Self, &'static str> {
        match value.trim().to_ascii_lowercase().as_str() {
            "start" => Ok(Self::Start),
            "stop" => Ok(Self::Stop),
            _ => Err("expected one of: start, stop"),
        }
    }

    /// The stable name used in storage and JSON output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Stop => "stop",
        }
    }
}

/// A recorded start or stop, and who recorded it.
///
/// Append-only, because an operator typed it — the same status
/// `docs/timing-model.md` § 6 gives a manual entry. A recorded start is an *input to
/// scoring*, never a filter on evidence: reads are journalled whether or not anybody
/// remembered to type `race start`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RaceSession {
    /// Monotonic sequence within the database.
    pub seq: u64,
    /// The race.
    pub race: RaceId,
    /// What happened.
    pub action: SessionAction,
    /// The instant being recorded — the gun time for a start.
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    /// Who recorded it.
    pub actor: String,
    /// Why, when the operator said.
    pub note: Option<String>,
    /// When the row was written. Differs from `at` when a start is backdated.
    #[serde(with = "time::serde::rfc3339")]
    pub recorded_at: OffsetDateTime,
}

/// Time-aware chip to participant resolution.
#[derive(Debug, Clone, Default)]
pub struct ChipRegistry {
    by_chip: BTreeMap<ChipId, Vec<ChipAssignment>>,
}

impl ChipRegistry {
    /// Builds a registry, rejecting overlapping assignments for the same chip.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::OverlappingChipAssignment`] if one chip is assigned to two
    /// participants at the same instant. That is ambiguous evidence, and guessing which
    /// one was meant is exactly the kind of silent decision this project refuses to make.
    pub fn from_assignments(
        assignments: impl IntoIterator<Item = ChipAssignment>,
    ) -> Result<Self, DomainError> {
        let mut by_chip: BTreeMap<ChipId, Vec<ChipAssignment>> = BTreeMap::new();
        for assignment in assignments {
            by_chip
                .entry(assignment.chip.clone())
                .or_default()
                .push(assignment);
        }
        for (chip, list) in &mut by_chip {
            list.sort_by_key(|a| a.valid_from);
            for pair in list.windows(2) {
                if pair[0].overlaps(&pair[1]) {
                    return Err(DomainError::OverlappingChipAssignment { chip: chip.clone() });
                }
            }
        }
        Ok(Self { by_chip })
    }

    /// Resolves the assignment in force for a chip at an instant, if any.
    #[must_use]
    pub fn resolve(&self, chip: &ChipId, at: OffsetDateTime) -> Option<&ChipAssignment> {
        self.by_chip
            .get(chip)?
            .iter()
            .find(|assignment| assignment.covers(at))
    }

    /// Resolves just the participant.
    #[must_use]
    pub fn participant_at(&self, chip: &ChipId, at: OffsetDateTime) -> Option<ParticipantId> {
        self.resolve(chip, at).map(|a| a.participant)
    }

    /// Number of distinct chips known to the registry.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_chip.len()
    }

    /// Whether the registry is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_chip.is_empty()
    }
}

/// Maps a reader and antenna to the checkpoint it covers.
///
/// An entry with antenna `None` acts as a wildcard for every antenna on that reader, so a
/// single-antenna deployment needs one line of configuration rather than one per port.
#[derive(Debug, Clone, Default)]
pub struct AntennaMap {
    exact: BTreeMap<(ReaderId, u16), CheckpointId>,
    wildcard: BTreeMap<ReaderId, CheckpointId>,
}

impl AntennaMap {
    /// Creates an empty map.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Maps one specific antenna on a reader to a checkpoint.
    pub fn map_antenna(&mut self, reader: ReaderId, antenna: u16, checkpoint: CheckpointId) {
        self.exact.insert((reader, antenna), checkpoint);
    }

    /// Maps every antenna on a reader to a checkpoint.
    pub fn map_reader(&mut self, reader: ReaderId, checkpoint: CheckpointId) {
        self.wildcard.insert(reader, checkpoint);
    }

    /// Resolves the checkpoint covered by a reader and antenna.
    #[must_use]
    pub fn resolve(&self, reader: &ReaderId, antenna: Option<u16>) -> Option<CheckpointId> {
        // A specific antenna entry beats the reader-wide wildcard: an operator who has
        // mapped antenna 4 to the finish means it, even on a reader mapped to the start.
        antenna
            .and_then(|antenna| self.exact.get(&(reader.clone(), antenna)).copied())
            .or_else(|| self.wildcard.get(reader).copied())
    }

    /// Whether the map has no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.exact.is_empty() && self.wildcard.is_empty()
    }

    /// Every mapping, as `(reader, antenna, checkpoint)`.
    ///
    /// `antenna` is `None` for a reader-wide wildcard. Wildcards come first so the output
    /// reads the way the map resolves: the general rule, then the exceptions to it.
    pub fn entries(&self) -> impl Iterator<Item = (&ReaderId, Option<u16>, CheckpointId)> {
        let wildcards = self
            .wildcard
            .iter()
            .map(|(reader, checkpoint)| (reader, None, *checkpoint));
        let exact = self
            .exact
            .iter()
            .map(|((reader, antenna), checkpoint)| (reader, Some(*antenna), *checkpoint));
        wildcards.chain(exact)
    }

    /// How many mappings the map holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.exact.len() + self.wildcard.len()
    }
}

/// Everything needed to interpret one race's reads.
///
/// The complete configuration of a race, assembled in one place: who is entered, what they
/// are wearing, which antenna covers which checkpoint, and the rules for turning reads into
/// crossings. Derivation is a pure function of this plus the journal.
///
/// Both sides of the system build one. `splitforge-storage` loads it from the database;
/// `splitforge-testkit` constructs one in code for fixtures. Having a single type means the
/// fixtures exercise the same shape the real loader produces, rather than a parallel one
/// that can drift.
#[derive(Debug, Clone)]
pub struct RaceConfig {
    /// The race day.
    pub event: Event,
    /// The race.
    pub race: Race,
    /// Checkpoints, in sequence order.
    pub checkpoints: Vec<Checkpoint>,
    /// The roster.
    pub participants: Vec<Participant>,
    /// Time-bounded chip assignments.
    pub assignments: Vec<ChipAssignment>,
    /// Configured readers.
    pub readers: Vec<Reader>,
    /// Reader and antenna to checkpoint mapping, built from `readers`.
    pub antennas: AntennaMap,
    /// The rules in force.
    pub policy: TimingPolicy,
    /// Which clock placement is measured from.
    ///
    /// Separate from [`RaceConfig::policy`] because that is what the engine uses to turn
    /// reads into crossings, and the engine has no business knowing that start modes exist.
    pub start_mode: StartMode,
    /// Recorded starts and stops, oldest first.
    pub sessions: Vec<RaceSession>,
}

impl RaceConfig {
    /// Builds the chip registry.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::OverlappingChipAssignment`] if one chip is assigned to two
    /// participants at the same instant.
    pub fn chips(&self) -> Result<ChipRegistry, DomainError> {
        ChipRegistry::from_assignments(self.assignments.iter().cloned())
    }

    /// Looks up a checkpoint by name.
    #[must_use]
    pub fn checkpoint(&self, name: &str) -> Option<&Checkpoint> {
        self.checkpoints
            .iter()
            .find(|checkpoint| checkpoint.name == name)
    }

    /// Looks up a checkpoint by identifier.
    #[must_use]
    pub fn checkpoint_by_id(&self, id: CheckpointId) -> Option<&Checkpoint> {
        self.checkpoints
            .iter()
            .find(|checkpoint| checkpoint.id == id)
    }

    /// Looks up a participant by bib.
    #[must_use]
    pub fn participant(&self, bib: &str) -> Option<&Participant> {
        self.participants
            .iter()
            .find(|participant| participant.bib.as_str() == bib)
    }

    /// Looks up a participant by identifier.
    #[must_use]
    pub fn participant_by_id(&self, id: ParticipantId) -> Option<&Participant> {
        self.participants
            .iter()
            .find(|participant| participant.id == id)
    }

    /// The gun time in force: the most recently recorded start.
    ///
    /// The *last* start rather than the first, because a restarted race is a real thing —
    /// a false start, a delayed wave, a mat that was not live when the gun went. Earlier
    /// starts are not erased; they stay in [`RaceConfig::sessions`] and in the audit trail,
    /// which is what lets "why did the times move?" be answered.
    ///
    /// Falls back to [`Race::scheduled_start`] when nothing has been recorded, so a race
    /// configured but never formally started is still scorable.
    #[must_use]
    pub fn gun_time(&self) -> Option<OffsetDateTime> {
        self.sessions
            .iter()
            .rfind(|session| session.action == SessionAction::Start)
            .map(|session| session.at)
            .or(self.race.scheduled_start)
    }

    /// Whether the race has been started and not since stopped.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.sessions
            .last()
            .is_some_and(|session| session.action == SessionAction::Start)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::Duration;

    fn t(secs: i64) -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH + Duration::seconds(secs)
    }

    fn assignment(
        chip: &str,
        participant: ParticipantId,
        from: i64,
        until: Option<i64>,
    ) -> ChipAssignment {
        ChipAssignment {
            chip: ChipId::new(chip),
            participant,
            race: RaceId::new(),
            valid_from: t(from),
            valid_until: until.map(t),
        }
    }

    #[test]
    fn a_reused_chip_resolves_to_whoever_wore_it_at_the_time() {
        let morning = ParticipantId::new();
        let afternoon = ParticipantId::new();
        let registry = ChipRegistry::from_assignments([
            assignment("A", morning, 0, Some(100)),
            assignment("A", afternoon, 100, None),
        ])
        .expect("no overlap");

        let chip = ChipId::new("A");
        assert_eq!(registry.participant_at(&chip, t(50)), Some(morning));
        // Boundary is exclusive at the end, inclusive at the start.
        assert_eq!(registry.participant_at(&chip, t(100)), Some(afternoon));
        assert_eq!(registry.participant_at(&chip, t(5_000)), Some(afternoon));
    }

    #[test]
    fn a_read_before_any_assignment_resolves_to_nobody() {
        let registry =
            ChipRegistry::from_assignments([assignment("A", ParticipantId::new(), 100, None)])
                .expect("valid");
        assert_eq!(registry.participant_at(&ChipId::new("A"), t(50)), None);
    }

    #[test]
    fn overlapping_assignments_are_rejected_rather_than_guessed() {
        let err = ChipRegistry::from_assignments([
            assignment("A", ParticipantId::new(), 0, Some(200)),
            assignment("A", ParticipantId::new(), 100, None),
        ])
        .expect_err("overlap must be rejected");
        assert!(matches!(err, DomainError::OverlappingChipAssignment { .. }));
    }

    #[test]
    fn antenna_mapping_prefers_the_specific_entry_over_the_wildcard() {
        let reader = ReaderId::new("mat");
        let start = CheckpointId::new();
        let finish = CheckpointId::new();
        let mut map = AntennaMap::new();
        map.map_reader(reader.clone(), start);
        map.map_antenna(reader.clone(), 4, finish);

        assert_eq!(map.resolve(&reader, Some(4)), Some(finish));
        assert_eq!(map.resolve(&reader, Some(1)), Some(start));
        assert_eq!(map.resolve(&reader, None), Some(start));
        assert_eq!(map.resolve(&ReaderId::new("other"), Some(4)), None);
    }
}
