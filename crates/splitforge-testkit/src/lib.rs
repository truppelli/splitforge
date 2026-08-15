//! # splitforge-testkit
//!
//! Fixtures, test doubles, and race scenario builders.
//!
//! ## Boundaries
//!
//! - **May depend on:** splitforge-domain, splitforge-storage, splitforge-reader
//! - **Must never depend on:** nothing specific — test-only crate
//!
//! These rules come from ADR-0001 and are tabulated in `docs/architecture.md`.
//! They are enforced by `crates/splitforge-testkit/tests/dependency_rules.rs`, not left to
//! review (ADR-0012).
//!
//! ## What a fixture is
//!
//! A [`FixtureEvent`] is a complete, self-consistent event: race, checkpoints, roster, chip
//! assignments, antenna map, timing policy, and — crucially — a list of
//! [`PlannedCrossing`]s that states **what should happen**. The fixture is the oracle. A
//! test asserts that the engine agrees with it, rather than restating the engine's own
//! arithmetic back to itself.
//!
//! ## Why fixture identifiers are deterministic
//!
//! Derived identifiers are a function of the checkpoint id that produced them (see
//! `splitforge_domain::AcceptedRead`). If a fixture minted random ids, the same fixture
//! built in two processes would derive two different sets of accepted-read ids, and
//! "re-derive after a restart and compare" could not be written as an assertion at all.
//!
//! So every fixture id is a UUIDv5 of the fixture's own name. Same fixture, same ids,
//! forever, on any machine.

use splitforge_domain::{
    AntennaMap, Bib, Checkpoint, CheckpointId, CheckpointKind, CheckpointPolicy, ChipAssignment,
    ChipId, ChipRegistry, DomainError, Event, EventId, Participant, ParticipantId, Race, RaceId,
    ReaderId, SPLITFORGE_NAMESPACE, SelectionRule, TimingPolicy,
};
use time::macros::datetime;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

/// The gun time shared by every fixture race.
///
/// A fixed date rather than "now": a fixture that moves with the wall clock cannot be
/// compared against a stored expectation.
pub const RACE_START: OffsetDateTime = datetime!(2026-04-11 08:00:00 UTC);

/// Builds a deterministic fixture identifier.
///
/// Parts are joined with an ASCII unit separator for the same reason derived ids use one:
/// distinct inputs must not collide by concatenation.
fn fixture_id(kind: &str, name: &str) -> Uuid {
    let mut key = String::with_capacity(48);
    key.push_str("splitforge-fixture");
    key.push('\u{1f}');
    key.push_str(kind);
    key.push('\u{1f}');
    key.push_str(name);
    Uuid::new_v5(&SPLITFORGE_NAMESPACE, key.as_bytes())
}

/// An EPC-96 style chip identifier, 24 hex characters, derived from a bib number.
fn chip_for(bib: u32) -> ChipId {
    ChipId::new(format!("E2801160600002{bib:010}"))
}

/// What the fixture says must happen to a crossing.
///
/// This is the fixture's contract with the engine. A test that only counts rows can pass
/// while crediting the wrong runner; a test that checks expectations cannot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrossingExpectation {
    /// Becomes exactly one accepted read, credited to a participant.
    Accepted,
    /// Becomes exactly one accepted read with no participant.
    ///
    /// The chip is not on the roster — a marshal's test tag, or a runner wearing last
    /// year's chip. The crossing still happened, so it is recorded and flagged rather than
    /// discarded.
    AcceptedUnassigned,
    /// Becomes no accepted read: the minimum-lap rule treats it as a re-read.
    SuppressedAsReRead,
}

/// One chip passing one antenna at one instant, and what should come of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedCrossing {
    /// The chip that crosses.
    pub chip: ChipId,
    /// The reader whose field it passes through.
    pub reader: ReaderId,
    /// The antenna, when the deployment distinguishes them.
    pub antenna: Option<u16>,
    /// The checkpoint that antenna covers.
    pub checkpoint: CheckpointId,
    /// The true instant of the crossing, before any reader or transport error.
    pub at: OffsetDateTime,
    /// What the engine must conclude.
    pub expectation: CrossingExpectation,
}

/// A complete event, its roster, and the crossings that are supposed to happen.
#[derive(Debug, Clone)]
pub struct FixtureEvent {
    /// Short stable name, used by the CLI to select a fixture.
    pub slug: &'static str,
    /// The race day.
    pub event: Event,
    /// The race.
    pub race: Race,
    /// Checkpoints on the course, in sequence order.
    pub checkpoints: Vec<Checkpoint>,
    /// The roster.
    pub participants: Vec<Participant>,
    /// Time-bounded chip assignments.
    pub assignments: Vec<ChipAssignment>,
    /// Reader and antenna to checkpoint mapping.
    pub antennas: AntennaMap,
    /// The rules the engine should be run with.
    pub policy: TimingPolicy,
    /// Every crossing that happens during the event, in chronological order.
    pub crossings: Vec<PlannedCrossing>,
}

impl FixtureEvent {
    /// Selects a fixture by its slug.
    #[must_use]
    pub fn by_slug(slug: &str) -> Option<Self> {
        match slug {
            "five-k" => Some(five_k()),
            "multi-lap" => Some(multi_lap()),
            _ => None,
        }
    }

    /// Every fixture this crate ships.
    #[must_use]
    pub fn all() -> Vec<Self> {
        vec![five_k(), multi_lap()]
    }

    /// Builds the chip registry for this fixture.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError`] if the fixture's own assignments overlap, which would mean
    /// the fixture is malformed.
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

    /// Looks up a participant by bib.
    #[must_use]
    pub fn participant(&self, bib: &str) -> Option<&Participant> {
        self.participants
            .iter()
            .find(|participant| participant.bib.as_str() == bib)
    }

    /// How many crossings must become accepted reads, credited or not.
    #[must_use]
    pub fn expected_accepted(&self) -> usize {
        self.crossings
            .iter()
            .filter(|crossing| {
                matches!(
                    crossing.expectation,
                    CrossingExpectation::Accepted | CrossingExpectation::AcceptedUnassigned
                )
            })
            .count()
    }

    /// How many crossings must become timing events — that is, accepted *and* credited.
    #[must_use]
    pub fn expected_timing_events(&self) -> usize {
        self.crossings
            .iter()
            .filter(|crossing| crossing.expectation == CrossingExpectation::Accepted)
            .count()
    }
}

/// A 5K with a start mat and a finish mat on one two-antenna reader.
///
/// Twelve entrants. One (bib 109) starts and never finishes — a DNF, which exists here so
/// that "every starter has a finish" is never accidentally true. One unrostered chip
/// crosses the finish mat, which must produce a flagged crossing rather than silence.
#[must_use]
pub fn five_k() -> FixtureEvent {
    const SLUG: &str = "five-k";
    /// Finishing order, deliberately not bib order: a test must not be able to pass by
    /// assuming the field finishes in the order it registered.
    const FINISH_ORDER: [u32; 11] = [104, 101, 107, 102, 111, 105, 108, 103, 112, 106, 110];
    const DNF_BIB: u32 = 109;

    let event = Event {
        id: EventId::from_uuid(fixture_id("event", SLUG)),
        name: "SplitForge Fixture Day".to_owned(),
    };
    let race = Race {
        id: RaceId::from_uuid(fixture_id("race", SLUG)),
        event: event.id,
        name: "5K".to_owned(),
        scheduled_start: Some(RACE_START),
        expected_laps: 1,
    };

    let start = Checkpoint {
        id: CheckpointId::from_uuid(fixture_id("checkpoint", "five-k/start")),
        race: race.id,
        name: "start".to_owned(),
        kind: CheckpointKind::Start,
        sequence: 0,
    };
    let finish = Checkpoint {
        id: CheckpointId::from_uuid(fixture_id("checkpoint", "five-k/finish")),
        race: race.id,
        name: "finish".to_owned(),
        kind: CheckpointKind::Finish,
        sequence: 1,
    };

    let reader = ReaderId::new("mat");
    let mut antennas = AntennaMap::new();
    antennas.map_antenna(reader.clone(), 1, start.id);
    antennas.map_antenna(reader.clone(), 2, finish.id);

    // The finish mat reaches past the line, so the earliest detection is a runner still
    // approaching. An RSSI floor credits the first read taken at the mat instead. The
    // start mat keeps the plain `First` rule: a start time a fraction early is harmless,
    // and losing a start entirely is not.
    let policy = TimingPolicy::default().with_checkpoint(
        finish.id,
        CheckpointPolicy {
            min_interval_ms: None,
            selection_rule: Some(SelectionRule::FirstAboveRssi { floor_dbm: -62 }),
        },
    );

    let mut participants = Vec::new();
    let mut assignments = Vec::new();
    for bib in 101_u32..=112 {
        let participant = ParticipantId::from_uuid(fixture_id("participant", &format!("5k/{bib}")));
        participants.push(Participant {
            id: participant,
            race: race.id,
            bib: Bib::new(bib.to_string()),
            name: format!("Runner {bib}"),
        });
        assignments.push(ChipAssignment {
            chip: chip_for(bib),
            participant,
            race: race.id,
            // Chips are handed out at packet pickup the evening before and are open-ended
            // for the day; the window still has to exist, because resolution is a function
            // of read time.
            valid_from: RACE_START - Duration::hours(14),
            valid_until: None,
        });
    }

    let mut crossings = Vec::new();
    for (index, bib) in (101_u32..=112).enumerate() {
        let offset = i64::try_from(index).unwrap_or(0);
        crossings.push(PlannedCrossing {
            chip: chip_for(bib),
            reader: reader.clone(),
            antenna: Some(1),
            checkpoint: start.id,
            // A small field clears the mat over about ten seconds.
            at: RACE_START + Duration::milliseconds(200 + offset * 900),
            expectation: CrossingExpectation::Accepted,
        });
    }
    for (index, bib) in FINISH_ORDER.into_iter().enumerate() {
        debug_assert_ne!(bib, DNF_BIB, "the DNF must not appear in the finish order");
        let place = i64::try_from(index).unwrap_or(0);
        // 17:32.4 for the winner, widening to just over 29 minutes at the back.
        let elapsed_ms = 1_052_400 + place * 38_500 + place * place * 3_100;
        crossings.push(PlannedCrossing {
            chip: chip_for(bib),
            reader: reader.clone(),
            antenna: Some(2),
            checkpoint: finish.id,
            at: RACE_START + Duration::milliseconds(elapsed_ms),
            expectation: CrossingExpectation::Accepted,
        });
    }
    // A chip nobody on this roster is wearing. The crossing is real evidence and must be
    // recorded and surfaced, not silently dropped for being inconvenient.
    crossings.push(PlannedCrossing {
        chip: ChipId::new("E2801160600002FFFFFFFFFF"),
        reader: reader.clone(),
        antenna: Some(2),
        checkpoint: finish.id,
        at: RACE_START + Duration::minutes(25),
        expectation: CrossingExpectation::AcceptedUnassigned,
    });
    crossings.sort_by_key(|crossing| crossing.at);

    FixtureEvent {
        slug: SLUG,
        event,
        race,
        checkpoints: vec![start, finish],
        participants,
        assignments,
        antennas,
        policy,
        crossings,
    }
}

/// A four-lap criterium over a single lap mat.
///
/// The mat is crossed five times per rider: once leaving the start, then once per lap.
/// One rider stops beside the mat after lap two and is read again 45 seconds later, which
/// the minimum-lap rule must reject rather than credit as a 45-second lap.
#[must_use]
pub fn multi_lap() -> FixtureEvent {
    const SLUG: &str = "multi-lap";
    const RIDERS: u32 = 6;
    const LAPS: i64 = 4;
    /// Two minutes. Half the fastest plausible lap on this course, so a genuine lap is
    /// never at risk while a re-read at the mat cannot survive.
    const MIN_LAP_MS: u64 = 120_000;
    /// The rider who loiters at the mat, as a zero-based index into the field.
    const LOITERER: u32 = 2;

    let event = Event {
        id: EventId::from_uuid(fixture_id("event", SLUG)),
        name: "SplitForge Fixture Day".to_owned(),
    };
    let race = Race {
        id: RaceId::from_uuid(fixture_id("race", SLUG)),
        event: event.id,
        name: "Criterium".to_owned(),
        scheduled_start: Some(RACE_START),
        expected_laps: 4,
    };
    let lap = Checkpoint {
        id: CheckpointId::from_uuid(fixture_id("checkpoint", "multi-lap/lap")),
        race: race.id,
        name: "lap".to_owned(),
        kind: CheckpointKind::Lap,
        sequence: 0,
    };

    // A single-antenna deployment: one wildcard entry rather than one line per port.
    let reader = ReaderId::new("lap-mat");
    let mut antennas = AntennaMap::new();
    antennas.map_reader(reader.clone(), lap.id);

    let policy = TimingPolicy {
        min_lap_ms: Some(MIN_LAP_MS),
        ..TimingPolicy::default()
    };

    let mut participants = Vec::new();
    let mut assignments = Vec::new();
    for bib in 1..=RIDERS {
        let participant =
            ParticipantId::from_uuid(fixture_id("participant", &format!("crit/{bib}")));
        participants.push(Participant {
            id: participant,
            race: race.id,
            bib: Bib::new(bib.to_string()),
            name: format!("Rider {bib}"),
        });
        assignments.push(ChipAssignment {
            chip: chip_for(bib),
            participant,
            race: race.id,
            valid_from: RACE_START - Duration::hours(2),
            valid_until: None,
        });
    }

    let mut crossings = Vec::new();
    for rider in 0..RIDERS {
        let bib = rider + 1;
        let stagger = i64::from(rider) * 2_000;
        for completed in 0..=LAPS {
            // Laps lengthen slightly as the race goes on, and riders are not identical.
            let elapsed = completed * (270_000 + i64::from(rider) * 4_100 + completed * 2_300);
            crossings.push(PlannedCrossing {
                chip: chip_for(bib),
                reader: reader.clone(),
                antenna: Some(1),
                checkpoint: lap.id,
                at: RACE_START + Duration::milliseconds(stagger + elapsed),
                expectation: CrossingExpectation::Accepted,
            });

            if rider == LOITERER && completed == 2 {
                let loiter = RACE_START + Duration::milliseconds(stagger + elapsed + 45_000);
                crossings.push(PlannedCrossing {
                    chip: chip_for(bib),
                    reader: reader.clone(),
                    antenna: Some(1),
                    checkpoint: lap.id,
                    at: loiter,
                    expectation: CrossingExpectation::SuppressedAsReRead,
                });
            }
        }
    }
    crossings.sort_by_key(|crossing| crossing.at);

    FixtureEvent {
        slug: SLUG,
        event,
        race,
        checkpoints: vec![lap],
        participants,
        assignments,
        antennas,
        policy,
        crossings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_identifiers_are_stable_across_builds() {
        // If this ever fails, every derived id in every stored fixture expectation has
        // moved, and restart comparisons are silently meaningless.
        assert_eq!(five_k().race.id, five_k().race.id);
        assert_eq!(
            five_k()
                .checkpoint("finish")
                .expect("finish")
                .id
                .to_string(),
            "79d32a3e-7ad4-5a30-ac85-82ef7ca7370f"
        );
    }

    #[test]
    fn every_fixture_has_a_consistent_roster_and_antenna_map() {
        for fixture in FixtureEvent::all() {
            let chips = fixture.chips().expect("assignments must not overlap");
            assert!(!fixture.antennas.is_empty(), "{}", fixture.slug);
            assert_eq!(chips.len(), fixture.participants.len(), "{}", fixture.slug);

            for crossing in &fixture.crossings {
                assert_eq!(
                    fixture
                        .antennas
                        .resolve(&crossing.reader, crossing.antenna)
                        .as_ref(),
                    Some(&crossing.checkpoint),
                    "{}: every planned crossing must map to the checkpoint it claims",
                    fixture.slug
                );
                let resolved = chips.participant_at(&crossing.chip, crossing.at).is_some();
                let should_resolve =
                    crossing.expectation != CrossingExpectation::AcceptedUnassigned;
                assert_eq!(resolved, should_resolve, "{}", fixture.slug);
            }
        }
    }

    #[test]
    fn crossings_are_chronological() {
        for fixture in FixtureEvent::all() {
            assert!(
                fixture
                    .crossings
                    .windows(2)
                    .all(|pair| pair[0].at <= pair[1].at),
                "{}",
                fixture.slug
            );
        }
    }

    #[test]
    fn the_five_k_has_a_dnf_and_an_unrostered_chip() {
        let fixture = five_k();
        let finish = fixture.checkpoint("finish").expect("finish").id;
        let dnf_chip = chip_for(109);

        assert_eq!(fixture.participants.len(), 12);
        assert_eq!(
            fixture
                .crossings
                .iter()
                .filter(|crossing| crossing.checkpoint == finish)
                .count(),
            12,
            "eleven finishers plus one unrostered chip"
        );
        assert!(
            !fixture
                .crossings
                .iter()
                .any(|crossing| crossing.chip == dnf_chip && crossing.checkpoint == finish),
            "bib 109 must never reach the finish mat"
        );
        assert_eq!(fixture.expected_timing_events(), 23);
        assert_eq!(fixture.expected_accepted(), 24);
    }

    #[test]
    fn the_criterium_has_five_mat_crossings_per_rider_plus_one_re_read() {
        let fixture = multi_lap();
        assert_eq!(fixture.crossings.len(), 31);
        assert_eq!(fixture.expected_accepted(), 30);
        assert_eq!(
            fixture
                .crossings
                .iter()
                .filter(|crossing| {
                    crossing.expectation == CrossingExpectation::SuppressedAsReRead
                })
                .count(),
            1
        );
    }

    #[test]
    fn every_genuine_lap_is_comfortably_longer_than_the_minimum() {
        let fixture = multi_lap();
        let min_lap = i64::try_from(fixture.policy.min_lap_ms.expect("configured")).expect("fits");

        for participant in &fixture.participants {
            let chip = chip_for(
                participant
                    .bib
                    .as_str()
                    .parse::<u32>()
                    .expect("numeric bib"),
            );
            let laps: Vec<OffsetDateTime> = fixture
                .crossings
                .iter()
                .filter(|crossing| {
                    crossing.chip == chip && crossing.expectation == CrossingExpectation::Accepted
                })
                .map(|crossing| crossing.at)
                .collect();
            assert_eq!(laps.len(), 5);
            for pair in laps.windows(2) {
                let gap = (pair[1] - pair[0]).whole_milliseconds();
                assert!(
                    gap > i128::from(min_lap),
                    "a genuine lap must never be at risk from the minimum-lap rule"
                );
            }
        }
    }

    #[test]
    fn an_unknown_slug_is_not_silently_substituted() {
        assert!(FixtureEvent::by_slug("five-k").is_some());
        assert!(FixtureEvent::by_slug("multi-lap").is_some());
        assert!(FixtureEvent::by_slug("5k").is_none());
    }
}
