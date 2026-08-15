//! The JSON contract.
//!
//! Explicit view types rather than serializing domain structs directly. Two reasons: the
//! raw payload of a read is bytes and belongs in the database rather than on a terminal,
//! and an operator wants a bib number where the domain holds a participant UUID.
//!
//! These shapes are **not** the versioned export contract — that is Milestone 6. They are
//! diagnostic output, and they are allowed to change.

use std::collections::BTreeMap;

use serde::Serialize;
use splitforge_domain::{
    AcceptedRead, AcceptedReadId, ChipId, Derivation, DeviceClockState, ParticipantId, RawReadId,
    ReaderId, StoredRawRead, TimestampSource, TimingEventOrigin,
};
use splitforge_testkit::FixtureEvent;
use time::OffsetDateTime;

/// Result of creating or migrating a database.
#[derive(Debug, Serialize)]
pub struct InitReport {
    /// Where the database lives.
    pub database: String,
    /// Schema version after migration.
    pub schema_version: i64,
    /// Reads already present. Non-zero means the database was not new.
    pub reads: u64,
}

/// A raw read, as an operator wants to see it.
#[derive(Debug, Serialize)]
pub struct RawReadView {
    /// Journal sequence. A gap here is the question worth asking.
    pub seq: u64,
    /// The read's identifier.
    pub id: RawReadId,
    /// Which reader produced it.
    pub source: ReaderId,
    /// Antenna, when reported.
    pub antenna: Option<u16>,
    /// The chip.
    pub chip: ChipId,
    /// The timestamp that counts for timing.
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    /// Which clock produced `at`, and why.
    pub timestamp_source: TimestampSource,
    /// What the reader claimed, when it claimed anything.
    #[serde(with = "time::serde::rfc3339::option")]
    pub reader_timestamp: Option<OffsetDateTime>,
    /// When this process received the report.
    #[serde(with = "time::serde::rfc3339")]
    pub received_at: OffsetDateTime,
    /// Measured reader-to-device offset in milliseconds, when both clocks were known.
    pub clock_offset_ms: Option<i64>,
    /// Device clock state at receipt.
    pub device_clock_state: DeviceClockState,
    /// Signal strength, when reported.
    pub rssi_dbm: Option<i16>,
    /// Size of the retained payload. The bytes themselves stay in the database.
    pub payload_bytes: usize,
    /// Hash of the retained payload.
    pub payload_sha256: String,
    /// When the row was written. The gap from `received_at` is write latency.
    #[serde(with = "time::serde::rfc3339")]
    pub recorded_at: OffsetDateTime,
}

impl RawReadView {
    /// Builds a view of a stored read.
    #[must_use]
    pub fn of(stored: &StoredRawRead) -> Self {
        Self {
            seq: stored.seq,
            id: stored.read.id,
            source: stored.read.source.clone(),
            antenna: stored.read.antenna,
            chip: stored.read.chip.clone(),
            at: stored.read.authoritative_timestamp(),
            timestamp_source: stored.read.timestamp_source,
            reader_timestamp: stored.read.reader_timestamp,
            received_at: stored.read.received_at,
            clock_offset_ms: stored.read.clock_offset_ms,
            device_clock_state: stored.read.device_clock_state,
            rssi_dbm: stored.read.rssi_dbm,
            payload_bytes: stored.read.raw_payload.len(),
            payload_sha256: stored.payload_sha256.clone(),
            recorded_at: stored.recorded_at,
        }
    }
}

/// What the journal holds, without deriving anything from it.
#[derive(Debug, Serialize)]
pub struct StatusReport {
    /// Where the database lives.
    pub database: String,
    /// Schema version.
    pub schema_version: i64,
    /// Reads in the journal.
    pub raw_reads: usize,
    /// Earliest authoritative timestamp.
    #[serde(with = "time::serde::rfc3339::option")]
    pub first_read_at: Option<OffsetDateTime>,
    /// Latest authoritative timestamp.
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_read_at: Option<OffsetDateTime>,
    /// Reads taken while the device had no trustworthy time source.
    ///
    /// Not an error, and not a reason to discard anything — but results derived from these
    /// cannot be published without a caveat. See `docs/clock-and-time-discipline.md`.
    pub untrusted_clock_reads: usize,
    /// Reads timed by this device rather than by the reader.
    pub device_timed_reads: usize,
    /// Which fixture this database was populated from, when the caller said.
    pub fixture: Option<String>,
}

/// A crossing, joined against the roster.
#[derive(Debug, Serialize)]
pub struct AcceptedReadView {
    /// Deterministic identifier. Stable across re-derivation, including after a restart.
    pub id: AcceptedReadId,
    /// When the crossing happened.
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    /// Checkpoint name.
    pub checkpoint: String,
    /// The chip that crossed.
    pub chip: ChipId,
    /// Bib, when the chip resolved to somebody on the roster.
    pub bib: Option<String>,
    /// Name, when the chip resolved.
    pub name: Option<String>,
    /// Participant identifier, when the chip resolved.
    ///
    /// `null` is a flagged crossing, not a discarded one: the chip is not on this roster,
    /// which is usually a roster error rather than a phantom read.
    pub participant: Option<ParticipantId>,
    /// Which crossing of this checkpoint this was, for this participant.
    pub lap: Option<u16>,
    /// How many raw reads made up the burst behind this crossing.
    pub burst_reads: usize,
    /// Signal strength of the credited read.
    pub rssi_dbm: Option<i16>,
    /// The raw read that was credited. Walk here to get back to the bytes.
    pub source_raw_read: RawReadId,
}

/// Everything derivation concluded, with the counts worth checking.
#[derive(Debug, Serialize)]
pub struct DerivationReport {
    /// Which fixture supplied the roster and policy.
    pub fixture: String,
    /// Reads in the journal.
    pub raw_reads: usize,
    /// Crossings.
    pub accepted: usize,
    /// Suppressed reads.
    pub rejected: usize,
    /// Timing events. Fewer than `accepted` when a chip did not resolve.
    pub timing_events: usize,
    /// Crossings by a chip that is not on the roster.
    pub unassigned_crossings: usize,
    /// Which rule suppressed how many reads.
    pub rejections_by_reason: BTreeMap<String, usize>,
    /// The crossings themselves.
    pub accepted_reads: Vec<AcceptedReadView>,
}

impl DerivationReport {
    /// Joins a derivation against the fixture that produced it.
    #[must_use]
    pub fn build(fixture: &FixtureEvent, raw_reads: usize, derivation: &Derivation) -> Self {
        let laps: BTreeMap<AcceptedReadId, u16> = derivation
            .timing_events
            .iter()
            .map(|event| {
                let TimingEventOrigin::AcceptedRead { accepted_read } = event.origin;
                (accepted_read, event.lap)
            })
            .collect();

        let mut rejections_by_reason: BTreeMap<String, usize> = BTreeMap::new();
        for entry in &derivation.rejected {
            *rejections_by_reason
                .entry(entry.reason.label().to_owned())
                .or_insert(0) += 1;
        }

        let accepted_reads: Vec<AcceptedReadView> = derivation
            .accepted
            .iter()
            .map(|crossing| view(fixture, crossing, laps.get(&crossing.id).copied()))
            .collect();

        Self {
            fixture: fixture.slug.to_owned(),
            raw_reads,
            accepted: derivation.accepted.len(),
            rejected: derivation.rejected.len(),
            timing_events: derivation.timing_events.len(),
            unassigned_crossings: accepted_reads
                .iter()
                .filter(|view| view.participant.is_none())
                .count(),
            rejections_by_reason,
            accepted_reads,
        }
    }
}

/// Joins one crossing against the roster and checkpoint list.
fn view(fixture: &FixtureEvent, crossing: &AcceptedRead, lap: Option<u16>) -> AcceptedReadView {
    let checkpoint = fixture
        .checkpoints
        .iter()
        .find(|checkpoint| checkpoint.id == crossing.checkpoint)
        .map_or_else(|| crossing.checkpoint.to_string(), |c| c.name.clone());

    let participant = crossing.participant.and_then(|id| {
        fixture
            .participants
            .iter()
            .find(|participant| participant.id == id)
    });

    AcceptedReadView {
        id: crossing.id,
        at: crossing.at,
        checkpoint,
        chip: crossing.chip.clone(),
        bib: participant.map(|p| p.bib.as_str().to_owned()),
        name: participant.map(|p| p.name.clone()),
        participant: crossing.participant,
        lap,
        burst_reads: crossing.burst_size(),
        rssi_dbm: crossing.rssi_dbm,
        source_raw_read: crossing.source_raw_read,
    }
}

/// What a simulated run did.
#[derive(Debug, Serialize)]
pub struct SimulationReport {
    /// Which fixture ran.
    pub fixture: String,
    /// The reader it ran on.
    pub reader: ReaderId,
    /// The seed. Replaying with the same seed replays the same race.
    pub seed: u64,
    /// Crossings the fixture planned.
    pub planned_crossings: usize,
    /// Reads the simulator was going to emit.
    pub reads_scripted: usize,
    /// Reads this process actually took off the channel.
    pub reads_received: usize,
    /// Reads confirmed durable.
    ///
    /// Deliberately reported separately from `reads_received`. The gap between "received"
    /// and "persisted" is the number that matters when something goes wrong, and a system
    /// that reports only one of them cannot tell you it lost anything.
    pub reads_persisted: usize,
    /// Total reads in the journal afterwards, including any from earlier runs.
    pub journal_total: u64,
    /// Sequence number of the first read this run wrote.
    pub first_seq: Option<u64>,
    /// Sequence number of the last read this run wrote.
    pub last_seq: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use splitforge_domain::{ChipId, RejectedRead, RejectionReason};

    #[test]
    fn an_empty_derivation_reports_zeroes_rather_than_omitting_fields() {
        let fixture = splitforge_testkit::five_k();
        let report = DerivationReport::build(&fixture, 0, &Derivation::default());
        let json = serde_json::to_value(&report).expect("serialize");

        assert_eq!(json["accepted"], 0);
        assert_eq!(json["raw_reads"], 0);
        assert_eq!(json["fixture"], "five-k");
        assert!(json["rejections_by_reason"].is_object());
    }

    #[test]
    fn rejections_are_counted_by_rule() {
        let fixture = splitforge_testkit::five_k();
        let derivation = Derivation {
            rejected: vec![
                RejectedRead {
                    raw_read: RawReadId::new(),
                    reason: RejectionReason::UnmappedReader {
                        reader: ReaderId::new("stray"),
                        antenna: Some(3),
                    },
                },
                RejectedRead {
                    raw_read: RawReadId::new(),
                    reason: RejectionReason::UnmappedReader {
                        reader: ReaderId::new("stray"),
                        antenna: None,
                    },
                },
                RejectedRead {
                    raw_read: RawReadId::new(),
                    reason: RejectionReason::BelowMinLap {
                        min_lap_ms: 1_000,
                        actual_ms: 40,
                    },
                },
            ],
            ..Derivation::default()
        };

        let report = DerivationReport::build(&fixture, 3, &derivation);
        assert_eq!(report.rejections_by_reason["unmapped_reader"], 2);
        assert_eq!(report.rejections_by_reason["below_min_lap"], 1);
    }

    #[test]
    fn a_crossing_by_an_unrostered_chip_is_reported_as_such() {
        let fixture = splitforge_testkit::five_k();
        let finish = fixture.checkpoint("finish").expect("finish");
        let raw = RawReadId::new();
        let crossing = AcceptedRead::new(
            finish.id,
            ChipId::new("E2801160600002FFFFFFFFFF"),
            None,
            splitforge_testkit::RACE_START,
            raw,
            vec![raw],
            Some(-50),
        );
        let derivation = Derivation {
            accepted: vec![crossing],
            ..Derivation::default()
        };

        let report = DerivationReport::build(&fixture, 1, &derivation);
        assert_eq!(report.unassigned_crossings, 1);
        assert_eq!(report.accepted_reads[0].checkpoint, "finish");
        assert_eq!(report.accepted_reads[0].bib, None);
        assert_eq!(report.accepted_reads[0].lap, None);
    }
}
