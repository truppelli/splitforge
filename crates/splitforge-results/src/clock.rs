//! What the device's clock did to a result's accuracy (ADR-0048).
//!
//! Results are published whatever the clock was doing: refusing would be the software
//! deciding whether a result stands, and that belongs to the organizer
//! ([Q11](../../../docs/open-questions.md#q11-clock-error-budget-enforcement)). What the
//! software owes is to say which results it cannot vouch for, on the result itself. Two things
//! are known without measuring the clock against a reference, and both are flagged here:
//!
//! - a crossing timed by the device while its clock had no trustworthy source
//!   ([`ResultFlag::UntrustedDeviceClock`]);
//! - a recorded clock step between a result's start and its finish
//!   ([`ResultFlag::ClockStepDuringResult`]).
//!
//! A pass over finished entries rather than part of [`score`](crate::score): it changes no
//! time, status or place, so it cannot change what scoring decided, and the digest a revision
//! is compared by does not see it.

use std::collections::{BTreeMap, BTreeSet};

use splitforge_domain::{
    ClockStep, Derivation, ResultEntry, ResultFlag, StoredRawRead, TimestampSource, TimingEventId,
    TimingEventOrigin,
};
use time::OffsetDateTime;

/// The timing events whose time came from the device's clock while it had no trustworthy
/// source.
///
/// A read timed by the reader's own clock is not included: whether to trust that clock is
/// [Q3](../../../docs/open-questions.md#q3-reader-clock-trust-defaults), not this. A manual
/// entry is not included either, because a person typed its time.
#[must_use]
pub fn untrusted_events(
    reads: &[StoredRawRead],
    derivation: &Derivation,
) -> BTreeSet<TimingEventId> {
    let untrusted_reads: BTreeSet<_> = reads
        .iter()
        .map(|stored| &stored.read)
        .filter(|read| {
            matches!(read.timestamp_source, TimestampSource::DeviceReceipt { .. })
                && !read.device_clock_state.is_trustworthy()
        })
        .map(|read| read.id)
        .collect();
    let crossings: BTreeMap<_, _> = derivation
        .accepted
        .iter()
        .map(|crossing| (crossing.id, crossing.source_raw_read))
        .collect();

    derivation
        .timing_events
        .iter()
        .filter(|event| match &event.origin {
            TimingEventOrigin::AcceptedRead { accepted_read } => crossings
                .get(accepted_read)
                .is_some_and(|read| untrusted_reads.contains(read)),
            TimingEventOrigin::Manual { .. } => false,
        })
        .map(|event| event.id)
        .collect()
}

/// Adds the clock flags to each entry they apply to.
///
/// A step applies to an entry when the span the step was observed across overlaps the span
/// the entry's time was measured across: from the earlier of the gun and its start, to its
/// finish. Deliberately generous at the edges, because the step was only seen between two
/// samples ten seconds apart, and a flag on a result that was fine costs a look where a
/// missing one costs a wrong time nobody questions.
pub fn flag_clock(
    entries: &mut [ResultEntry],
    untrusted: &BTreeSet<TimingEventId>,
    steps: &[ClockStep],
    gun: Option<OffsetDateTime>,
) {
    for entry in entries {
        if entry.timing_events.iter().any(|id| untrusted.contains(id)) {
            entry.flags.push(ResultFlag::UntrustedDeviceClock);
        }

        let Some(finish) = entry.finish_at else {
            continue;
        };
        let Some(from) = [gun, entry.start_at].into_iter().flatten().min() else {
            continue;
        };
        let stepped = steps.iter().any(|step| {
            let earlier = step.observed_before.min(step.observed_after);
            let later = step.observed_before.max(step.observed_after);
            earlier <= finish && later >= from
        });
        if stepped {
            entry.flags.push(ResultFlag::ClockStepDuringResult);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use splitforge_domain::{
        AcceptedRead, Bib, CheckpointId, ChipId, DeviceClockState, FallbackReason, ManualEntryId,
        ParticipantId, RawRead, RawReadId, ReaderId, ResultStatus, StatusSource, TimingEvent,
    };
    use time::Duration;

    const GUN: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;

    fn at(seconds: i64) -> OffsetDateTime {
        GUN + Duration::seconds(seconds)
    }

    fn read(state: DeviceClockState, source: TimestampSource) -> StoredRawRead {
        StoredRawRead {
            seq: 1,
            recorded_at: at(0),
            payload_sha256: String::new(),
            read: RawRead {
                id: RawReadId::new(),
                source: ReaderId::new("mat"),
                antenna: Some(1),
                chip: ChipId::new("E280"),
                reader_timestamp: Some(at(0)),
                reader_uptime_us: None,
                received_at: at(0),
                received_at_monotonic_ns: None,
                rssi_dbm: None,
                device_clock_state: state,
                timestamp_source: source,
                clock_offset_ms: None,
                raw_payload: Vec::new(),
            },
        }
    }

    const DEVICE: TimestampSource = TimestampSource::DeviceReceipt {
        reason: FallbackReason::NoReaderTimestamp,
    };

    /// One crossing per read, each credited to its own timing event.
    fn derive_from(reads: &[StoredRawRead]) -> Derivation {
        let participant = ParticipantId::new();
        let checkpoint = CheckpointId::new();
        let accepted: Vec<AcceptedRead> = reads
            .iter()
            .map(|stored| {
                AcceptedRead::new(
                    checkpoint,
                    stored.read.chip.clone(),
                    Some(participant),
                    at(0),
                    stored.read.id,
                    vec![stored.read.id],
                    None,
                )
            })
            .collect();
        let timing_events = accepted
            .iter()
            .map(|crossing| {
                TimingEvent::from_accepted(participant, checkpoint, at(0), 1, crossing.id)
            })
            .collect();
        Derivation {
            accepted,
            rejected: Vec::new(),
            timing_events,
        }
    }

    #[test]
    fn only_a_device_timed_read_under_an_untrustworthy_clock_is_untrusted() {
        let reads = [
            read(DeviceClockState::Unsynced, DEVICE),
            read(DeviceClockState::Manual, DEVICE),
            read(DeviceClockState::NtpSynced, DEVICE),
            // The reader's clock timed it, so the device's clock never touched the time.
            read(DeviceClockState::Unsynced, TimestampSource::ReaderUtc),
        ];
        let derivation = derive_from(&reads);
        let untrusted = untrusted_events(&reads, &derivation);

        let expected: BTreeSet<_> = derivation.timing_events[..2]
            .iter()
            .map(|event| event.id)
            .collect();
        assert_eq!(untrusted, expected);
    }

    #[test]
    fn a_manual_entry_is_never_untrusted() {
        let reads = [read(DeviceClockState::Unsynced, DEVICE)];
        let mut derivation = derive_from(&reads);
        derivation.timing_events = vec![TimingEvent::from_manual(
            ParticipantId::new(),
            CheckpointId::new(),
            at(0),
            1,
            ManualEntryId::new(),
        )];
        assert!(untrusted_events(&reads, &derivation).is_empty());
    }

    fn entry(start: Option<i64>, finish: Option<i64>, events: Vec<TimingEventId>) -> ResultEntry {
        ResultEntry {
            participant: ParticipantId::new(),
            bib: Bib::new("101"),
            name: "Runner 101".to_owned(),
            status: ResultStatus::Finished,
            status_source: StatusSource::Derived,
            status_reason: None,
            start_at: start.map(at),
            finish_at: finish.map(at),
            gun_time_ms: None,
            chip_time_ms: None,
            scoring_time_ms: None,
            place: None,
            flags: Vec::new(),
            timing_events: events,
        }
    }

    fn step(before: i64, after: i64) -> ClockStep {
        ClockStep {
            observed_before: at(before),
            observed_after: at(after),
            monotonic_ms: 10_000,
            step_ms: (after - before) * 1_000 - 10_000,
        }
    }

    #[test]
    fn an_entry_resting_on_an_untrusted_event_is_flagged_and_its_time_is_kept() {
        let bad = TimingEventId::new();
        let mut entries = vec![
            entry(Some(5), Some(1_200), vec![TimingEventId::new(), bad]),
            entry(Some(5), Some(1_300), vec![TimingEventId::new()]),
        ];
        let before = entries.clone();
        flag_clock(&mut entries, &BTreeSet::from([bad]), &[], Some(GUN));

        assert_eq!(entries[0].flags, vec![ResultFlag::UntrustedDeviceClock]);
        assert!(entries[1].flags.is_empty());
        assert_eq!(
            entries[0].finish_at, before[0].finish_at,
            "nothing but the flag moves"
        );
    }

    #[test]
    fn a_step_between_the_gun_and_the_finish_is_flagged() {
        // Forward an hour at 600 s: everything read after it is an hour later than it was.
        let steps = [step(600, 4_210)];
        let mut entries = vec![
            entry(Some(5), Some(1_200), Vec::new()),
            // Finished before the step.
            entry(Some(5), Some(590), Vec::new()),
            // Started after it: both ends on the same, new clock.
            entry(Some(4_300), Some(5_000), Vec::new()),
            // Not finished, so there is no elapsed time to be wrong.
            entry(Some(5), None, Vec::new()),
        ];
        flag_clock(&mut entries, &BTreeSet::new(), &steps, None);

        assert_eq!(entries[0].flags, vec![ResultFlag::ClockStepDuringResult]);
        for later in &entries[1..] {
            assert!(later.flags.is_empty(), "{later:?}");
        }
    }

    #[test]
    fn a_gun_time_spans_from_the_gun_even_when_the_chip_start_is_later() {
        // Gun at 0, the step at 100, this runner's start mat at 300. Their gun time crosses
        // the step even though their chip time does not.
        let mut entries = vec![entry(Some(300), Some(1_200), Vec::new())];
        flag_clock(&mut entries, &BTreeSet::new(), &[step(100, 120)], Some(GUN));
        assert_eq!(entries[0].flags, vec![ResultFlag::ClockStepDuringResult]);
    }

    #[test]
    fn a_backward_step_is_flagged_across_its_whole_span() {
        // Back ten minutes at 1_000: times from 400 to 1_000 were read twice.
        let mut entries = vec![entry(Some(5), Some(700), Vec::new())];
        flag_clock(&mut entries, &BTreeSet::new(), &[step(1_000, 400)], None);
        assert_eq!(entries[0].flags, vec![ResultFlag::ClockStepDuringResult]);
    }
}
