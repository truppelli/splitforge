use super::*;
use splitforge_domain::{
    AcceptedReadId, AntennaMap, Bib, Event, EventId, Participant, Race, RaceSession,
    ResultRevisionId, ResultStatus, RevisionStatus, ScoringPolicy, SessionAction, StatusSource,
    TimingPolicy,
};
use time::Duration;
use time::macros::datetime;

const GUN: OffsetDateTime = datetime!(2026-10-11 11:30 UTC);

struct Fixture {
    config: RaceConfig,
    course: Course,
    start: CheckpointId,
    ten_k: CheckpointId,
    finish: CheckpointId,
    ada: ParticipantId,
    ben: ParticipantId,
}

fn checkpoint(race: RaceId, name: &str, kind: CheckpointKind, sequence: u16) -> Checkpoint {
    Checkpoint {
        id: CheckpointId::new(),
        race,
        name: name.to_owned(),
        kind,
        sequence,
    }
}

fn participant(race: RaceId, bib: &str, name: &str) -> Participant {
    Participant {
        id: ParticipantId::new(),
        race,
        bib: Bib::new(bib),
        name: name.to_owned(),
    }
}

/// A half marathon with a 10K split, started at [`GUN`], and two runners.
fn half() -> Fixture {
    let event = Event {
        id: EventId::new(),
        name: "Harbor Point".to_owned(),
    };
    let race = Race {
        id: RaceId::new(),
        event: event.id,
        name: "Half marathon".to_owned(),
        scheduled_start: None,
        expected_laps: 1,
    };
    let start = checkpoint(race.id, "Start", CheckpointKind::Start, 0);
    let ten_k = checkpoint(race.id, "10K", CheckpointKind::Split, 1);
    let finish = checkpoint(race.id, "Finish", CheckpointKind::Finish, 2);
    let ada = participant(race.id, "12", "Ada Lovelace");
    let ben = participant(race.id, "13", "");
    let (start_id, ten_k_id, finish_id, ada_id, ben_id) =
        (start.id, ten_k.id, finish.id, ada.id, ben.id);
    let course = Course {
        race: race.id,
        key: "half".to_owned(),
        meters: 21_097.5,
        splits: BTreeMap::from([("10K".to_owned(), 10_000.0)]),
    };
    let sessions = vec![RaceSession {
        seq: 1,
        race: race.id,
        action: SessionAction::Start,
        at: GUN,
        actor: "op".to_owned(),
        note: None,
        recorded_at: GUN,
    }];
    Fixture {
        config: RaceConfig {
            event,
            race,
            checkpoints: vec![start, ten_k, finish],
            participants: vec![ada, ben],
            assignments: vec![],
            readers: vec![],
            antennas: AntennaMap::new(),
            policy: TimingPolicy::default(),
            start_mode: StartMode::Gun,
            sessions,
        },
        course,
        start: start_id,
        ten_k: ten_k_id,
        finish: finish_id,
        ada: ada_id,
        ben: ben_id,
    }
}

fn event(who: ParticipantId, at: CheckpointId, minutes: i64, lap: u16) -> TimingEvent {
    TimingEvent::from_accepted(
        who,
        at,
        GUN + Duration::minutes(minutes),
        lap,
        AcceptedReadId::new(),
    )
}

fn runner(bib: &str) -> RunnerRef {
    RunnerRef {
        distance_key: "half".to_owned(),
        bib: bib.to_owned(),
    }
}

#[test]
fn the_manifest_places_start_and_finish_itself_and_splits_from_the_course() {
    let f = half();
    let manifest = manifest(&[(&f.config, &f.course)]).unwrap();
    let distance = &manifest.distances[0];
    assert_eq!(distance.key, "half");
    assert_eq!(distance.name, "Half marathon");
    assert_eq!(distance.gun_at, Some(GUN));
    assert_eq!(distance.laps, 1);
    let points: Vec<_> = distance
        .timing_points
        .iter()
        .map(|p| (p.key.as_str(), p.meters))
        .collect();
    assert_eq!(
        points,
        [("start", 0.0), ("10k", 10_000.0), ("finish", 21_097.5)]
    );
}

#[test]
fn a_lap_course_places_the_finish_at_one_lap() {
    let mut f = half();
    f.config.race.expected_laps = 4;
    f.course.meters = 40_000.0;
    let manifest = manifest(&[(&f.config, &f.course)]).unwrap();
    let finish = manifest.distances[0].timing_points.last().unwrap();
    assert_eq!(finish.meters, 10_000.0);
    assert_eq!(manifest.distances[0].laps, 4);
}

#[test]
fn a_manifest_the_site_would_refuse_is_refused_here_with_what_to_fix() {
    let f = half();

    let mut unplaced = f.course.clone();
    unplaced.splits.clear();
    assert_eq!(
        manifest(&[(&f.config, &unplaced)]),
        Err(PublishError::SplitUnplaced {
            race: f.config.race.id,
            checkpoint: "10K".to_owned()
        })
    );

    let mut off = f.course.clone();
    off.splits.insert("10K".to_owned(), 30_000.0);
    assert!(matches!(
        manifest(&[(&f.config, &off)]),
        Err(PublishError::SplitOffCourse { .. })
    ));

    let mut no_distance = f.course.clone();
    no_distance.meters = 0.0;
    assert!(matches!(
        manifest(&[(&f.config, &no_distance)]),
        Err(PublishError::NoDistance { .. })
    ));

    let mut long_key = f.course.clone();
    long_key.key = "x".repeat(41);
    assert!(matches!(
        manifest(&[(&f.config, &long_key)]),
        Err(PublishError::InvalidKey { .. })
    ));

    assert!(matches!(
        manifest(&[(&f.config, &f.course), (&f.config, &f.course)]),
        Err(PublishError::DuplicateKey { .. })
    ));

    let other = half();
    assert!(matches!(
        manifest(&[(&f.config, &other.course)]),
        Err(PublishError::CourseMismatch { .. })
    ));
}

#[test]
fn two_checkpoints_that_publish_under_one_key_are_refused() {
    let mut f = half();
    let race = f.config.race.id;
    f.config
        .checkpoints
        .push(checkpoint(race, "10k!", CheckpointKind::Split, 3));
    f.course.splits.insert("10k!".to_owned(), 10_000.0);
    assert!(matches!(
        manifest(&[(&f.config, &f.course)]),
        Err(PublishError::PointKeyCollision { .. })
    ));
}

#[test]
fn point_keys_read_well_in_a_url() {
    assert_eq!(point_key("Finish"), "finish");
    assert_eq!(point_key("Mile 13.1 — Bridge"), "mile-13-1-bridge");
    assert_eq!(point_key("  --  "), "point");
    assert_eq!(point_key(&"a".repeat(60)).len(), MAX_KEY_LEN);
}

#[test]
fn crossings_carry_time_since_the_gun_and_leave_out_what_is_not_the_race() {
    let f = half();
    let stranger = ParticipantId::new();
    let events = [
        // A warm-up over the start mat before the gun is lap 0 (ADR-0029).
        event(f.ada, f.start, -20, 0),
        // Crossing the start a moment before the gun is still lap 1, and not negative.
        TimingEvent::from_accepted(
            f.ada,
            f.start,
            GUN - Duration::seconds(2),
            1,
            AcceptedReadId::new(),
        ),
        event(f.ada, f.ten_k, 40, 1),
        event(f.ben, f.ten_k, 41, 1),
        event(stranger, f.ten_k, 42, 1),
        // Another race's mat is not this race's to publish, and is not reported either.
        event(f.ada, CheckpointId::new(), 43, 1),
    ];
    let out = crossings(&f.config, &f.course, &events).unwrap();

    let ada = &out.runners[&runner("12")];
    assert_eq!(
        ada.iter()
            .map(|c| (c.point_key.as_str(), c.elapsed_ms))
            .collect::<Vec<_>>(),
        [("start", Some(0)), ("10k", Some(2_400_000))]
    );
    assert_eq!(ada[1].name.as_deref(), Some("Ada Lovelace"));

    // No name in the roster is no name on the wire, not an empty string.
    assert_eq!(out.runners[&runner("13")][0].name, None);

    let reasons: Vec<_> = out.skipped.iter().map(|s| s.reason.clone()).collect();
    assert_eq!(
        reasons,
        [SkipReason::BeforeTheGun, SkipReason::UnknownParticipant]
    );
}

#[test]
fn with_no_gun_a_crossing_goes_without_elapsed_time_rather_than_a_wrong_one() {
    let mut f = half();
    f.config.sessions.clear();
    let out = crossings(&f.config, &f.course, &[event(f.ada, f.ten_k, 40, 1)]).unwrap();
    assert_eq!(out.runners[&runner("12")][0].elapsed_ms, None);
    assert_eq!(
        manifest(&[(&f.config, &f.course)]).unwrap().distances[0].gun_at,
        None
    );
}

#[test]
fn a_bib_the_site_cannot_store_is_skipped_not_sent() {
    let mut f = half();
    f.config.participants[0].bib = Bib::new("x".repeat(21));
    let out = crossings(&f.config, &f.course, &[event(f.ada, f.ten_k, 40, 1)]).unwrap();
    assert!(out.runners.is_empty());
    assert_eq!(out.skipped[0].reason, SkipReason::BibTooLong);
}

#[test]
fn only_runners_whose_crossings_changed_are_sent_and_a_runner_who_lost_them_all_is_too() {
    let f = half();
    let first = crossings(
        &f.config,
        &f.course,
        &[event(f.ada, f.ten_k, 40, 1), event(f.ben, f.ten_k, 41, 1)],
    )
    .unwrap();
    assert_eq!(
        changed(&BTreeMap::new(), &first.runners),
        [runner("12"), runner("13")]
    );
    assert!(changed(&first.runners, &first.runners).is_empty());

    // The chip read as Ben's was Ada's all along: Ada gains a crossing, Ben loses his only one.
    let second = crossings(
        &f.config,
        &f.course,
        &[event(f.ada, f.ten_k, 40, 1), event(f.ada, f.finish, 90, 1)],
    )
    .unwrap();
    let moved = changed(&first.runners, &second.runners);
    assert_eq!(moved, [runner("12"), runner("13")]);

    let sent = batches(&second.runners, &moved, 5000);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].replaces, moved);
    // Ben is named with nothing, which takes him off the board.
    assert_eq!(sent[0].crossings.len(), 2);
    assert!(sent[0].crossings.iter().all(|c| c.bib == "12"));
}

#[test]
fn a_runner_is_never_split_across_two_batches() {
    let f = half();
    let events = [
        event(f.ada, f.start, 0, 1),
        event(f.ada, f.ten_k, 40, 1),
        event(f.ada, f.finish, 90, 1),
        event(f.ben, f.start, 0, 1),
        event(f.ben, f.ten_k, 41, 1),
    ];
    let now = crossings(&f.config, &f.course, &events).unwrap().runners;
    let all: Vec<_> = now.keys().cloned().collect();

    let sent = batches(&now, &all, 4);
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0].replaces, [runner("12")]);
    assert_eq!(sent[0].crossings.len(), 3);
    assert_eq!(sent[1].replaces, [runner("13")]);
    assert_eq!(sent[1].crossings.len(), 2);

    // A cap on runners, not only on crossings.
    assert_eq!(batches(&now, &all, 1).len(), 2);
    assert!(batches(&now, &[], 10).is_empty());
}

fn entry(who: &Participant, place: Option<u32>, events: Vec<TimingEventId>) -> ResultEntry {
    ResultEntry {
        participant: who.id,
        bib: who.bib.clone(),
        name: who.name.clone(),
        status: if place.is_some() {
            ResultStatus::Finished
        } else {
            ResultStatus::Dnf
        },
        status_source: StatusSource::Derived,
        status_reason: None,
        start_at: Some(GUN + Duration::seconds(30)),
        finish_at: place.map(|_| GUN + Duration::minutes(90)),
        gun_time_ms: place.map(|_| 5_400_000),
        chip_time_ms: place.map(|_| 5_370_000),
        scoring_time_ms: place.map(|_| 5_400_000),
        place,
        flags: vec![],
        timing_events: events,
    }
}

fn published(f: &Fixture, entries: Vec<ResultEntry>, policy: ScoringPolicy) -> ResultRevision {
    ResultRevision {
        id: ResultRevisionId::new(),
        race: f.config.race.id,
        number: 3,
        status: RevisionStatus::Final,
        generated_at: GUN + Duration::hours(3),
        actor: "op".to_owned(),
        reason: "Corrected bib 13".to_owned(),
        policy,
        entries,
    }
}

#[test]
fn a_revision_is_one_distance_with_its_number_digest_and_splits() {
    let f = half();
    let split = event(f.ada, f.ten_k, 40, 1);
    let finish = event(f.ada, f.finish, 90, 1);
    let rev = published(
        &f,
        vec![
            entry(
                &f.config.participants[0],
                Some(1),
                vec![split.id, finish.id],
            ),
            entry(&f.config.participants[1], None, vec![]),
        ],
        ScoringPolicy::gun(Some(GUN)),
    );
    let out = revision(&f.config, &f.course, &rev, &[split, finish]).unwrap();

    assert_eq!(out.revision, 3);
    assert_eq!(out.status, "final");
    assert_eq!(out.distance_key, "half");
    assert_eq!(out.digest, rev.digest());
    assert_eq!(out.note.as_deref(), Some("Corrected bib 13"));
    assert_eq!(out.published_at, rev.generated_at);

    let ada = &out.entries[0];
    assert_eq!(ada.status, "finished");
    assert_eq!(ada.overall_place, Some(1));
    // Only intermediate mats are splits, measured from the gun under gun timing.
    assert_eq!(
        ada.splits,
        [Split {
            point_key: "10k".to_owned(),
            elapsed_ms: 2_400_000
        }]
    );

    // A blank name is published as the bib rather than refusing everyone's results.
    assert_eq!(out.entries[1].name, "Bib 13");
    assert_eq!(out.entries[1].status, "dnf");
}

#[test]
fn under_chip_timing_splits_run_from_the_runners_own_start() {
    let f = half();
    let split = event(f.ada, f.ten_k, 40, 1);
    let policy = ScoringPolicy {
        start_mode: StartMode::Chip,
        ..ScoringPolicy::gun(Some(GUN))
    };
    let rev = published(
        &f,
        vec![entry(&f.config.participants[0], Some(1), vec![split.id])],
        policy,
    );
    let out = revision(&f.config, &f.course, &rev, &[split]).unwrap();
    assert_eq!(out.entries[0].splits[0].elapsed_ms, 2_370_000);
}

#[test]
fn a_revision_the_site_would_refuse_whole_is_refused_here() {
    let mut f = half();
    let rev = published(&f, vec![], ScoringPolicy::gun(Some(GUN)));
    let other = half();
    assert!(matches!(
        revision(&f.config, &other.course, &rev, &[]),
        Err(PublishError::CourseMismatch { .. })
    ));

    f.config.participants[0].bib = Bib::new("x".repeat(21));
    let long = published(
        &f,
        vec![entry(&f.config.participants[0], Some(1), vec![])],
        ScoringPolicy::gun(Some(GUN)),
    );
    assert!(matches!(
        revision(&f.config, &f.course, &long, &[]),
        Err(PublishError::BibTooLong { .. })
    ));
}

#[test]
fn publishing_is_deterministic() {
    let f = half();
    let events = [event(f.ada, f.ten_k, 40, 1), event(f.ben, f.ten_k, 41, 1)];
    let a = crossings(&f.config, &f.course, &events).unwrap();
    let b = crossings(&f.config, &f.course, &events).unwrap();
    assert_eq!(a, b);
    let json_a = serde_json::to_string(&batches(
        &a.runners,
        &changed(&BTreeMap::new(), &a.runners),
        10,
    ))
    .unwrap();
    let json_b = serde_json::to_string(&batches(
        &b.runners,
        &changed(&BTreeMap::new(), &b.runners),
        10,
    ))
    .unwrap();
    assert_eq!(json_a, json_b);
}
