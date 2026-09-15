//! # splitforge-engine
//!
//! Read acceptance, deduplication, participant assignment, and checkpoint/lap state.
//!
//! ## Boundaries
//!
//! - **May depend on:** splitforge-domain
//! - **Must never depend on:** splitforge-llrp or any protocol crate, splitforge-api, splitforge-sync
//!
//! These rules come from ADR-0001 and are tabulated in `docs/architecture.md`.
//! They are enforced by `crates/splitforge-testkit/tests/dependency_rules.rs`, not left to
//! review (ADR-0012).
//!
//! ## Derivation is a pure function
//!
//! [`derive`] takes evidence plus policy and returns conclusions. It performs no I/O, holds
//! no state between calls, and is **deterministic**: the same journal and the same policy
//! always produce byte-identical output, including identifiers.
//!
//! That property is what makes crash recovery boring. On restart, re-derive from the
//! journal and compare — if the output differs, something is wrong with the code, not with
//! the race.

// No panicking on any path reachable during an event: a corrupt frame, a missing
// field, or an out-of-range value must become an error the caller can act on, never a
// timer that stops mid-race. Test code is exempt, where panicking on the unexpected is
// the point. See CONTRIBUTING.md, "Code standards".
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use std::cmp::Reverse;
use std::collections::BTreeMap;

use splitforge_domain::{
    AcceptedRead, AcceptedReadId, AntennaMap, CheckpointId, ChipId, ChipRegistry, Derivation,
    ManualEntry, ParticipantId, RawReadId, RejectedRead, RejectionReason, SelectionRule,
    StoredRawRead, TimingEvent, TimingPolicy,
};
use time::{Duration, OffsetDateTime};

/// Everything [`derive`] needs.
#[derive(Debug, Clone, Copy)]
pub struct DerivationInput<'a> {
    /// The journal contents, in any order.
    pub reads: &'a [StoredRawRead],
    /// The rules in force.
    pub policy: &'a TimingPolicy,
    /// Time-bounded chip to participant mapping.
    pub chips: &'a ChipRegistry,
    /// Reader and antenna to checkpoint mapping.
    pub antennas: &'a AntennaMap,
    /// What operators wrote down when a chip did not record it.
    ///
    /// A second kind of evidence, not a correction to the first
    /// ([timing model § 6](../../../docs/timing-model.md#6-timing-events-and-manual-entries)).
    /// Manual entries never suppress a read and a read never suppresses an entry: both
    /// produce timing events, and the scoring rules decide which one counts.
    pub manual: &'a [ManualEntry],
    /// When the race started: `RaceConfig::gun_time()`, or `None` with no gun in force.
    ///
    /// **Used only to number laps and to bound the minimum lap**
    /// ([ADR-0029](../../../docs/adr/0029-a-race-starts-at-the-gun.md)). It filters nothing:
    /// every read before it is still grouped, selected, accepted, and turned into a timing
    /// event, as [ADR-0015](../../../docs/adr/0015-race-start-records-the-gun.md) requires.
    pub gun_time: Option<OffsetDateTime>,
}

/// Derives crossings, suppressions, and timing events from raw reads.
///
/// Deterministic and idempotent: calling this twice with the same input produces exactly
/// equal output.
#[must_use]
pub fn derive(input: &DerivationInput<'_>) -> Derivation {
    let mut rejected = Vec::new();
    let mut groups: BTreeMap<(ChipId, CheckpointId), Vec<&StoredRawRead>> = BTreeMap::new();

    // A read whose reader/antenna is not mapped stays in the journal; only its
    // interpretation is withheld, and it becomes interpretable the moment the mapping is
    // fixed and derivation is re-run.
    for stored in input.reads {
        match input
            .antennas
            .resolve(&stored.read.source, stored.read.antenna)
        {
            Some(checkpoint) => groups
                .entry((stored.read.chip.clone(), checkpoint))
                .or_default()
                .push(stored),
            None => rejected.push(RejectedRead {
                raw_read: stored.read.id,
                reason: RejectionReason::UnmappedReader {
                    reader: stored.read.source.clone(),
                    antenna: stored.read.antenna,
                },
            }),
        }
    }

    let mut accepted = Vec::new();
    // Which crossings are on the race side of the gun, for numbering laps once the evidence
    // is merged. Kept beside the crossings rather than on them, because it is a fact about
    // this derivation's gun and not about the crossing.
    let mut race_sides: BTreeMap<AcceptedReadId, bool> = BTreeMap::new();
    for ((chip, checkpoint), mut group) in groups {
        // Sort by authoritative time, then by insertion sequence. Reader timestamps can
        // legitimately arrive out of order relative to receipt, and seq breaks ties
        // deterministically.
        group.sort_by_key(|stored| (stored.read.authoritative_timestamp(), stored.seq));

        let window = Duration::milliseconds(
            i64::try_from(input.policy.min_interval_ms_for(checkpoint)).unwrap_or(i64::MAX),
        );
        let rule = input.policy.selection_rule_for(checkpoint);
        // When the previous crossing was credited, and whether it was on the race side.
        let mut last_credited: Option<(OffsetDateTime, bool)> = None;

        for burst in split_into_bursts(&group, window) {
            let Some(chosen) = select(&burst, rule) else {
                // Nothing in the burst cleared the RSSI floor. Every read is suppressed,
                // and each says which rule suppressed it.
                let floor_dbm = match rule {
                    SelectionRule::FirstAboveRssi { floor_dbm } => floor_dbm,
                    SelectionRule::First | SelectionRule::PeakRssi => i16::MIN,
                };
                for stored in &burst {
                    rejected.push(RejectedRead {
                        raw_read: stored.read.id,
                        reason: RejectionReason::BelowRssiFloor {
                            rssi_dbm: stored.read.rssi_dbm,
                            floor_dbm,
                        },
                    });
                }
                continue;
            };

            let at = chosen.read.authoritative_timestamp();
            let race_side = on_the_race_side(&burst, input.gun_time);

            // A lap credited faster than physically plausible is a re-read, not a
            // superhuman lap.
            //
            // Not measured across the gun (ADR-0029). A warm-up is not the start of a lap, so
            // it cannot make the start crossing after it a re-read. On a course whose start
            // line is its lap line, that rule used to reject the runner's start.
            let lap_violation = input
                .policy
                .min_lap_ms
                .zip(last_credited)
                .filter(|(_, (_, previous_race_side))| *previous_race_side || !race_side)
                .and_then(|(min_lap_ms, (previous, _))| {
                    let elapsed = (at - previous).whole_milliseconds();
                    (elapsed < i128::from(min_lap_ms))
                        .then(|| (min_lap_ms, i64::try_from(elapsed).unwrap_or(i64::MAX)))
                });
            if let Some((min_lap_ms, actual_ms)) = lap_violation {
                for stored in &burst {
                    rejected.push(RejectedRead {
                        raw_read: stored.read.id,
                        reason: RejectionReason::BelowMinLap {
                            min_lap_ms,
                            actual_ms,
                        },
                    });
                }
                continue;
            }

            let participant = input.chips.participant_at(&chip, at);
            let burst_ids: Vec<RawReadId> = burst.iter().map(|stored| stored.read.id).collect();
            let crossing = AcceptedRead::new(
                checkpoint,
                chip.clone(),
                participant,
                at,
                chosen.read.id,
                burst_ids,
                chosen.read.rssi_dbm,
            );

            for stored in &burst {
                if stored.read.id != chosen.read.id {
                    let interval =
                        (stored.read.authoritative_timestamp() - at).whole_milliseconds();
                    rejected.push(RejectedRead {
                        raw_read: stored.read.id,
                        reason: RejectionReason::DuplicateWithinWindow {
                            accepted: crossing.id,
                            interval_ms: i64::try_from(interval).unwrap_or(i64::MAX),
                        },
                    });
                }
            }

            last_credited = Some((at, race_side));
            race_sides.insert(crossing.id, race_side);
            accepted.push(crossing);
        }
    }

    accepted.sort_by(|left, right| left.at.cmp(&right.at).then(left.id.cmp(&right.id)));

    // Both kinds of evidence, merged into one chronological sequence before laps are
    // counted. Interleaving matters: a runner whose chip failed on lap 2 and was written
    // down instead must still be on lap 3 for the read that follows, and counting the two
    // sources separately would give them two lap 1s.
    enum Source<'a> {
        Crossing(&'a AcceptedRead),
        Manual(&'a ManualEntry),
    }

    let mut sources: Vec<(
        OffsetDateTime,
        ParticipantId,
        CheckpointId,
        bool,
        Source<'_>,
    )> = Vec::new();
    for crossing in &accepted {
        if let Some(participant) = crossing.participant {
            sources.push((
                crossing.at,
                participant,
                crossing.checkpoint,
                race_sides.get(&crossing.id).copied().unwrap_or(true),
                Source::Crossing(crossing),
            ));
        }
    }
    for entry in input.manual {
        // One instant, so it is on the race side exactly when it is at or after the gun.
        sources.push((
            entry.at,
            entry.participant,
            entry.checkpoint,
            input.gun_time.is_none_or(|gun| entry.at >= gun),
            Source::Manual(entry),
        ));
    }

    // Sorted by time, then by kind, then by identifier, so a tie resolves the same way on
    // every machine and every re-derivation. A crossing sorts ahead of an entry at the same
    // instant: a chip that did record the runner keeps the lower lap number.
    sources.sort_by(|left, right| {
        let key = |source: &Source<'_>| match source {
            Source::Crossing(crossing) => (0_u8, *crossing.id.as_uuid()),
            Source::Manual(entry) => (1_u8, *entry.id.as_uuid()),
        };
        left.0
            .cmp(&right.0)
            .then_with(|| key(&left.4).cmp(&key(&right.4)))
    });

    // Lap numbers count per participant, not per chip: a participant who swaps a failed
    // chip mid-race is still on the same lap.
    //
    // And from the gun (ADR-0029). Every crossing that was over before the gun is lap 0, and
    // the first on the race side is lap 1, so a warm-up over the lap line does not push the
    // runner's first real lap to lap 2. With no gun, laps count from the first crossing.
    let mut lap_counters: BTreeMap<(ParticipantId, CheckpointId), u16> = BTreeMap::new();
    let mut timing_events = Vec::new();
    for (at, participant, checkpoint, race_side, source) in sources {
        let lap = if !race_side {
            0
        } else {
            let counter = lap_counters.entry((participant, checkpoint)).or_insert(0);
            *counter = counter.saturating_add(1);
            *counter
        };
        timing_events.push(match source {
            Source::Crossing(crossing) => {
                TimingEvent::from_accepted(participant, checkpoint, at, lap, crossing.id)
            }
            Source::Manual(entry) => {
                TimingEvent::from_manual(participant, checkpoint, at, lap, entry.id)
            }
        });
    }

    rejected.sort_by_key(|entry| entry.raw_read.0);

    Derivation {
        accepted,
        rejected,
        timing_events,
    }
}

/// Whether a crossing belongs to the race: its reads were still arriving when the gun went.
///
/// **Judged by the burst's last read, not by the credited one** (ADR-0029). Under the default
/// rule the credited read is the burst's first, so a runner standing on the mat when the race
/// starts is credited a moment before the gun. Numbering laps by that instant gave them lap 0,
/// and the runner a step behind lap 1, for the same start. A crossing whose last read came
/// before the gun was over before the race began, and that is a warm-up. A read at the gun's
/// instant is on the race side, as it is for scoring (ADR-0028). With no gun, every crossing
/// belongs to the race.
fn on_the_race_side(burst: &[&StoredRawRead], gun: Option<OffsetDateTime>) -> bool {
    gun.is_none_or(|gun| {
        burst
            .last()
            .is_some_and(|last| last.read.authoritative_timestamp() >= gun)
    })
}

/// Splits chronologically ordered reads into bursts.
///
/// A new burst starts when the gap from the **previous read** exceeds the window, not the
/// gap from the burst's first read. A chip sitting in an antenna's field produces a
/// continuous stream, and that is one crossing however long it lasts.
fn split_into_bursts<'a>(
    reads: &[&'a StoredRawRead],
    window: Duration,
) -> Vec<Vec<&'a StoredRawRead>> {
    let mut bursts = Vec::new();
    let mut current: Vec<&StoredRawRead> = Vec::new();
    let mut previous: Option<OffsetDateTime> = None;

    for stored in reads {
        let at = stored.read.authoritative_timestamp();
        // Strictly greater: reads exactly `window` apart stay in the same crossing.
        if previous.is_some_and(|prev| at - prev > window) && !current.is_empty() {
            bursts.push(std::mem::take(&mut current));
        }
        current.push(stored);
        previous = Some(at);
    }
    if !current.is_empty() {
        bursts.push(current);
    }
    bursts
}

/// Picks the read within a burst that becomes the crossing.
fn select<'a>(burst: &[&'a StoredRawRead], rule: SelectionRule) -> Option<&'a StoredRawRead> {
    match rule {
        SelectionRule::First => burst.first().copied(),
        SelectionRule::FirstAboveRssi { floor_dbm } => burst
            .iter()
            .copied()
            .find(|stored| stored.read.rssi_dbm.is_some_and(|rssi| rssi >= floor_dbm)),
        // Ties go to the earliest read: the burst is already in chronological order, so
        // reversing the index makes the earlier one win.
        SelectionRule::PeakRssi => burst
            .iter()
            .enumerate()
            .max_by_key(|(index, stored)| {
                (stored.read.rssi_dbm.unwrap_or(i16::MIN), Reverse(*index))
            })
            .map(|(_, stored)| *stored),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use splitforge_domain::{
        ChipAssignment, DeviceClockState, ManualEntryId, RaceId, RawRead, ReaderId,
        TimestampSource, TimingEventOrigin,
    };

    const READER: &str = "finish";
    const CHIP: &str = "E280A";

    fn at(millis: i64) -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH + Duration::milliseconds(1_700_000_000_000 + millis)
    }

    fn stored(seq: u64, chip: &str, millis: i64, rssi: Option<i16>) -> StoredRawRead {
        let received = at(millis);
        StoredRawRead {
            seq,
            recorded_at: received,
            payload_sha256: String::new(),
            read: RawRead {
                id: RawReadId::new(),
                source: ReaderId::new(READER),
                antenna: Some(1),
                chip: ChipId::new(chip),
                reader_timestamp: Some(received),
                reader_uptime_us: None,
                received_at: received,
                received_at_monotonic_ns: None,
                rssi_dbm: rssi,
                device_clock_state: DeviceClockState::GpsLocked,
                timestamp_source: TimestampSource::ReaderUtc,
                clock_offset_ms: Some(0),
                raw_payload: Vec::new(),
            },
        }
    }

    /// `count` reads of one chip, `spacing_ms` apart, sequenced from 1.
    fn burst_of(count: i64, chip: &str, spacing_ms: i64, rssi: Option<i16>) -> Vec<StoredRawRead> {
        (0..count)
            .map(|i| {
                stored(
                    u64::try_from(i + 1).unwrap_or(1),
                    chip,
                    i * spacing_ms,
                    rssi,
                )
            })
            .collect()
    }

    struct Fixture {
        checkpoint: CheckpointId,
        participant: ParticipantId,
        antennas: AntennaMap,
        chips: ChipRegistry,
    }

    fn fixture() -> Fixture {
        let checkpoint = CheckpointId::new();
        let participant = ParticipantId::new();
        let mut antennas = AntennaMap::new();
        antennas.map_reader(ReaderId::new(READER), checkpoint);
        let chips = ChipRegistry::from_assignments([ChipAssignment {
            chip: ChipId::new(CHIP),
            participant,
            race: RaceId::new(),
            valid_from: OffsetDateTime::UNIX_EPOCH,
            valid_until: None,
        }])
        .expect("valid assignments");
        Fixture {
            checkpoint,
            participant,
            antennas,
            chips,
        }
    }

    fn run(reads: &[StoredRawRead], policy: &TimingPolicy, fixture: &Fixture) -> Derivation {
        derive(&DerivationInput {
            reads,
            policy,
            chips: &fixture.chips,
            antennas: &fixture.antennas,
            manual: &[],
            gun_time: None,
        })
    }

    #[test]
    fn one_crossing_of_many_reads_becomes_exactly_one_accepted_read() {
        let fixture = fixture();
        // 47 reads over 1.6 s — one runner clearing a mat.
        let reads = burst_of(47, CHIP, 35, Some(-55));

        let derivation = run(&reads, &TimingPolicy::default(), &fixture);

        assert_eq!(derivation.accepted.len(), 1);
        assert_eq!(derivation.rejected.len(), 46);
        assert_eq!(derivation.timing_events.len(), 1);
        assert_eq!(derivation.accepted[0].burst_size(), 47);
        assert_eq!(
            derivation.accepted[0].at,
            at(0),
            "the `first` rule credits the earliest read"
        );
        assert_eq!(
            derivation.accepted[0].participant,
            Some(fixture.participant)
        );
        assert_eq!(
            derivation.accepted[0].checkpoint, fixture.checkpoint,
            "the crossing is credited to the checkpoint the antenna map resolved"
        );
        assert!(
            derivation
                .rejected
                .iter()
                .all(|entry| matches!(entry.reason, RejectionReason::DuplicateWithinWindow { .. })),
            "every suppressed read must say which rule suppressed it"
        );
    }

    #[test]
    fn a_genuine_second_crossing_is_not_deduplicated_away() {
        let fixture = fixture();
        let reads = vec![
            stored(1, CHIP, 0, Some(-55)),
            stored(2, CHIP, 200, Some(-55)),
            // Well beyond the 3 s window: a real second crossing.
            stored(3, CHIP, 120_000, Some(-55)),
            stored(4, CHIP, 120_150, Some(-55)),
        ];

        let derivation = run(&reads, &TimingPolicy::default(), &fixture);

        assert_eq!(derivation.accepted.len(), 2);
        assert_eq!(derivation.accepted[0].at, at(0));
        assert_eq!(derivation.accepted[1].at, at(120_000));
        assert_eq!(derivation.timing_events[0].lap, 1);
        assert_eq!(derivation.timing_events[1].lap, 2);
    }

    #[test]
    fn a_continuous_stream_longer_than_the_window_is_still_one_crossing() {
        let fixture = fixture();
        // Reads every 500 ms for 10 s: a chip left lying on the mat. The gap between
        // consecutive reads never exceeds the window, so it is one crossing.
        let reads = burst_of(20, CHIP, 500, Some(-55));

        let derivation = run(&reads, &TimingPolicy::default(), &fixture);
        assert_eq!(derivation.accepted.len(), 1);
        assert_eq!(derivation.accepted[0].burst_size(), 20);
    }

    #[test]
    fn an_unknown_chip_still_produces_a_crossing_but_no_timing_event() {
        let fixture = fixture();
        let reads = vec![stored(1, "NOT-ON-THE-ROSTER", 0, Some(-55))];

        let derivation = run(&reads, &TimingPolicy::default(), &fixture);

        assert_eq!(derivation.accepted.len(), 1, "the crossing still happened");
        assert_eq!(derivation.accepted[0].participant, None);
        assert!(
            derivation.timing_events.is_empty(),
            "nobody to credit it to yet — usually a roster error, fixable by re-deriving"
        );
    }

    #[test]
    fn a_read_from_an_unmapped_reader_is_withheld_not_credited() {
        let fixture = fixture();
        let mut read = stored(1, CHIP, 0, Some(-55));
        read.read.source = ReaderId::new("some-other-reader");

        let derivation = run(&[read], &TimingPolicy::default(), &fixture);

        assert!(derivation.accepted.is_empty());
        assert_eq!(derivation.rejected.len(), 1);
        assert!(matches!(
            derivation.rejected[0].reason,
            RejectionReason::UnmappedReader { .. }
        ));
    }

    #[test]
    fn the_rssi_floor_rule_skips_a_runner_still_approaching_the_mat() {
        let fixture = fixture();
        let policy = TimingPolicy {
            selection_rule: SelectionRule::FirstAboveRssi { floor_dbm: -60 },
            ..TimingPolicy::default()
        };
        let reads = vec![
            stored(1, CHIP, 0, Some(-80)),   // distant early pickup
            stored(2, CHIP, 100, Some(-72)), // still approaching
            stored(3, CHIP, 200, Some(-55)), // on the mat
            stored(4, CHIP, 300, Some(-50)),
        ];

        let derivation = run(&reads, &policy, &fixture);

        assert_eq!(derivation.accepted.len(), 1);
        assert_eq!(
            derivation.accepted[0].at,
            at(200),
            "credit the first read that actually cleared the floor"
        );
    }

    #[test]
    fn a_burst_entirely_below_the_floor_is_suppressed_with_a_reason() {
        let fixture = fixture();
        let policy = TimingPolicy {
            selection_rule: SelectionRule::FirstAboveRssi { floor_dbm: -60 },
            ..TimingPolicy::default()
        };
        let reads = vec![
            stored(1, CHIP, 0, Some(-90)),
            stored(2, CHIP, 100, Some(-85)),
        ];

        let derivation = run(&reads, &policy, &fixture);

        assert!(derivation.accepted.is_empty());
        assert_eq!(derivation.rejected.len(), 2);
        assert!(
            derivation
                .rejected
                .iter()
                .all(|entry| matches!(entry.reason, RejectionReason::BelowRssiFloor { .. }))
        );
    }

    #[test]
    fn the_peak_rssi_rule_credits_the_closest_approach() {
        let fixture = fixture();
        let policy = TimingPolicy {
            selection_rule: SelectionRule::PeakRssi,
            ..TimingPolicy::default()
        };
        let reads = vec![
            stored(1, CHIP, 0, Some(-80)),
            stored(2, CHIP, 100, Some(-48)),
            stored(3, CHIP, 200, Some(-60)),
        ];

        let derivation = run(&reads, &policy, &fixture);

        assert_eq!(derivation.accepted.len(), 1);
        assert_eq!(derivation.accepted[0].at, at(100));
    }

    #[test]
    fn a_lap_faster_than_the_minimum_is_treated_as_a_re_read() {
        let fixture = fixture();
        let policy = TimingPolicy {
            min_interval_ms: 1_000,
            min_lap_ms: Some(60_000),
            ..TimingPolicy::default()
        };
        let reads = vec![
            stored(1, CHIP, 0, Some(-55)),
            // 10 s later: past the dedup window, but far too fast to be a real lap.
            stored(2, CHIP, 10_000, Some(-55)),
            // 90 s: a plausible lap.
            stored(3, CHIP, 90_000, Some(-55)),
        ];

        let derivation = run(&reads, &policy, &fixture);

        assert_eq!(derivation.accepted.len(), 2);
        assert_eq!(derivation.accepted[0].at, at(0));
        assert_eq!(derivation.accepted[1].at, at(90_000));
        assert_eq!(derivation.rejected.len(), 1);
        assert!(matches!(
            derivation.rejected[0].reason,
            RejectionReason::BelowMinLap {
                min_lap_ms: 60_000,
                actual_ms: 10_000
            }
        ));
    }

    #[test]
    fn derivation_is_idempotent() {
        let fixture = fixture();
        let reads = burst_of(30, CHIP, 40, Some(-55));
        let policy = TimingPolicy::default();

        let first = run(&reads, &policy, &fixture);
        let second = run(&reads, &policy, &fixture);

        assert_eq!(
            first, second,
            "re-deriving the same evidence must be bit-identical, ids included"
        );
    }

    #[test]
    fn input_order_does_not_change_the_result() {
        let fixture = fixture();
        let mut reads = burst_of(10, CHIP, 40, Some(-55));
        let policy = TimingPolicy::default();
        let forward = run(&reads, &policy, &fixture);

        reads.reverse();
        let reversed = run(&reads, &policy, &fixture);

        assert_eq!(forward, reversed);
    }

    #[test]
    fn two_chips_at_one_checkpoint_do_not_suppress_each_other() {
        let fixture = fixture();
        let reads = vec![
            stored(1, CHIP, 0, Some(-55)),
            stored(2, "OTHER-CHIP", 50, Some(-55)),
            stored(3, CHIP, 100, Some(-55)),
        ];

        let derivation = run(&reads, &TimingPolicy::default(), &fixture);

        assert_eq!(
            derivation.accepted.len(),
            2,
            "deduplication groups by chip, so two runners crossing together both count"
        );
    }

    /// One operator's claim, for the fixture's participant at the fixture's checkpoint.
    fn manual_at(fixture: &Fixture, millis: i64) -> ManualEntry {
        ManualEntry {
            id: ManualEntryId::new(),
            race: RaceId::new(),
            participant: fixture.participant,
            checkpoint: fixture.checkpoint,
            at: at(millis),
            actor: "marshal".to_owned(),
            reason: "chip failed".to_owned(),
        }
    }

    /// Derives from both kinds of evidence at once.
    fn run_with(
        reads: &[StoredRawRead],
        manual: &[ManualEntry],
        policy: &TimingPolicy,
        fixture: &Fixture,
    ) -> Derivation {
        derive(&DerivationInput {
            reads,
            policy,
            chips: &fixture.chips,
            antennas: &fixture.antennas,
            manual,
            gun_time: None,
        })
    }

    #[test]
    fn an_entry_with_no_reads_at_all_still_times_the_runner() {
        // The case the feature exists for: the chip never reported, so there is nothing to
        // deduplicate and nothing to score — until somebody writes down what they saw.
        let fixture = fixture();
        let entry = manual_at(&fixture, 0);

        let derivation = run_with(
            &[],
            std::slice::from_ref(&entry),
            &TimingPolicy::default(),
            &fixture,
        );

        assert!(derivation.accepted.is_empty());
        assert_eq!(derivation.timing_events.len(), 1);
        let event = &derivation.timing_events[0];
        assert_eq!(event.participant, fixture.participant);
        assert_eq!(event.at, at(0));
        assert_eq!(event.lap, 1);
        assert_eq!(
            event.origin,
            TimingEventOrigin::Manual {
                manual_entry: entry.id
            }
        );
    }

    #[test]
    fn an_entry_never_suppresses_a_read_and_a_read_never_suppresses_an_entry() {
        // Two kinds of evidence, not a correction of one by the other. A chip that did
        // report and an official who also wrote it down produce two timing events, and
        // which one counts is a scoring question rather than a derivation one.
        let fixture = fixture();
        let reads = burst_of(12, CHIP, 35, Some(-55));

        let derivation = run_with(
            &reads,
            &[manual_at(&fixture, 0)],
            &TimingPolicy::default(),
            &fixture,
        );

        assert_eq!(
            derivation.accepted.len(),
            1,
            "the burst is still one crossing"
        );
        assert_eq!(
            derivation.rejected.len(),
            11,
            "and the entry did not change how the burst was deduplicated"
        );
        assert_eq!(derivation.timing_events.len(), 2);
        assert_eq!(
            derivation
                .timing_events
                .iter()
                .filter(|event| matches!(event.origin, TimingEventOrigin::Manual { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn a_hand_written_lap_keeps_the_reads_around_it_in_sequence() {
        // The interleaving claim. A runner clears the mat on lap 1, the chip dies on lap 2
        // and a marshal writes it down, and the chip recovers for lap 3. Counting the two
        // sources separately would hand the recovered read lap 2 and put the runner a whole
        // lap behind for the rest of the race.
        let fixture = fixture();
        let policy = TimingPolicy::default();
        // Well clear of the dedup window, so each burst is unambiguously its own crossing.
        let lap_ms = 10_000_i64;

        let mut reads = burst_of(3, CHIP, 35, Some(-55));
        reads.extend((0..3).map(|i| {
            stored(
                u64::try_from(i + 4).unwrap_or(4),
                CHIP,
                2 * lap_ms + i * 35,
                Some(-55),
            )
        }));

        let derivation = run_with(&reads, &[manual_at(&fixture, lap_ms)], &policy, &fixture);

        assert_eq!(
            derivation.accepted.len(),
            2,
            "the two bursts are two crossings before laps are counted"
        );

        let laps: Vec<u16> = derivation.timing_events.iter().map(|e| e.lap).collect();
        assert_eq!(laps, vec![1, 2, 3], "got: {:?}", derivation.timing_events);

        let origins: Vec<bool> = derivation
            .timing_events
            .iter()
            .map(|event| matches!(event.origin, TimingEventOrigin::Manual { .. }))
            .collect();
        assert_eq!(
            origins,
            vec![false, true, false],
            "the hand-written lap must sit between the two the chip recorded"
        );
    }

    #[test]
    fn deriving_twice_from_the_same_entry_produces_the_same_identifiers() {
        // The property that makes an entry evidence rather than an edit: re-deriving after
        // a restore has to reproduce the timing event byte for byte, not merely an
        // equivalent one.
        let fixture = fixture();
        let reads = burst_of(6, CHIP, 35, Some(-55));
        let manual = vec![manual_at(&fixture, 500), manual_at(&fixture, 900)];

        let first = run_with(&reads, &manual, &TimingPolicy::default(), &fixture);
        let second = run_with(&reads, &manual, &TimingPolicy::default(), &fixture);

        assert_eq!(first.timing_events, second.timing_events);
    }

    #[test]
    fn the_order_entries_are_supplied_in_does_not_change_the_result() {
        // Storage returns entries in insertion order, and an operator can record a 9 a.m.
        // crossing at 11 a.m. Derivation sorts by the claimed time, so the order the rows
        // happen to arrive in cannot reach the output.
        let fixture = fixture();
        let early = manual_at(&fixture, 100);
        let late = manual_at(&fixture, 8_000);

        let forwards = run_with(
            &[],
            &[early.clone(), late.clone()],
            &TimingPolicy::default(),
            &fixture,
        );
        let backwards = run_with(&[], &[late, early], &TimingPolicy::default(), &fixture);

        assert_eq!(forwards.timing_events, backwards.timing_events);
        assert_eq!(forwards.timing_events[0].at, at(100));
        assert_eq!(forwards.timing_events[0].lap, 1);
        assert_eq!(forwards.timing_events[1].lap, 2);
    }

    #[test]
    fn two_officials_recording_the_same_runner_produce_two_events() {
        // Deliberate. Two people writing down bib 104 at 08:17:32 saw one thing and produced
        // two records of it, and collapsing them would throw away the corroboration that
        // makes a disputed time defensible.
        let fixture = fixture();
        let first = manual_at(&fixture, 4_000);
        let second = ManualEntry {
            id: ManualEntryId::new(),
            actor: "second marshal".to_owned(),
            ..first.clone()
        };

        let derivation = run_with(&[], &[first, second], &TimingPolicy::default(), &fixture);

        assert_eq!(derivation.timing_events.len(), 2);
        assert_ne!(
            derivation.timing_events[0].id, derivation.timing_events[1].id,
            "identical claims by two people must not collapse into one event"
        );
    }

    // ADR-0029: laps count from the gun, and the minimum lap is not measured across it.

    /// The gun in these tests: one minute after the first read.
    const GUN_MS: i64 = 60_000;

    /// Derives with the gun at [`GUN_MS`].
    fn run_from_gun(
        reads: &[StoredRawRead],
        manual: &[ManualEntry],
        policy: &TimingPolicy,
        fixture: &Fixture,
    ) -> Derivation {
        derive(&DerivationInput {
            reads,
            policy,
            chips: &fixture.chips,
            antennas: &fixture.antennas,
            manual,
            gun_time: Some(at(GUN_MS)),
        })
    }

    fn laps(derivation: &Derivation) -> Vec<u16> {
        derivation
            .timing_events
            .iter()
            .map(|event| event.lap)
            .collect()
    }

    /// A lap line crossed in a warm-up, then at the start, then after one lap.
    fn warm_up_then_two_crossings() -> Vec<StoredRawRead> {
        vec![
            stored(1, CHIP, 0, Some(-55)),
            stored(2, CHIP, GUN_MS + 5_000, Some(-55)),
            stored(3, CHIP, GUN_MS + 185_000, Some(-55)),
        ]
    }

    #[test]
    fn laps_count_from_the_gun_and_a_warm_up_is_lap_0() {
        // Counted from each runner's first crossing, the warm-up was lap 1 and the first real
        // lap was recorded as lap 2.
        let fixture = fixture();
        let derivation = run_from_gun(
            &warm_up_then_two_crossings(),
            &[],
            &TimingPolicy::default(),
            &fixture,
        );

        assert_eq!(laps(&derivation), vec![0, 1, 2], "{derivation:?}");
    }

    #[test]
    fn the_gun_filters_nothing() {
        // The warm-up is still a crossing and still a timing event. It is only numbered.
        let fixture = fixture();
        let derivation = run_from_gun(
            &warm_up_then_two_crossings(),
            &[],
            &TimingPolicy::default(),
            &fixture,
        );

        assert_eq!(derivation.accepted.len(), 3);
        assert_eq!(derivation.timing_events.len(), 3);
        assert_eq!(derivation.timing_events[0].at, at(0));
    }

    #[test]
    fn a_warm_up_cannot_make_the_start_crossing_a_re_read() {
        // A criterium whose start line is its lap line, with a two-minute minimum lap. The
        // warm-up 60 s before the gun made the start crossing 5 s after it look like a re-read
        // 65 s later, and the runner's start was rejected.
        let fixture = fixture();
        let policy = TimingPolicy {
            min_interval_ms: 1_000,
            min_lap_ms: Some(120_000),
            ..TimingPolicy::default()
        };
        let reads = vec![
            stored(1, CHIP, 0, Some(-55)),
            stored(2, CHIP, GUN_MS + 5_000, Some(-55)),
        ];

        let derivation = run_from_gun(&reads, &[], &policy, &fixture);

        assert!(
            derivation.rejected.is_empty(),
            "the start crossing was rejected: {:?}",
            derivation.rejected
        );
        assert_eq!(laps(&derivation), vec![0, 1]);
    }

    #[test]
    fn the_minimum_lap_still_applies_on_each_side_of_the_gun() {
        let fixture = fixture();
        let policy = TimingPolicy {
            min_interval_ms: 1_000,
            min_lap_ms: Some(30_000),
            ..TimingPolicy::default()
        };
        let reads = vec![
            // Two warm-up passes 10 s apart: the second is a re-read.
            stored(1, CHIP, 0, Some(-55)),
            stored(2, CHIP, 10_000, Some(-55)),
            // Two race passes 10 s apart: the second is a re-read.
            stored(3, CHIP, GUN_MS + 5_000, Some(-55)),
            stored(4, CHIP, GUN_MS + 15_000, Some(-55)),
        ];

        let derivation = run_from_gun(&reads, &[], &policy, &fixture);

        assert_eq!(derivation.accepted.len(), 2, "{derivation:?}");
        assert_eq!(derivation.rejected.len(), 2);
        assert!(
            derivation
                .rejected
                .iter()
                .all(|rejected| matches!(rejected.reason, RejectionReason::BelowMinLap { .. }))
        );
        assert_eq!(laps(&derivation), vec![0, 1]);
    }

    #[test]
    fn a_crossing_at_the_instant_of_the_gun_is_lap_1() {
        // Consistent with ADR-0028, where a crossing at the gun's instant counts.
        let fixture = fixture();
        let reads = vec![
            stored(1, CHIP, 0, Some(-55)),
            stored(2, CHIP, GUN_MS, Some(-55)),
        ];

        let derivation = run_from_gun(&reads, &[], &TimingPolicy::default(), &fixture);

        assert_eq!(laps(&derivation), vec![0, 1]);
    }

    /// One chip's reads from `from_ms` to `to_ms`, 100 ms apart: a burst, as a mat sees it.
    fn reads_from(first_seq: u64, from_ms: i64, to_ms: i64) -> Vec<StoredRawRead> {
        (from_ms..=to_ms)
            .step_by(100)
            .zip(first_seq..)
            .map(|(millis, seq)| stored(seq, CHIP, millis, Some(-55)))
            .collect()
    }

    fn one_second_window() -> TimingPolicy {
        TimingPolicy {
            min_interval_ms: 1_000,
            ..TimingPolicy::default()
        }
    }

    #[test]
    fn a_runner_on_the_mat_when_the_gun_goes_is_on_lap_1() {
        // The case the promoter's rule is built on, and the one the credited instant gets
        // wrong. The burst's first read is credited 300 ms before the gun, and its reads are
        // still arriving when the gun goes. Numbered by the credited instant this was lap 0,
        // while a runner a step behind was lap 1 for the same start: the four-lap criterium
        // fixture's first rider crosses at exactly the gun and came out one lap behind the
        // other five.
        let fixture = fixture();
        let mut reads = vec![stored(1, CHIP, 0, Some(-55))];
        reads.extend(reads_from(2, GUN_MS - 300, GUN_MS + 200));
        reads.push(stored(20, CHIP, GUN_MS + 270_000, Some(-55)));

        let derivation = run_from_gun(&reads, &[], &one_second_window(), &fixture);

        assert_eq!(derivation.accepted.len(), 3, "{derivation:?}");
        assert_eq!(
            derivation.timing_events[1].at,
            at(GUN_MS - 300),
            "the credited instant is still the first read"
        );
        assert_eq!(laps(&derivation), vec![0, 1, 2]);
    }

    #[test]
    fn a_crossing_over_before_the_gun_is_a_warm_up() {
        // The other side of the same line: the last read came 100 ms before the gun.
        let fixture = fixture();
        let mut reads = reads_from(1, GUN_MS - 500, GUN_MS - 100);
        reads.push(stored(20, CHIP, GUN_MS + 5_000, Some(-55)));

        let derivation = run_from_gun(&reads, &[], &one_second_window(), &fixture);

        assert_eq!(laps(&derivation), vec![0, 1], "{derivation:?}");
    }

    #[test]
    fn a_warm_up_cannot_make_a_start_on_the_mat_a_re_read() {
        let fixture = fixture();
        let policy = TimingPolicy {
            min_lap_ms: Some(120_000),
            ..one_second_window()
        };
        let mut reads = vec![stored(1, CHIP, 0, Some(-55))];
        reads.extend(reads_from(2, GUN_MS - 300, GUN_MS + 200));

        let derivation = run_from_gun(&reads, &[], &policy, &fixture);

        assert!(
            !derivation
                .rejected
                .iter()
                .any(|rejected| matches!(rejected.reason, RejectionReason::BelowMinLap { .. })),
            "the start on the mat was rejected as a re-read: {:?}",
            derivation.rejected
        );
        assert_eq!(laps(&derivation), vec![0, 1]);
    }

    #[test]
    fn a_manual_entry_follows_the_same_rule() {
        let fixture = fixture();
        let before = manual_at(&fixture, 0);
        let reads = vec![stored(1, CHIP, GUN_MS + 5_000, Some(-55))];

        let derivation = run_from_gun(
            &reads,
            std::slice::from_ref(&before),
            &TimingPolicy::default(),
            &fixture,
        );

        assert_eq!(laps(&derivation), vec![0, 1]);
        assert_eq!(
            derivation.timing_events[0].origin,
            TimingEventOrigin::Manual {
                manual_entry: before.id
            }
        );
    }

    #[test]
    fn with_no_gun_laps_count_from_the_first_crossing() {
        let fixture = fixture();
        let derivation = run(
            &warm_up_then_two_crossings(),
            &TimingPolicy::default(),
            &fixture,
        );

        assert_eq!(laps(&derivation), vec![1, 2, 3]);
    }
}
