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
    AcceptedRead, AcceptedReadId, Checkpoint, ChipId, Derivation, DeviceClockState, Event,
    ManualEntryId, ParticipantId, Race, RaceConfig, RaceSession, RawReadId, Reader, ReaderId,
    StoredClockStep, StoredManualEntry, StoredRawRead, TimestampSource, TimingEventOrigin,
    TimingPolicy,
};
use splitforge_storage::{AuditEntry, ImportSummary};
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
    /// Races already configured.
    pub races: usize,
}

/// One recorded wall-clock discontinuity, as an operator wants to see it.
#[derive(Debug, Serialize)]
pub struct ClockStepView {
    /// Insertion sequence. The only ordering here that does not come from the clock under
    /// suspicion.
    pub seq: u64,
    /// The wall clock immediately before the jump was noticed.
    #[serde(with = "time::serde::rfc3339")]
    pub from: OffsetDateTime,
    /// The wall clock immediately after.
    #[serde(with = "time::serde::rfc3339")]
    pub to: OffsetDateTime,
    /// How much real time actually passed between the two readings.
    pub monotonic_ms: u64,
    /// Wall movement minus real movement. Negative means the clock went backwards.
    pub step_ms: i64,
    /// `forwards` or `backwards`, so a reader does not have to interpret a sign.
    pub direction: &'static str,
}

/// Every recorded step, newest first, with the summary an operator reads first.
#[derive(Debug, Serialize)]
pub struct ClockStepsView {
    /// How many are listed here.
    pub steps: usize,
    /// The largest by magnitude among those listed, keeping its sign.
    pub largest_ms: Option<i64>,
    /// The steps themselves.
    pub entries: Vec<ClockStepView>,
}

impl ClockStepsView {
    /// Builds the view from what storage returned.
    #[must_use]
    pub fn of(steps: &[StoredClockStep]) -> Self {
        let entries: Vec<ClockStepView> = steps
            .iter()
            .map(|stored| ClockStepView {
                seq: stored.seq,
                from: stored.step.observed_before,
                to: stored.step.observed_after,
                monotonic_ms: stored.step.monotonic_ms,
                step_ms: stored.step.step_ms,
                direction: if stored.step.is_backward() {
                    "backwards"
                } else {
                    "forwards"
                },
            })
            .collect();

        let largest_ms = entries
            .iter()
            .map(|entry| entry.step_ms)
            .max_by_key(|step| step.unsigned_abs());

        Self {
            steps: entries.len(),
            largest_ms,
            entries,
        }
    }
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

/// An event.
#[derive(Debug, Serialize)]
pub struct EventView {
    /// Identifier.
    pub id: String,
    /// Name.
    pub name: String,
}

impl EventView {
    /// Builds a view.
    #[must_use]
    pub fn of(event: &Event) -> Self {
        Self {
            id: event.id.to_string(),
            name: event.name.clone(),
        }
    }
}

/// A race.
#[derive(Debug, Serialize)]
pub struct RaceView {
    /// Identifier.
    pub id: String,
    /// Name.
    pub name: String,
    /// The event it belongs to.
    pub event: String,
    /// Scheduled start.
    #[serde(with = "time::serde::rfc3339::option")]
    pub scheduled_start: Option<OffsetDateTime>,
    /// Laps expected.
    pub expected_laps: u16,
}

impl RaceView {
    /// Builds a view.
    #[must_use]
    pub fn of(race: &Race) -> Self {
        Self {
            id: race.id.to_string(),
            name: race.name.clone(),
            event: race.event.to_string(),
            scheduled_start: race.scheduled_start,
            expected_laps: race.expected_laps,
        }
    }
}

/// A checkpoint.
#[derive(Debug, Serialize)]
pub struct CheckpointView {
    /// Identifier.
    pub id: String,
    /// Name.
    pub name: String,
    /// Role on the course.
    pub kind: String,
    /// Position in the expected sequence.
    pub sequence: u16,
}

impl CheckpointView {
    /// Builds a view.
    #[must_use]
    pub fn of(checkpoint: &Checkpoint) -> Self {
        Self {
            id: checkpoint.id.to_string(),
            name: checkpoint.name.clone(),
            kind: format!("{:?}", checkpoint.kind).to_lowercase(),
            sequence: checkpoint.sequence,
        }
    }
}

/// A participant, with the chips assigned to them.
#[derive(Debug, Serialize)]
pub struct ParticipantView {
    /// Bib.
    pub bib: String,
    /// Name.
    pub name: String,
    /// Identifier.
    pub id: ParticipantId,
    /// Chips assigned to them, in window order.
    pub chips: Vec<String>,
}

/// A chip assignment.
#[derive(Debug, Serialize)]
pub struct AssignmentView {
    /// The chip.
    pub chip: String,
    /// The bib wearing it.
    pub bib: Option<String>,
    /// Start of validity.
    #[serde(with = "time::serde::rfc3339")]
    pub valid_from: OffsetDateTime,
    /// End of validity, exclusive.
    #[serde(with = "time::serde::rfc3339::option")]
    pub valid_until: Option<OffsetDateTime>,
}

/// What an import changed.
#[derive(Debug, Serialize)]
pub struct ImportReport {
    /// The file read.
    pub file: String,
    /// The race it applied to.
    pub race: String,
    /// Rows that did not exist before.
    pub inserted: usize,
    /// Rows that existed and were changed.
    pub updated: usize,
    /// Rows that existed and already matched.
    pub unchanged: usize,
    /// Rows considered.
    pub total: usize,
}

impl ImportReport {
    /// Builds a report.
    #[must_use]
    pub fn of(file: &str, race: &str, summary: ImportSummary) -> Self {
        Self {
            file: file.to_owned(),
            race: race.to_owned(),
            inserted: summary.inserted,
            updated: summary.updated,
            unchanged: summary.unchanged,
            total: summary.total(),
        }
    }
}

/// A configured reader, with what the journal has seen from it.
///
/// Connection state is deliberately absent. No reader protocol exists until Milestone 3, so
/// everything here is either configuration or an observation from the journal — never a
/// claim about a link this build cannot open. See `docs/hardware-support.md`.
#[derive(Debug, Serialize)]
pub struct ReaderStatusView {
    /// Operator-assigned identifier.
    pub id: String,
    /// Human-readable description.
    pub label: String,
    /// Configured address, if any. Unused until Milestone 3.
    pub endpoint: Option<String>,
    /// How far its clock is trusted.
    pub timestamp_trust: String,
    /// Checkpoints it covers, as `antenna → checkpoint`.
    pub covers: Vec<String>,
    /// Reads in the journal from this reader.
    pub reads: usize,
    /// Reads within the recent window.
    pub reads_recent: usize,
    /// The most recent read from it.
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_read_at: Option<OffsetDateTime>,
    /// How wide the recent window was.
    pub window_secs: u64,
}

/// A recorded start or stop.
#[derive(Debug, Serialize)]
pub struct SessionView {
    /// Sequence within the database.
    pub seq: u64,
    /// What happened.
    pub action: String,
    /// The instant recorded.
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    /// Who recorded it.
    pub actor: String,
    /// Why, when the operator said.
    pub note: Option<String>,
    /// When the row was written.
    #[serde(with = "time::serde::rfc3339")]
    pub recorded_at: OffsetDateTime,
}

impl SessionView {
    /// Builds a view.
    #[must_use]
    pub fn of(session: &RaceSession) -> Self {
        Self {
            seq: session.seq,
            action: session.action.as_str().to_owned(),
            at: session.at,
            actor: session.actor.clone(),
            note: session.note.clone(),
            recorded_at: session.recorded_at,
        }
    }
}

/// One audit trail entry.
#[derive(Debug, Serialize)]
pub struct AuditView {
    /// Sequence within the database.
    pub seq: u64,
    /// When.
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    /// Who.
    pub actor: String,
    /// What.
    pub action: String,
    /// What it applied to.
    pub subject: Option<String>,
    /// Structured detail, when the action recorded any.
    pub detail: Option<serde_json::Value>,
}

impl AuditView {
    /// Builds a view, parsing the stored detail when it is valid JSON.
    #[must_use]
    pub fn of(entry: &AuditEntry) -> Self {
        Self {
            seq: entry.seq,
            at: entry.at,
            actor: entry.actor.clone(),
            action: entry.action.clone(),
            subject: entry.subject.clone(),
            detail: entry
                .detail
                .as_deref()
                .and_then(|text| serde_json::from_str(text).ok()),
        }
    }
}

/// The timing policy in force.
#[derive(Debug, Serialize)]
pub struct PolicyView {
    /// The race.
    pub race: String,
    /// The policy itself.
    pub policy: TimingPolicy,
    /// Which clock placement is measured from.
    pub start_mode: &'static str,
    /// Per-checkpoint overrides, keyed by checkpoint name rather than id.
    pub per_checkpoint_names: BTreeMap<String, String>,
}

/// What the database holds.
#[derive(Debug, Serialize)]
pub struct StatusReport {
    /// Where the database lives.
    pub database: String,
    /// Schema version.
    pub schema_version: i64,
    /// The race summarized, when one was resolved.
    pub race: Option<String>,
    /// Roster size.
    pub participants: usize,
    /// Checkpoints configured.
    pub checkpoints: usize,
    /// Chip assignments configured.
    pub chip_assignments: usize,
    /// Readers configured.
    pub readers: usize,
    /// Antenna mappings configured.
    pub antenna_mappings: usize,
    /// The gun time in force, recorded or scheduled.
    #[serde(with = "time::serde::rfc3339::option")]
    pub gun_time: Option<OffsetDateTime>,
    /// Whether the race has been started and not since stopped.
    pub running: bool,
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
}

/// One thing `doctor` found.
#[derive(Debug, Serialize)]
pub struct Finding {
    /// How much it matters: `error` or `warning`.
    pub severity: String,
    /// A dotted machine-readable name.
    pub check: String,
    /// What is wrong, in the operator's terms.
    pub detail: String,
}

impl Finding {
    /// Something that will produce wrong results or lose data.
    #[must_use]
    pub fn error(check: &str, detail: impl Into<String>) -> Self {
        Self {
            severity: "error".to_owned(),
            check: check.to_owned(),
            detail: detail.into(),
        }
    }

    /// Something worth knowing before the gun.
    #[must_use]
    pub fn warning(check: &str, detail: impl Into<String>) -> Self {
        Self {
            severity: "warning".to_owned(),
            check: check.to_owned(),
            detail: detail.into(),
        }
    }
}

/// What `doctor` concluded.
#[derive(Debug, Serialize)]
pub struct DoctorReport {
    /// Where the database lives.
    pub database: String,
    /// Schema version.
    pub schema_version: i64,
    /// What the device's time source is, or why that could not be determined.
    ///
    /// Always present. A device checked before a race should be able to show that the
    /// question was asked, and an absent field would leave "not synchronized" and "not
    /// asked" looking identical.
    pub clock_source: crate::clock_source::ClockSourceView,
    /// Checks that ran.
    pub checks_run: usize,
    /// Problems that will produce wrong results or lose data.
    pub errors: usize,
    /// Problems worth knowing before the gun.
    pub warnings: usize,
    /// The findings themselves.
    pub findings: Vec<Finding>,
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

/// One manual entry, joined against the roster.
///
/// Both times are here on purpose. `at` is what the operator claims happened; `recorded_at`
/// is when they typed it, and the gap between them can be an hour of walking back from a
/// checkpoint with a clipboard. A dispute needs both.
#[derive(Debug, Serialize)]
pub struct ManualEntryView {
    /// Insertion sequence.
    pub seq: u64,
    /// The entry's own identifier. Random, not derived — see [`ManualEntryId`].
    pub id: ManualEntryId,
    /// Bib, when the roster still lists the participant.
    ///
    /// `null` should be unreachable: the foreign key refuses an entry for a participant
    /// who does not exist. A view that asserted that would turn a data question into a
    /// panic, so it reports what it found instead.
    pub bib: Option<String>,
    /// Name, when the roster still lists the participant.
    pub name: Option<String>,
    /// Which participant the operator named.
    pub participant: ParticipantId,
    /// Checkpoint name, falling back to its identifier.
    pub checkpoint: String,
    /// **When the operator claims it happened.**
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    /// When the row was written — the data-entry time.
    #[serde(with = "time::serde::rfc3339")]
    pub recorded_at: OffsetDateTime,
    /// Who entered it.
    pub actor: String,
    /// Why it was entered by hand.
    pub reason: String,
}

impl ManualEntryView {
    /// Joins one stored entry against the roster and checkpoint list.
    #[must_use]
    pub fn of(stored: &StoredManualEntry, config: &RaceConfig) -> Self {
        let participant = config.participant_by_id(stored.entry.participant);
        let checkpoint = config
            .checkpoint_by_id(stored.entry.checkpoint)
            .map_or_else(|| stored.entry.checkpoint.to_string(), |c| c.name.clone());

        Self {
            seq: stored.seq,
            id: stored.entry.id,
            bib: participant.map(|p| p.bib.as_str().to_owned()),
            name: participant.map(|p| p.name.clone()),
            participant: stored.entry.participant,
            checkpoint,
            at: stored.entry.at,
            recorded_at: stored.recorded_at,
            actor: stored.entry.actor.clone(),
            reason: stored.entry.reason.clone(),
        }
    }
}

/// Every manual entry recorded for one race, oldest first.
#[derive(Debug, Serialize)]
pub struct ManualEntriesView {
    /// Which race they belong to.
    pub race: String,
    /// How many are listed here.
    pub entries: usize,
    /// The entries themselves.
    pub manual_entries: Vec<ManualEntryView>,
}

impl ManualEntriesView {
    /// Builds the view from what storage returned.
    #[must_use]
    pub fn of(entries: &[StoredManualEntry], config: &RaceConfig) -> Self {
        let manual_entries: Vec<ManualEntryView> = entries
            .iter()
            .map(|stored| ManualEntryView::of(stored, config))
            .collect();

        Self {
            race: config.race.name.clone(),
            entries: manual_entries.len(),
            manual_entries,
        }
    }
}

/// Everything derivation concluded, with the counts worth checking.
#[derive(Debug, Serialize)]
pub struct DerivationReport {
    /// Which race supplied the roster and policy.
    pub race: String,
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
    /// Joins a derivation against the configuration that produced it.
    #[must_use]
    pub fn build(config: &RaceConfig, raw_reads: usize, derivation: &Derivation) -> Self {
        // Only crossings have a lap to attach to a row in this report; a manual entry has
        // no accepted read to key on, and is counted separately below.
        let laps: BTreeMap<AcceptedReadId, u16> = derivation
            .timing_events
            .iter()
            .filter_map(|event| match &event.origin {
                TimingEventOrigin::AcceptedRead { accepted_read } => {
                    Some((*accepted_read, event.lap))
                }
                TimingEventOrigin::Manual { .. } => None,
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
            .map(|crossing| view(config, crossing, laps.get(&crossing.id).copied()))
            .collect();

        Self {
            race: config.race.name.clone(),
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
fn view(config: &RaceConfig, crossing: &AcceptedRead, lap: Option<u16>) -> AcceptedReadView {
    let checkpoint = config
        .checkpoint_by_id(crossing.checkpoint)
        .map_or_else(|| crossing.checkpoint.to_string(), |c| c.name.clone());

    let participant = crossing
        .participant
        .and_then(|id| config.participant_by_id(id));

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
    /// Which crossing plan ran.
    pub scenario: String,
    /// Which race the reads belong to.
    pub race: String,
    /// The reader it ran on.
    pub reader: ReaderId,
    /// The seed. Replaying with the same seed replays the same race.
    pub seed: u64,
    /// Crossings the plan contained.
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

/// Builds a reader status view.
#[must_use]
pub fn reader_status(
    reader: &Reader,
    config: &RaceConfig,
    reads: &[StoredRawRead],
    window_secs: u64,
) -> ReaderStatusView {
    let mine: Vec<&StoredRawRead> = reads
        .iter()
        .filter(|stored| stored.read.source == reader.id)
        .collect();

    let last_read_at = mine
        .last()
        .map(|stored| stored.read.authoritative_timestamp());
    let cutoff = last_read_at
        .map(|last| last - time::Duration::seconds(i64::try_from(window_secs).unwrap_or(i64::MAX)));

    let covers: Vec<String> = config
        .antennas
        .entries()
        .filter(|(mapped, _, _)| **mapped == reader.id)
        .map(|(_, antenna, checkpoint)| {
            let name = config
                .checkpoint_by_id(checkpoint)
                .map_or_else(|| checkpoint.to_string(), |c| c.name.clone());
            match antenna {
                Some(antenna) => format!("antenna {antenna} → {name}"),
                None => format!("all antennas → {name}"),
            }
        })
        .collect();

    ReaderStatusView {
        id: reader.id.as_str().to_owned(),
        label: reader.label.clone(),
        endpoint: reader.endpoint.clone(),
        timestamp_trust: reader.timestamp_trust.as_str().to_owned(),
        covers,
        reads: mine.len(),
        reads_recent: cutoff.map_or(0, |cutoff| {
            mine.iter()
                .filter(|stored| stored.read.authoritative_timestamp() >= cutoff)
                .count()
        }),
        last_read_at,
        window_secs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use splitforge_domain::{ChipId, RejectedRead, RejectionReason};

    fn config() -> RaceConfig {
        splitforge_testkit::five_k().config
    }

    #[test]
    fn an_empty_derivation_reports_zeroes_rather_than_omitting_fields() {
        let report = DerivationReport::build(&config(), 0, &Derivation::default());
        let json = serde_json::to_value(&report).expect("serialize");

        assert_eq!(json["accepted"], 0);
        assert_eq!(json["raw_reads"], 0);
        assert_eq!(json["race"], "5K");
        assert!(json["rejections_by_reason"].is_object());
    }

    #[test]
    fn rejections_are_counted_by_rule() {
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

        let report = DerivationReport::build(&config(), 3, &derivation);
        assert_eq!(report.rejections_by_reason["unmapped_reader"], 2);
        assert_eq!(report.rejections_by_reason["below_min_lap"], 1);
    }

    #[test]
    fn a_crossing_by_an_unrostered_chip_is_reported_as_such() {
        let config = config();
        let finish = config.checkpoint("finish").expect("finish");
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

        let report = DerivationReport::build(&config, 1, &derivation);
        assert_eq!(report.unassigned_crossings, 1);
        assert_eq!(report.accepted_reads[0].checkpoint, "finish");
        assert_eq!(report.accepted_reads[0].bib, None);
        assert_eq!(report.accepted_reads[0].lap, None);
    }

    #[test]
    fn reader_status_reports_coverage_without_claiming_a_connection() {
        let config = config();
        let reader = config.readers.first().expect("a reader");
        let view = reader_status(reader, &config, &[], 60);

        assert_eq!(view.id, "mat");
        assert_eq!(view.reads, 0);
        assert_eq!(view.covers.len(), 2, "two antennas, two checkpoints");
        assert!(view.covers.iter().any(|line| line.contains("finish")));

        // The point of the assertion: there is no field that could carry a fabricated
        // connection state, because none exists until Milestone 3.
        let json = serde_json::to_value(&view).expect("serialize");
        assert!(json.get("connected").is_none());
        assert_eq!(json["endpoint"], serde_json::Value::Null);
    }

    #[test]
    fn an_audit_detail_that_is_not_json_does_not_break_the_view() {
        let entry = AuditEntry {
            seq: 1,
            at: splitforge_testkit::RACE_START,
            actor: "phillip".to_owned(),
            action: "roster.import".to_owned(),
            subject: Some("5K".to_owned()),
            detail: Some("not json at all".to_owned()),
        };
        assert_eq!(AuditView::of(&entry).detail, None);
    }
}
