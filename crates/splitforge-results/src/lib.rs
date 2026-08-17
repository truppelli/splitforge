//! # splitforge-results
//!
//! Result revisions, placement ranking, and DNS/DNF/DQ status handling.
//!
//! ## Boundaries
//!
//! - **May depend on:** splitforge-domain
//! - **Must never depend on:** splitforge-llrp, splitforge-api, splitforge-sync
//!
//! These rules come from ADR-0001 and are tabulated in `docs/architecture.md`.
//! They are enforced by `crates/splitforge-testkit/tests/dependency_rules.rs`, not left to
//! review (ADR-0012).
//!
//! ## Scoring is a pure function
//!
//! [`score`] takes timing events, a roster, operator declarations, and a policy, and
//! returns result entries. It performs no I/O, holds no state, and is deterministic.
//!
//! That matters more here than anywhere else in the workspace. `docs/roadmap.md` says it
//! plainly — *"complexity here is where scoring bugs live"* — and a scoring bug is not like
//! other bugs: it is published, screenshotted, and believed before anybody notices. A pure
//! function is one that can be exhaustively tested against hand-computed answers without a
//! database, a clock, or a race.
//!
//! ## What this milestone deliberately does not score
//!
//! Age groups, waves, penalties, relay teams, and multi-lap courses with intermediate
//! splits. One start, one finish, one overall placement. Every one of those exclusions is a
//! source of scoring bugs, and none should be built before the basic flow is provably
//! correct — see `docs/roadmap.md`, Milestone 4.

// No panicking on any path reachable during an event: a corrupt frame, a missing
// field, or an out-of-range value must become an error the caller can act on, never a
// timer that stops mid-race. Test code is exempt, where panicking on the unexpected is
// the point. See CONTRIBUTING.md, "Code standards".
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use std::collections::BTreeMap;

use splitforge_domain::{
    Bib, Checkpoint, CheckpointId, CheckpointKind, Participant, ParticipantId, ResultEntry,
    ResultFlag, ResultStatus, ScoringPolicy, StartMode, StatusDeclaration, StatusSource,
    TimingEvent, TimingEventId,
};
use time::OffsetDateTime;

/// Why a race could not be scored at all.
///
/// Each of these is a configuration mistake rather than a data problem, and each would
/// otherwise produce a results sheet that looks complete and is wrong.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScoringError {
    /// No checkpoint is marked as the finish.
    #[error("the race has no finish checkpoint, so nobody can be scored as finished")]
    NoFinishCheckpoint,
    /// More than one checkpoint is marked as the finish.
    ///
    /// Milestone 4 scores one start and one finish. Picking one of several arbitrarily
    /// would silently decide which line counted.
    #[error("the race has {count} finish checkpoints ({names}); this milestone scores exactly one")]
    MultipleFinishCheckpoints {
        /// How many were found.
        count: usize,
        /// Their names, in course order.
        names: String,
    },
    /// More than one checkpoint is marked as the start.
    #[error("the race has {count} start checkpoints ({names}); this milestone scores exactly one")]
    MultipleStartCheckpoints {
        /// How many were found.
        count: usize,
        /// Their names, in course order.
        names: String,
    },
    /// Chip-time scoring was requested, but there is no start line to measure from.
    #[error("chip-time scoring needs a start checkpoint, and this race has none")]
    ChipTimeWithoutStartCheckpoint,
}

/// Everything [`score`] needs.
#[derive(Debug, Clone, Copy)]
pub struct ScoringInput<'a> {
    /// Everyone registered. Scoring covers the roster, not just those who were seen —
    /// a participant with no crossings is a DNS, which is a result, not an absence.
    pub participants: &'a [Participant],
    /// The course.
    pub checkpoints: &'a [Checkpoint],
    /// The timing events derived from the journal, in any order.
    pub timing_events: &'a [TimingEvent],
    /// Operator status declarations, in any order. The highest sequence per participant
    /// wins.
    pub declarations: &'a [StatusDeclaration],
    /// The rules to score under.
    pub policy: &'a ScoringPolicy,
}

/// Scores a race.
///
/// Deterministic: the same input always produces the same entries in the same order.
///
/// # Errors
///
/// Returns [`ScoringError`] when the course is not one this milestone can score — no finish
/// line, several finish lines, or chip-time scoring without a start line.
pub fn score(input: &ScoringInput<'_>) -> Result<Vec<ResultEntry>, ScoringError> {
    let finish = sole_checkpoint(input.checkpoints, CheckpointKind::Finish)?
        .ok_or(ScoringError::NoFinishCheckpoint)?;
    let start = sole_checkpoint(input.checkpoints, CheckpointKind::Start)?;

    if input.policy.start_mode == StartMode::Chip && start.is_none() {
        return Err(ScoringError::ChipTimeWithoutStartCheckpoint);
    }

    // Earliest crossing per (participant, checkpoint). "First valid finish per participant"
    // is the rule for the finish line; the same rule at the start line is what a chip time
    // is measured from.
    let mut earliest: BTreeMap<(ParticipantId, CheckpointId), &TimingEvent> = BTreeMap::new();
    for event in input.timing_events {
        earliest
            .entry((event.participant, event.checkpoint))
            .and_modify(|held| {
                if (event.at, event.id) < (held.at, held.id) {
                    *held = event;
                }
            })
            .or_insert(event);
    }

    // Any crossing anywhere proves they started, even if the start mat missed them.
    let mut seen_anywhere: BTreeMap<ParticipantId, ()> = BTreeMap::new();
    for event in input.timing_events {
        seen_anywhere.insert(event.participant, ());
    }

    let declared = latest_declarations(input.declarations);

    let mut entries: Vec<ResultEntry> = input
        .participants
        .iter()
        .map(|participant| {
            let start_event = start.and_then(|id| earliest.get(&(participant.id, id)).copied());
            let finish_event = earliest.get(&(participant.id, finish)).copied();
            build_entry(
                participant,
                start_event,
                finish_event,
                seen_anywhere.contains_key(&participant.id),
                declared.get(&participant.id).copied(),
                input.policy,
            )
        })
        .collect();

    assign_places(&mut entries);
    sort_for_publication(&mut entries);
    Ok(entries)
}

/// Finds the one checkpoint of a kind, or reports that there are several.
fn sole_checkpoint(
    checkpoints: &[Checkpoint],
    kind: CheckpointKind,
) -> Result<Option<CheckpointId>, ScoringError> {
    let mut matching: Vec<&Checkpoint> = checkpoints
        .iter()
        .filter(|checkpoint| checkpoint.kind == kind)
        .collect();
    matching.sort_by_key(|checkpoint| checkpoint.sequence);

    match matching.as_slice() {
        [] => Ok(None),
        [only] => Ok(Some(only.id)),
        several => {
            let count = several.len();
            let names = several
                .iter()
                .map(|checkpoint| checkpoint.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            Err(match kind {
                CheckpointKind::Finish => ScoringError::MultipleFinishCheckpoints { count, names },
                _ => ScoringError::MultipleStartCheckpoints { count, names },
            })
        }
    }
}

/// The declaration in force for each participant: the one with the highest sequence.
///
/// Reversing a disqualification means declaring a new status, so the newest row wins and
/// every earlier one stays exactly where it is.
fn latest_declarations(
    declarations: &[StatusDeclaration],
) -> BTreeMap<ParticipantId, &StatusDeclaration> {
    let mut latest: BTreeMap<ParticipantId, &StatusDeclaration> = BTreeMap::new();
    for declaration in declarations {
        latest
            .entry(declaration.participant)
            .and_modify(|held| {
                if declaration.seq > held.seq {
                    *held = declaration;
                }
            })
            .or_insert(declaration);
    }
    latest
}

/// Builds one participant's line, before placement.
fn build_entry(
    participant: &Participant,
    start_event: Option<&TimingEvent>,
    finish_event: Option<&TimingEvent>,
    seen_anywhere: bool,
    declaration: Option<&StatusDeclaration>,
    policy: &ScoringPolicy,
) -> ResultEntry {
    let mut flags = Vec::new();

    let start_at = start_event.map(|event| event.at);
    let finish_at = finish_event.map(|event| event.at);

    let gun_time_ms = policy
        .gun_time
        .zip(finish_at)
        .map(|(gun, finish)| elapsed_ms(gun, finish));
    let chip_time_ms = start_at
        .zip(finish_at)
        .map(|(start, finish)| elapsed_ms(start, finish));

    // The measured time placement uses. Chip mode falls back to the gun when a participant
    // has no start detection, because a missed start mat must not turn a finisher into a
    // blank row — but the fallback is flagged, never silent.
    let scoring_time_ms = if finish_at.is_none() {
        None
    } else {
        match policy.start_mode {
            StartMode::Gun => {
                if gun_time_ms.is_none() {
                    flags.push(ResultFlag::NoGunTime);
                }
                gun_time_ms
            }
            StartMode::Chip => match chip_time_ms {
                Some(chip) => Some(chip),
                None => {
                    flags.push(ResultFlag::NoStartReadUnderChipTime);
                    if gun_time_ms.is_none() {
                        flags.push(ResultFlag::NoGunTime);
                    }
                    gun_time_ms
                }
            },
        }
    };

    // A finish that precedes the start it is measured from is physically impossible, so it
    // is reported rather than published as a negative time. The measured values stay
    // visible — they are the diagnostic — but nothing gets placed on them.
    let impossible = scoring_time_ms.is_some_and(|ms| ms < 0);
    if impossible {
        flags.push(ResultFlag::FinishBeforeStart);
    }
    let scoring_time_ms = if impossible { None } else { scoring_time_ms };

    let (status, status_source, status_reason) = match declaration {
        Some(declaration) => (
            declaration.status,
            StatusSource::Declared,
            Some(declaration.reason.clone()),
        ),
        None => {
            let derived = if finish_at.is_some() {
                ResultStatus::Finished
            } else if seen_anywhere {
                ResultStatus::Dnf
            } else {
                ResultStatus::Dns
            };
            (derived, StatusSource::Derived, None)
        }
    };

    if status == ResultStatus::Finished
        && status_source == StatusSource::Declared
        && finish_at.is_none()
    {
        flags.push(ResultFlag::DeclaredFinishedWithoutFinishRead);
    }

    let timing_events: Vec<TimingEventId> = [start_event, finish_event]
        .into_iter()
        .flatten()
        .map(|event| event.id)
        .collect();

    ResultEntry {
        participant: participant.id,
        bib: participant.bib.clone(),
        name: participant.name.clone(),
        status,
        status_source,
        status_reason,
        start_at,
        finish_at,
        gun_time_ms,
        chip_time_ms,
        scoring_time_ms,
        place: None,
        flags,
        timing_events,
    }
}

/// Milliseconds between two instants, saturating rather than overflowing.
fn elapsed_ms(from: OffsetDateTime, to: OffsetDateTime) -> i64 {
    i64::try_from((to - from).whole_milliseconds()).unwrap_or(i64::MAX)
}

/// Assigns overall places by scoring time.
///
/// Standard competition ranking: equal times share a place, and the shared places are
/// consumed — two runners tied for 2nd are both 2nd, and the next is 4th. Anyone not
/// [`ResultStatus::is_placed`], and anyone with no defensible time, takes no place.
fn assign_places(entries: &mut [ResultEntry]) {
    let mut placeable: Vec<usize> = (0..entries.len())
        .filter(|&index| {
            entries[index].status.is_placed() && entries[index].scoring_time_ms.is_some()
        })
        .collect();

    placeable.sort_by(|&left, &right| {
        entries[left]
            .scoring_time_ms
            .cmp(&entries[right].scoring_time_ms)
            .then_with(|| bib_order(&entries[left].bib).cmp(&bib_order(&entries[right].bib)))
            .then_with(|| entries[left].participant.cmp(&entries[right].participant))
    });

    let mut place = 0_u32;
    let mut previous_time: Option<i64> = None;
    for (offset, &index) in placeable.iter().enumerate() {
        let time = entries[index].scoring_time_ms;
        if previous_time != time {
            place = u32::try_from(offset).unwrap_or(u32::MAX).saturating_add(1);
            previous_time = time;
        }
        entries[index].place = Some(place);
    }
}

/// Orders entries the way a results sheet reads: finishers by place, then everyone else.
fn sort_for_publication(entries: &mut [ResultEntry]) {
    entries.sort_by(|left, right| {
        // Placed entries first, in place order. `None` sorts last rather than first, which
        // is the opposite of Option's natural order.
        let key = |entry: &ResultEntry| (entry.place.is_none(), entry.place.unwrap_or(0));
        key(left)
            .cmp(&key(right))
            .then_with(|| left.status.cmp(&right.status))
            .then_with(|| bib_order(&left.bib).cmp(&bib_order(&right.bib)))
            .then_with(|| left.participant.cmp(&right.participant))
    });
}

/// Sort key that orders numeric bibs numerically and everything else lexicographically.
///
/// Bibs are strings because they are not always numeric, but when they are, `10` belongs
/// after `9` on a results sheet.
fn bib_order(bib: &Bib) -> (u8, u64, &str) {
    bib.as_str()
        .parse::<u64>()
        .map_or((1, 0, bib.as_str()), |number| (0, number, bib.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use splitforge_domain::{
        AcceptedReadId, RaceId, StatusDeclarationId, TimingEvent, TimingPolicy,
    };
    use time::Duration;

    const GUN: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;

    fn at(seconds: i64) -> OffsetDateTime {
        GUN + Duration::seconds(seconds)
    }

    struct Course {
        race: RaceId,
        start: Checkpoint,
        finish: Checkpoint,
    }

    fn course() -> Course {
        let race = RaceId::new();
        Course {
            race,
            start: Checkpoint {
                id: CheckpointId::new(),
                race,
                name: "start".to_owned(),
                kind: CheckpointKind::Start,
                sequence: 0,
            },
            finish: Checkpoint {
                id: CheckpointId::new(),
                race,
                name: "finish".to_owned(),
                kind: CheckpointKind::Finish,
                sequence: 1,
            },
        }
    }

    fn runner(course: &Course, bib: &str) -> Participant {
        Participant {
            id: ParticipantId::new(),
            race: course.race,
            bib: Bib::new(bib),
            name: format!("Runner {bib}"),
        }
    }

    fn crossing(participant: &Participant, checkpoint: &Checkpoint, seconds: i64) -> TimingEvent {
        TimingEvent::from_accepted(
            participant.id,
            checkpoint.id,
            at(seconds),
            1,
            AcceptedReadId::new(),
        )
    }

    fn declaration(
        seq: u64,
        course: &Course,
        participant: &Participant,
        status: ResultStatus,
        reason: &str,
    ) -> StatusDeclaration {
        StatusDeclaration {
            id: StatusDeclarationId::new(),
            seq,
            race: course.race,
            participant: participant.id,
            status,
            reason: reason.to_owned(),
            actor: "referee".to_owned(),
            declared_at: at(10_000),
        }
    }

    fn gun_policy() -> ScoringPolicy {
        ScoringPolicy {
            start_mode: StartMode::Gun,
            timing: TimingPolicy::default(),
            gun_time: Some(GUN),
        }
    }

    fn chip_policy() -> ScoringPolicy {
        ScoringPolicy {
            start_mode: StartMode::Chip,
            ..gun_policy()
        }
    }

    fn run(
        course: &Course,
        participants: &[Participant],
        events: &[TimingEvent],
        declarations: &[StatusDeclaration],
        policy: &ScoringPolicy,
    ) -> Vec<ResultEntry> {
        score(&ScoringInput {
            participants,
            checkpoints: &[course.start.clone(), course.finish.clone()],
            timing_events: events,
            declarations,
            policy,
        })
        .expect("scoreable course")
    }

    #[test]
    fn finishers_are_placed_by_gun_time_in_crossing_order() {
        let course = course();
        let first = runner(&course, "101");
        let second = runner(&course, "102");
        let events = vec![
            crossing(&first, &course.finish, 1_200),
            crossing(&second, &course.finish, 1_100),
        ];

        let entries = run(
            &course,
            &[first.clone(), second.clone()],
            &events,
            &[],
            &gun_policy(),
        );

        assert_eq!(entries[0].bib.as_str(), "102");
        assert_eq!(entries[0].place, Some(1));
        assert_eq!(entries[0].gun_time_ms, Some(1_100_000));
        assert_eq!(entries[1].bib.as_str(), "101");
        assert_eq!(entries[1].place, Some(2));
    }

    #[test]
    fn the_first_finish_crossing_is_the_one_that_counts() {
        // A runner who crosses the finish mat, then wanders back over it to find their bag.
        let course = course();
        let runner = runner(&course, "101");
        let events = vec![
            crossing(&runner, &course.finish, 1_100),
            crossing(&runner, &course.finish, 1_400),
        ];

        let entries = run(&course, &[runner], &events, &[], &gun_policy());

        assert_eq!(entries[0].finish_at, Some(at(1_100)));
        assert_eq!(entries[0].gun_time_ms, Some(1_100_000));
    }

    #[test]
    fn a_runner_who_started_but_never_finished_is_a_dnf() {
        let course = course();
        let runner = runner(&course, "101");
        let events = vec![crossing(&runner, &course.start, 0)];

        let entries = run(&course, &[runner], &events, &[], &gun_policy());

        assert_eq!(entries[0].status, ResultStatus::Dnf);
        assert_eq!(entries[0].status_source, StatusSource::Derived);
        assert_eq!(entries[0].place, None);
        assert_eq!(entries[0].scoring_time_ms, None);
    }

    #[test]
    fn a_registered_runner_who_never_appeared_is_a_dns() {
        let course = course();
        let runner = runner(&course, "101");

        let entries = run(&course, &[runner], &[], &[], &gun_policy());

        assert_eq!(entries[0].status, ResultStatus::Dns);
        assert_eq!(
            entries.len(),
            1,
            "a DNS is a result, not an absence — scoring covers the roster"
        );
    }

    #[test]
    fn chip_time_measures_from_the_runners_own_start() {
        let course = course();
        let runner = runner(&course, "101");
        // Crossed the start mat 40 s after the gun, finished 1200 s after the gun.
        let events = vec![
            crossing(&runner, &course.start, 40),
            crossing(&runner, &course.finish, 1_200),
        ];

        let entries = run(&course, &[runner], &events, &[], &chip_policy());

        assert_eq!(entries[0].gun_time_ms, Some(1_200_000));
        assert_eq!(entries[0].chip_time_ms, Some(1_160_000));
        assert_eq!(
            entries[0].scoring_time_ms,
            Some(1_160_000),
            "chip mode places on chip time"
        );
        assert!(entries[0].flags.is_empty());
    }

    #[test]
    fn a_missed_start_mat_falls_back_to_gun_time_and_says_so() {
        let course = course();
        let runner = runner(&course, "101");
        let events = vec![crossing(&runner, &course.finish, 1_200)];

        let entries = run(&course, &[runner], &events, &[], &chip_policy());

        assert_eq!(entries[0].status, ResultStatus::Finished);
        assert_eq!(entries[0].chip_time_ms, None);
        assert_eq!(
            entries[0].scoring_time_ms,
            Some(1_200_000),
            "a missed start mat must not turn a finisher into a blank row"
        );
        assert_eq!(entries[0].flags, vec![ResultFlag::NoStartReadUnderChipTime]);
        assert_eq!(entries[0].place, Some(1));
    }

    #[test]
    fn a_disqualification_keeps_the_time_but_takes_no_place() {
        let course = course();
        let winner = runner(&course, "101");
        let cheat = runner(&course, "102");
        let third = runner(&course, "103");
        let events = vec![
            crossing(&cheat, &course.finish, 1_000),
            crossing(&winner, &course.finish, 1_100),
            crossing(&third, &course.finish, 1_200),
        ];
        let declarations = vec![declaration(
            1,
            &course,
            &cheat,
            ResultStatus::Dq,
            "course cut",
        )];

        let entries = run(
            &course,
            &[winner.clone(), cheat.clone(), third.clone()],
            &events,
            &declarations,
            &gun_policy(),
        );

        let disqualified = entries
            .iter()
            .find(|entry| entry.bib.as_str() == "102")
            .expect("the DQ is still on the sheet");
        assert_eq!(disqualified.status, ResultStatus::Dq);
        assert_eq!(disqualified.status_source, StatusSource::Declared);
        assert_eq!(disqualified.status_reason.as_deref(), Some("course cut"));
        assert_eq!(
            disqualified.gun_time_ms,
            Some(1_000_000),
            "the clock did not lie about when they crossed"
        );
        assert_eq!(disqualified.place, None);

        // Everyone behind them moves up.
        assert_eq!(
            entries
                .iter()
                .find(|entry| entry.bib.as_str() == "101")
                .and_then(|entry| entry.place),
            Some(1)
        );
        assert_eq!(
            entries
                .iter()
                .find(|entry| entry.bib.as_str() == "103")
                .and_then(|entry| entry.place),
            Some(2)
        );
    }

    #[test]
    fn a_reinstated_runner_is_scored_by_the_newest_declaration() {
        let course = course();
        let runner = runner(&course, "101");
        let events = vec![crossing(&runner, &course.finish, 1_000)];
        // Disqualified, then reinstated on appeal. Both rows exist; the newest is in force.
        let declarations = vec![
            declaration(1, &course, &runner, ResultStatus::Dq, "course cut"),
            declaration(
                2,
                &course,
                &runner,
                ResultStatus::Finished,
                "appeal upheld — marshal error",
            ),
        ];

        let entries = run(&course, &[runner], &events, &declarations, &gun_policy());

        assert_eq!(entries[0].status, ResultStatus::Finished);
        assert_eq!(
            entries[0].status_reason.as_deref(),
            Some("appeal upheld — marshal error")
        );
        assert_eq!(entries[0].place, Some(1));
    }

    #[test]
    fn declaration_order_in_the_input_does_not_decide_the_outcome() {
        let course = course();
        let runner = runner(&course, "101");
        let events = vec![crossing(&runner, &course.finish, 1_000)];
        let dq = declaration(1, &course, &runner, ResultStatus::Dq, "course cut");
        let reinstated = declaration(2, &course, &runner, ResultStatus::Finished, "appeal");

        let forward = run(
            &course,
            std::slice::from_ref(&runner),
            &events,
            &[dq.clone(), reinstated.clone()],
            &gun_policy(),
        );
        let reversed = run(
            &course,
            &[runner],
            &events,
            &[reinstated, dq],
            &gun_policy(),
        );

        assert_eq!(forward, reversed, "sequence decides, not input order");
        assert_eq!(forward[0].status, ResultStatus::Finished);
    }

    #[test]
    fn an_operator_can_declare_a_finish_the_chip_never_recorded() {
        // A dead chip at the finish. The marshal saw them cross and says so.
        let course = course();
        let runner = runner(&course, "101");
        let events = vec![crossing(&runner, &course.start, 0)];
        let declarations = vec![declaration(
            1,
            &course,
            &runner,
            ResultStatus::Finished,
            "chip failed; timed by hand at 20:00",
        )];

        let entries = run(&course, &[runner], &events, &declarations, &gun_policy());

        assert_eq!(entries[0].status, ResultStatus::Finished);
        assert_eq!(
            entries[0].flags,
            vec![ResultFlag::DeclaredFinishedWithoutFinishRead],
            "the declaration stands, and the entry says the evidence is missing"
        );
        assert_eq!(
            entries[0].place, None,
            "no finish time means nothing to place them on"
        );
    }

    #[test]
    fn tied_times_share_a_place_and_consume_it() {
        let course = course();
        let first = runner(&course, "101");
        let tied_a = runner(&course, "102");
        let tied_b = runner(&course, "103");
        let fourth = runner(&course, "104");
        let events = vec![
            crossing(&first, &course.finish, 1_000),
            crossing(&tied_a, &course.finish, 1_100),
            crossing(&tied_b, &course.finish, 1_100),
            crossing(&fourth, &course.finish, 1_200),
        ];

        let entries = run(
            &course,
            &[first, tied_a, tied_b, fourth],
            &events,
            &[],
            &gun_policy(),
        );

        let place_of = |bib: &str| {
            entries
                .iter()
                .find(|entry| entry.bib.as_str() == bib)
                .and_then(|entry| entry.place)
        };
        assert_eq!(place_of("101"), Some(1));
        assert_eq!(place_of("102"), Some(2));
        assert_eq!(place_of("103"), Some(2));
        assert_eq!(place_of("104"), Some(4), "the shared place is consumed");
    }

    #[test]
    fn a_finish_before_the_start_is_reported_not_published_as_a_negative_time() {
        let course = course();
        let runner = runner(&course, "101");
        // Clock problem or mis-mapped antenna: the finish precedes the start.
        let events = vec![
            crossing(&runner, &course.start, 500),
            crossing(&runner, &course.finish, 100),
        ];

        let entries = run(&course, &[runner], &events, &[], &chip_policy());

        assert_eq!(
            entries[0].chip_time_ms,
            Some(-400_000),
            "kept as diagnostic"
        );
        assert_eq!(entries[0].scoring_time_ms, None);
        assert_eq!(entries[0].place, None);
        assert!(entries[0].flags.contains(&ResultFlag::FinishBeforeStart));
    }

    #[test]
    fn with_no_gun_recorded_a_finisher_is_flagged_rather_than_placed() {
        let course = course();
        let runner = runner(&course, "101");
        let events = vec![crossing(&runner, &course.finish, 1_000)];
        let policy = ScoringPolicy {
            gun_time: None,
            ..gun_policy()
        };

        let entries = run(&course, &[runner], &events, &[], &policy);

        assert_eq!(entries[0].status, ResultStatus::Finished);
        assert_eq!(entries[0].scoring_time_ms, None);
        assert_eq!(entries[0].place, None);
        assert_eq!(entries[0].flags, vec![ResultFlag::NoGunTime]);
    }

    #[test]
    fn numeric_bibs_sort_numerically_among_equal_times() {
        let course = course();
        let nine = runner(&course, "9");
        let ten = runner(&course, "10");
        let events = vec![
            crossing(&nine, &course.finish, 1_000),
            crossing(&ten, &course.finish, 1_000),
        ];

        let entries = run(&course, &[ten, nine], &events, &[], &gun_policy());

        assert_eq!(
            entries[0].bib.as_str(),
            "9",
            "bib 10 belongs after bib 9, not before it"
        );
    }

    #[test]
    fn scoring_is_deterministic_regardless_of_input_order() {
        let course = course();
        let runners: Vec<Participant> = (101..=110)
            .map(|bib| runner(&course, &bib.to_string()))
            .collect();
        let mut events: Vec<TimingEvent> = runners
            .iter()
            .enumerate()
            .map(|(index, runner)| {
                crossing(
                    runner,
                    &course.finish,
                    1_000 + i64::try_from(index).unwrap_or(0),
                )
            })
            .collect();

        let forward = run(&course, &runners, &events, &[], &gun_policy());
        events.reverse();
        let mut shuffled = runners.clone();
        shuffled.reverse();
        let reversed = run(&course, &shuffled, &events, &[], &gun_policy());

        assert_eq!(forward, reversed);
    }

    #[test]
    fn a_course_with_no_finish_line_refuses_to_score() {
        let course = course();
        let result = score(&ScoringInput {
            participants: &[],
            checkpoints: std::slice::from_ref(&course.start),
            timing_events: &[],
            declarations: &[],
            policy: &gun_policy(),
        });
        assert_eq!(result, Err(ScoringError::NoFinishCheckpoint));
    }

    #[test]
    fn two_finish_lines_are_refused_rather_than_arbitrarily_resolved() {
        let course = course();
        let second_finish = Checkpoint {
            id: CheckpointId::new(),
            name: "finish-b".to_owned(),
            sequence: 2,
            ..course.finish.clone()
        };
        let result = score(&ScoringInput {
            participants: &[],
            checkpoints: &[course.start, course.finish, second_finish],
            timing_events: &[],
            declarations: &[],
            policy: &gun_policy(),
        });
        assert!(matches!(
            result,
            Err(ScoringError::MultipleFinishCheckpoints { count: 2, .. })
        ));
    }

    #[test]
    fn chip_scoring_without_a_start_line_is_refused() {
        let course = course();
        let result = score(&ScoringInput {
            participants: &[],
            checkpoints: &[course.finish],
            timing_events: &[],
            declarations: &[],
            policy: &chip_policy(),
        });
        assert_eq!(result, Err(ScoringError::ChipTimeWithoutStartCheckpoint));
    }

    #[test]
    fn gun_scoring_works_on_a_course_with_only_a_finish_line() {
        // Plenty of small races have one mat. Requiring a start line would refuse to score
        // an event that is perfectly scoreable from the gun.
        let course = course();
        let runner = runner(&course, "101");
        let events = vec![crossing(&runner, &course.finish, 1_000)];

        let entries = score(&ScoringInput {
            participants: &[runner],
            checkpoints: &[course.finish],
            timing_events: &events,
            declarations: &[],
            policy: &gun_policy(),
        })
        .expect("one mat is enough for gun time");

        assert_eq!(entries[0].place, Some(1));
        assert_eq!(entries[0].gun_time_ms, Some(1_000_000));
    }
}
