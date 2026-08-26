//! Milestone 1, stated as assertions.
//!
//! > A simulated event produces durable raw and accepted reads, entirely offline, and
//! > duplicate reads are preserved raw while reducing to one accepted timing event.
//!
//! The restart half of that criterion is in `restart.rs`.

mod common;

use std::collections::BTreeMap;

use splitforge_domain::{ChipId, RejectionReason, TimingEventOrigin};
use splitforge_testkit::{CrossingExpectation, five_k, multi_lap};

/// Every read is either credited or suppressed with a stated reason. Nothing evaporates.
///
/// This is the property that makes the journal auditable at all: a read that is neither
/// accepted nor rejected is a read nobody can account for.
#[tokio::test]
async fn every_read_is_accounted_for() {
    for fixture in [five_k(), multi_lap()] {
        let slug = fixture.slug;
        let slice = common::run(fixture).await;
        assert_eq!(
            slice.derivation.accepted.len() + slice.derivation.rejected.len(),
            slice.reads.len(),
            "{slug}: reads went missing between the journal and the derivation"
        );
    }
}

#[tokio::test]
async fn duplicate_reads_are_preserved_raw_and_reduced_to_one_crossing() {
    let slice = common::run(five_k()).await;

    // The raw journal holds the whole burst...
    assert!(
        slice.reads.len() > 400,
        "a 24-crossing event should produce hundreds of raw reads, got {}",
        slice.reads.len()
    );
    // ...and derivation reduces it to one crossing each.
    assert_eq!(slice.derivation.accepted.len(), 24);

    // Every suppressed read at the start mat says it was a duplicate, not that it vanished.
    let duplicates = slice
        .derivation
        .rejected
        .iter()
        .filter(|entry| matches!(entry.reason, RejectionReason::DuplicateWithinWindow { .. }))
        .count();
    assert_eq!(
        duplicates,
        slice.reads.len() - slice.derivation.accepted.len(),
        "the 5K has no unmapped readers and no lap rule, so every non-credited read is a \
         duplicate"
    );

    // And each duplicate points at the crossing it belongs to.
    let crossings: Vec<_> = slice.derivation.accepted.iter().map(|c| c.id).collect();
    for entry in &slice.derivation.rejected {
        if let RejectionReason::DuplicateWithinWindow { accepted, .. } = entry.reason {
            assert!(
                crossings.contains(&accepted),
                "a duplicate referenced a crossing that does not exist"
            );
        }
    }
}

#[tokio::test]
async fn the_five_k_produces_exactly_the_crossings_the_fixture_planned() {
    let slice = common::run(five_k()).await;

    for planned in &slice.fixture.crossings {
        slice.assert_matches(planned);
    }
    assert_eq!(
        slice.derivation.accepted.len(),
        slice.fixture.expected_accepted()
    );
    assert_eq!(
        slice.derivation.timing_events.len(),
        slice.fixture.expected_timing_events()
    );
}

#[tokio::test]
async fn a_dnf_has_a_start_and_no_finish() {
    let slice = common::run(five_k()).await;
    let chip = ChipId::new("E28011606000020000000109");
    let start = slice.fixture.checkpoint("start").expect("start");
    let finish = slice.fixture.checkpoint("finish").expect("finish");

    assert_eq!(slice.crossings_of(&chip, start).len(), 1);
    assert!(
        slice.crossings_of(&chip, finish).is_empty(),
        "bib 109 never crossed the finish mat, so nothing may invent a finish for them"
    );
}

#[tokio::test]
async fn an_unrostered_chip_is_recorded_and_flagged_rather_than_discarded() {
    let slice = common::run(five_k()).await;
    let chip = ChipId::new("E2801160600002FFFFFFFFFF");
    let finish = slice.fixture.checkpoint("finish").expect("finish");

    let crossings = slice.crossings_of(&chip, finish);
    assert_eq!(crossings.len(), 1, "the crossing really happened");
    assert_eq!(
        crossings[0].participant, None,
        "and it must not be credited to whoever happens to be nearby in the roster"
    );

    // It produces no timing event, because there is nobody to time.
    let credited: Vec<_> = slice
        .derivation
        .timing_events
        .iter()
        .filter(|event| {
            matches!(
                event.origin,
                TimingEventOrigin::AcceptedRead { accepted_read } if accepted_read == crossings[0].id
            )
        })
        .collect();
    assert!(credited.is_empty());

    // The evidence is still in the journal in full.
    let raw = slice
        .reads
        .iter()
        .filter(|stored| stored.read.chip == chip)
        .count();
    assert!(raw > 1, "the burst behind the flagged crossing is retained");
}

#[tokio::test]
async fn the_finish_mat_credits_a_read_taken_at_the_line_not_one_taken_approaching_it() {
    let slice = common::run(five_k()).await;
    let finish = slice.fixture.checkpoint("finish").expect("finish");
    let floor_dbm = -62;

    for crossing in &slice.derivation.accepted {
        if crossing.checkpoint == finish.id {
            let rssi = crossing.rssi_dbm.expect("the simulator reports rssi");
            assert!(
                rssi >= floor_dbm,
                "the finish checkpoint's RSSI floor was not applied: credited {rssi} dBm"
            );
        }
    }
}

#[tokio::test]
async fn the_criterium_counts_five_mat_crossings_per_rider_in_order() {
    let slice = common::run(multi_lap()).await;
    let lap = slice.fixture.checkpoint("lap").expect("lap");

    for planned in &slice.fixture.crossings {
        slice.assert_matches(planned);
    }

    let mut laps_by_participant: BTreeMap<_, Vec<u16>> = BTreeMap::new();
    for event in &slice.derivation.timing_events {
        assert_eq!(event.checkpoint, lap.id);
        laps_by_participant
            .entry(event.participant)
            .or_default()
            .push(event.lap);
    }

    assert_eq!(laps_by_participant.len(), 6, "six riders started");
    for (participant, laps) in &laps_by_participant {
        assert_eq!(
            laps,
            &vec![1, 2, 3, 4, 5],
            "{participant} must be credited one crossing off the line plus four laps, \
             numbered consecutively"
        );
    }
}

#[tokio::test]
async fn a_rider_loitering_at_the_mat_is_not_credited_with_a_45_second_lap() {
    let slice = common::run(multi_lap()).await;

    let stray = slice
        .fixture
        .crossings
        .iter()
        .find(|crossing| crossing.expectation == CrossingExpectation::SuppressedAsReRead)
        .expect("the criterium fixture contains one re-read");

    slice.assert_matches(stray);

    let below_min_lap = slice
        .derivation
        .rejected
        .iter()
        .filter(|entry| matches!(entry.reason, RejectionReason::BelowMinLap { .. }))
        .count();
    assert!(
        below_min_lap > 0,
        "the minimum-lap rule never fired, so this fixture is not testing what it claims"
    );

    // The suppressed reads are still in the journal: the rider was genuinely there.
    let raw_near = slice
        .reads
        .iter()
        .filter(|stored| {
            stored.read.chip == stray.chip
                && (stored.read.authoritative_timestamp() - stray.at).abs() <= common::TOLERANCE
        })
        .count();
    assert_eq!(
        raw_near, below_min_lap,
        "every read of the loitering rider is preserved raw and individually explained"
    );
}

#[tokio::test]
async fn reader_timestamps_are_authoritative_when_the_reader_clock_is_healthy() {
    // The simulated LAN adds a few milliseconds of latency, well inside the trust
    // threshold, so a correctly configured deployment must be using reader time. If this
    // ever flips to device time, every recorded crossing has silently acquired the
    // pipeline's scheduling jitter.
    let slice = common::run(five_k()).await;

    assert!(
        slice
            .reads
            .iter()
            .all(|stored| stored.read.is_reader_timed()),
        "the pipeline fell back to device receipt time for at least one read"
    );
    for stored in &slice.reads {
        let offset = stored.read.clock_offset_ms.expect("offset is measured");
        assert!(
            offset.abs() < 1_000,
            "a simulated LAN produced an implausible reader offset of {offset} ms"
        );
    }
}

#[tokio::test]
async fn the_same_seed_replays_the_same_race() {
    let first = common::run(five_k()).await;
    let second = common::run(five_k()).await;

    assert_eq!(first.reads.len(), second.reads.len());

    let shape = |slice: &common::Slice| -> Vec<_> {
        slice
            .derivation
            .accepted
            .iter()
            .map(|crossing| {
                (
                    crossing.chip.clone(),
                    crossing.at,
                    crossing.burst_size(),
                    crossing.rssi_dbm,
                )
            })
            .collect()
    };
    assert_eq!(shape(&first), shape(&second));

    // The identifiers, however, must *not* match — and that is the point rather than a
    // shortcoming. Raw read ids are minted once per ingest, and a crossing's id is derived
    // from the read it credits. Two runs are two different pieces of evidence describing
    // two different races that happen to look alike. Only re-deriving *the same journal*
    // reproduces ids; see `restart.rs`.
    assert_ne!(first.reads[0].read.id, second.reads[0].read.id);
    assert_ne!(
        first.derivation.accepted[0].id,
        second.derivation.accepted[0].id
    );
}
