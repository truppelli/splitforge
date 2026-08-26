//! Manual entries: what an operator writes down when the chip does not.
//!
//! [Timing model § 6](../../../docs/timing-model.md#6-timing-events-and-manual-entries) puts
//! it plainly:
//!
//! > Manual entries are **evidence too** — append-only, recording who entered it, when, why,
//! > and what they claimed. An operator writing down a bib at the finish because a chip
//! > failed is producing a primary record, and it deserves the same immutability as a reader
//! > report.
//!
//! That is the whole design. A [`ManualEntry`] is not a correction to a read and not a
//! result override; it is a second **kind** of evidence, sitting beside `raw_reads` and
//! feeding the same derivation. A [`TimingEvent`](crate::TimingEvent) has exactly one
//! origin, and this is the other one.
//!
//! ## Why it is not a result edit
//!
//! The tempting shortcut is to let an operator type a finish time straight into the results
//! table. That would destroy the property
//! [ADR-0014](../../../docs/adr/0014-mutable-configuration-immutable-evidence.md) exists to
//! protect: results are *derived*, and re-deriving must reproduce them. An entry recorded
//! here survives re-derivation, survives a restore from a snapshot that predates it, and
//! shows up in every future revision — because it is evidence, and evidence is the input.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::ids::{CheckpointId, ManualEntryId, ParticipantId, RaceId};

/// A claim by an operator that a participant was at a checkpoint at a time.
///
/// Immutable once recorded. A mistaken entry is corrected by recording the correction, not
/// by editing this — the same rule the raw read journal follows
/// ([ADR-0005](../../../docs/adr/0005-raw-read-append-only-journal.md)).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManualEntry {
    /// Identifies this entry.
    pub id: ManualEntryId,
    /// Which race it belongs to.
    pub race: RaceId,
    /// Who the operator says was there.
    pub participant: ParticipantId,
    /// Where.
    pub checkpoint: CheckpointId,
    /// **When the operator claims it happened** — never when they typed it.
    ///
    /// These are different times and the gap between them can be an hour. `recorded_at` on
    /// [`StoredManualEntry`] holds the second one, so a dispute can see both.
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    /// Who entered it. Attribution is what makes a manual entry auditable rather than
    /// anonymous, since nothing about the claim itself can be checked against a machine.
    pub actor: String,
    /// Why it was entered — "chip failed at the start", "runner finished off the mat".
    ///
    /// Required rather than optional. An entry with no reason is indistinguishable from a
    /// typo six months later, when somebody is asking why a runner has a time nothing
    /// recorded.
    pub reason: String,
}

/// A manual entry as it exists in the journal, with the fields storage assigns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredManualEntry {
    /// Insertion sequence, independent of any clock.
    pub seq: u64,
    /// When the row was written — the data-entry time, not the claimed time.
    #[serde(with = "time::serde::rfc3339")]
    pub recorded_at: OffsetDateTime,
    /// The claim itself.
    #[serde(flatten)]
    pub entry: ManualEntry,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{CheckpointId, ParticipantId, RaceId};

    fn entry() -> ManualEntry {
        ManualEntry {
            id: ManualEntryId::new(),
            race: RaceId::new(),
            participant: ParticipantId::new(),
            checkpoint: CheckpointId::new(),
            at: OffsetDateTime::from_unix_timestamp(1_775_000_000).expect("a valid timestamp"),
            actor: "phillip".to_owned(),
            reason: "chip failed at the start".to_owned(),
        }
    }

    #[test]
    fn two_entries_for_the_same_claim_are_distinct_records() {
        // Deliberately not a derived identifier. Two officials both writing down bib 104 at
        // 08:17:32 have produced two records of one event, and collapsing them would throw
        // away the fact that two people saw it.
        let first = entry();
        let mut second = first.clone();
        second.id = ManualEntryId::new();

        assert_ne!(first.id, second.id);
        assert_eq!(first.participant, second.participant);
        assert_eq!(first.at, second.at);
    }

    #[test]
    fn an_entry_round_trips_through_json() {
        let entry = entry();
        let json = serde_json::to_string(&entry).expect("serialize");
        let back: ManualEntry = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(entry, back);
    }

    #[test]
    fn the_stored_form_keeps_the_claimed_time_and_the_entry_time_apart() {
        // The two differ by however long it took to walk to the laptop, and a dispute needs
        // both: one is when the runner finished, the other is when somebody said so.
        let entry = entry();
        let claimed = entry.at;
        let stored = StoredManualEntry {
            seq: 1,
            recorded_at: claimed + time::Duration::minutes(43),
            entry,
        };

        assert_eq!(stored.entry.at, claimed);
        assert_eq!(
            stored.recorded_at - stored.entry.at,
            time::Duration::minutes(43)
        );

        let json = serde_json::to_value(&stored).expect("serialize");
        assert!(json.get("at").is_some(), "flattened: {json}");
        assert!(json.get("recorded_at").is_some(), "got: {json}");
    }
}
