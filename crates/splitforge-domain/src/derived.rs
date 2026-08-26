//! Derived records: what we concluded from the evidence.
//!
//! Everything here is regenerable. Nothing here is evidence. Each record carries
//! references back to the raw reads that produced it, so a finish time can always be
//! walked back to the bytes a reader sent.
//!
//! # Deterministic identifiers
//!
//! Derived ids are UUIDv5 values computed from the evidence, not random v4 values. That
//! makes re-derivation **bit-identical** rather than merely equivalent: re-deriving after
//! a crash, or on another machine, produces the same ids, so outputs can be compared
//! directly. Random ids would make every re-derivation look like a change.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::ids::{
    AcceptedReadId, CheckpointId, ChipId, ManualEntryId, ParticipantId, RawReadId, ReaderId,
    SPLITFORGE_NAMESPACE, TimingEventId,
};

/// Builds a deterministic identifier from a record kind and its distinguishing parts.
///
/// Parts are joined with an ASCII unit separator, which cannot occur in a UUID, an EPC, or
/// a decimal number — so distinct inputs cannot collide by concatenation.
pub(crate) fn derived_uuid(kind: &str, parts: &[&str]) -> Uuid {
    let mut name = String::with_capacity(64);
    name.push_str(kind);
    for part in parts {
        name.push('\u{1f}');
        name.push_str(part);
    }
    Uuid::new_v5(&SPLITFORGE_NAMESPACE, name.as_bytes())
}

/// One crossing: the single read from a burst that was credited.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptedRead {
    /// Deterministic identifier.
    pub id: AcceptedReadId,
    /// Where the crossing happened.
    pub checkpoint: CheckpointId,
    /// The chip that crossed.
    pub chip: ChipId,
    /// Who was wearing it, if the chip resolved to anybody.
    ///
    /// `None` is a *flagged* condition, not a discarded read. An unmapped chip is usually
    /// a roster error, and the crossing still happened.
    pub participant: Option<ParticipantId>,
    /// The authoritative timestamp of the credited read.
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    /// The raw read that was credited.
    pub source_raw_read: RawReadId,
    /// Every raw read in the burst, credited or not. The evidence for this crossing.
    pub burst_raw_reads: Vec<RawReadId>,
    /// Signal strength of the credited read, when known.
    pub rssi_dbm: Option<i16>,
}

impl AcceptedRead {
    /// Builds a crossing with a deterministic identifier.
    ///
    /// Identity is `(checkpoint, chip, credited raw read)` — one crossing per credited
    /// read, stable across re-derivation.
    #[must_use]
    pub fn new(
        checkpoint: CheckpointId,
        chip: ChipId,
        participant: Option<ParticipantId>,
        at: OffsetDateTime,
        source_raw_read: RawReadId,
        burst_raw_reads: Vec<RawReadId>,
        rssi_dbm: Option<i16>,
    ) -> Self {
        let id = AcceptedReadId::from_uuid(derived_uuid(
            "accepted-read",
            &[
                &checkpoint.to_string(),
                chip.as_str(),
                &source_raw_read.to_string(),
            ],
        ));
        Self {
            id,
            checkpoint,
            chip,
            participant,
            at,
            source_raw_read,
            burst_raw_reads,
            rssi_dbm,
        }
    }

    /// How many raw reads made up this crossing.
    #[must_use]
    pub fn burst_size(&self) -> usize {
        self.burst_raw_reads.len()
    }
}

/// Why a raw read did not become a crossing.
///
/// Every suppressed read keeps one of these. "Missing from the results" and "suppressed
/// by rule X at time T" are very different answers to give a runner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum RejectionReason {
    /// Part of a burst already credited to another read.
    DuplicateWithinWindow {
        /// The crossing this read belongs to.
        accepted: AcceptedReadId,
        /// Milliseconds between this read and the credited one.
        interval_ms: i64,
    },
    /// Signal strength below the configured floor.
    BelowRssiFloor {
        /// The read's signal strength, if it reported one.
        rssi_dbm: Option<i16>,
        /// The configured floor.
        floor_dbm: i16,
    },
    /// A lap credited faster than physically plausible; treated as a re-read.
    BelowMinLap {
        /// The configured minimum lap time.
        min_lap_ms: u64,
        /// The lap time this read would have produced.
        actual_ms: i64,
    },
    /// The reader or antenna is not mapped to any checkpoint.
    ///
    /// The read stays in the journal. Only its interpretation is withheld, and it becomes
    /// interpretable the moment the mapping is corrected.
    UnmappedReader {
        /// The reader that produced the read.
        reader: ReaderId,
        /// The antenna, if reported.
        antenna: Option<u16>,
    },
}

impl RejectionReason {
    /// A stable machine-readable name for the rule that suppressed the read.
    ///
    /// Matches the serde tag, so a count keyed by this label lines up with the JSON an
    /// operator is looking at.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::DuplicateWithinWindow { .. } => "duplicate_within_window",
            Self::BelowRssiFloor { .. } => "below_rssi_floor",
            Self::BelowMinLap { .. } => "below_min_lap",
            Self::UnmappedReader { .. } => "unmapped_reader",
        }
    }
}

/// A raw read that was not credited, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RejectedRead {
    /// The read that was not credited.
    pub raw_read: RawReadId,
    /// Which rule suppressed it.
    pub reason: RejectionReason,
}

/// What produced a timing event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "origin", rename_all = "snake_case")]
pub enum TimingEventOrigin {
    /// Derived from a crossing — a chip read by a mat.
    AcceptedRead {
        /// The crossing.
        accepted_read: AcceptedReadId,
    },
    /// Entered by an operator, because no chip recorded it.
    ///
    /// Carries the entry rather than copying its contents, so the reason and the person who
    /// typed it are one lookup away from any result that depends on them
    /// ([timing model § 6](../../../docs/timing-model.md#6-timing-events-and-manual-entries)).
    Manual {
        /// The entry.
        manual_entry: ManualEntryId,
    },
}

/// A participant was at a checkpoint at a time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimingEvent {
    /// Deterministic identifier.
    pub id: TimingEventId,
    /// Who.
    pub participant: ParticipantId,
    /// Where.
    pub checkpoint: CheckpointId,
    /// When.
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    /// Which lap this crossing represents, starting at 1.
    pub lap: u16,
    /// What produced it.
    pub origin: TimingEventOrigin,
}

impl TimingEvent {
    /// Builds a timing event from a crossing, with a deterministic identifier.
    #[must_use]
    pub fn from_accepted(
        participant: ParticipantId,
        checkpoint: CheckpointId,
        at: OffsetDateTime,
        lap: u16,
        accepted_read: AcceptedReadId,
    ) -> Self {
        let id = TimingEventId::from_uuid(derived_uuid(
            "timing-event",
            &[
                &participant.to_string(),
                &checkpoint.to_string(),
                &lap.to_string(),
                &accepted_read.to_string(),
            ],
        ));
        Self {
            id,
            participant,
            checkpoint,
            at,
            lap,
            origin: TimingEventOrigin::AcceptedRead { accepted_read },
        }
    }

    /// Builds a timing event from an operator's entry, with a deterministic identifier.
    ///
    /// Deterministic for the same reason the crossing case is: re-deriving must reproduce
    /// byte-identical output. The entry's own identifier is the random part, minted once
    /// when the operator typed it; everything downstream of it is a pure function.
    #[must_use]
    pub fn from_manual(
        participant: ParticipantId,
        checkpoint: CheckpointId,
        at: OffsetDateTime,
        lap: u16,
        manual_entry: ManualEntryId,
    ) -> Self {
        let id = TimingEventId::from_uuid(derived_uuid(
            "timing-event",
            &[
                &participant.to_string(),
                &checkpoint.to_string(),
                &lap.to_string(),
                &manual_entry.to_string(),
            ],
        ));
        Self {
            id,
            participant,
            checkpoint,
            at,
            lap,
            origin: TimingEventOrigin::Manual { manual_entry },
        }
    }
}

/// The complete output of one derivation pass.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Derivation {
    /// Crossings, in chronological order.
    pub accepted: Vec<AcceptedRead>,
    /// Suppressed reads, with the rule that suppressed each.
    pub rejected: Vec<RejectedRead>,
    /// Timing events, in chronological order.
    pub timing_events: Vec<TimingEvent>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::Duration;

    fn at(secs: i64) -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH + Duration::seconds(secs)
    }

    #[test]
    fn accepted_read_ids_are_deterministic() {
        let checkpoint = CheckpointId::new();
        let chip = ChipId::new("E280");
        let raw = RawReadId::new();

        let first = AcceptedRead::new(
            checkpoint,
            chip.clone(),
            None,
            at(10),
            raw,
            vec![raw],
            Some(-50),
        );
        // Same evidence, different burst membership and participant: identity is the
        // credited read, so the id must not move.
        let second = AcceptedRead::new(
            checkpoint,
            chip,
            Some(ParticipantId::new()),
            at(10),
            raw,
            vec![raw, RawReadId::new()],
            None,
        );

        assert_eq!(first.id, second.id, "re-derivation must reproduce the id");
    }

    #[test]
    fn different_evidence_produces_different_ids() {
        let checkpoint = CheckpointId::new();
        let chip = ChipId::new("E280");
        let one = AcceptedRead::new(
            checkpoint,
            chip.clone(),
            None,
            at(10),
            RawReadId::new(),
            vec![],
            None,
        );
        let two = AcceptedRead::new(
            checkpoint,
            chip,
            None,
            at(10),
            RawReadId::new(),
            vec![],
            None,
        );
        assert_ne!(one.id, two.id);
    }

    #[test]
    fn timing_event_ids_are_deterministic_and_lap_aware() {
        let participant = ParticipantId::new();
        let checkpoint = CheckpointId::new();
        let accepted = AcceptedReadId::new();

        let lap_one = TimingEvent::from_accepted(participant, checkpoint, at(10), 1, accepted);
        let lap_one_again =
            TimingEvent::from_accepted(participant, checkpoint, at(10), 1, accepted);
        let lap_two = TimingEvent::from_accepted(participant, checkpoint, at(99), 2, accepted);

        assert_eq!(lap_one.id, lap_one_again.id);
        assert_ne!(lap_one.id, lap_two.id);
    }

    #[test]
    fn rejection_labels_match_the_serde_tag() {
        let reason = RejectionReason::BelowMinLap {
            min_lap_ms: 1,
            actual_ms: 0,
        };
        let json = serde_json::to_value(&reason).expect("serialize");
        assert_eq!(json["reason"], reason.label());
    }

    #[test]
    fn separator_prevents_concatenation_collisions() {
        // Without a separator, ("ab", "c") and ("a", "bc") would hash identically.
        assert_ne!(
            derived_uuid("k", &["ab", "c"]),
            derived_uuid("k", &["a", "bc"])
        );
    }
}
