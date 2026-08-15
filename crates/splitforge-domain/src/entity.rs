//! Event configuration: races, checkpoints, participants, and chip assignments.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::error::DomainError;
use crate::ids::{Bib, CheckpointId, ChipId, EventId, ParticipantId, RaceId, ReaderId};

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
