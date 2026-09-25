//! What SplitForge derived, as RaceDay Connect reads it.
//!
//! Three documents, all pure functions of state SplitForge already holds:
//!
//! - [`manifest`] — the course: one distance per SplitForge race, with the gun when there
//!   is one. Sent again whenever the gun changes, because the live board works elapsed
//!   time out from it.
//! - [`crossings`], [`changed`] and [`batches`] — live crossings, **a runner at a time**.
//!   Crossings are re-derived from the journal on every pass, so one already sent can stop
//!   being true: a chip reassigned to another bib, a lap renumbered by a backdated gun
//!   (ADR-0029), a read set aside. Each runner whose crossings changed is sent whole and
//!   named in `replaces`, so RaceDay Connect drops whatever it held that is no longer
//!   true. A runner whose crossings did not change is not sent at all.
//! - [`revision`] — one published result revision, as a revision of one distance.
//!
//! Nothing here reads a clock, a file or the network, so nothing here can make timing
//! wait. Given the same derivation it returns the same bytes.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use splitforge_domain::{
    Checkpoint, CheckpointId, CheckpointKind, ParticipantId, RaceConfig, RaceId, ResultEntry,
    ResultRevision, StartMode, TimingEvent, TimingEventId,
};
use time::OffsetDateTime;

use super::contract::{
    Crossing, CrossingsBatch, Distance, MAX_BIB_LEN, MAX_ENTRIES_PER_REVISION, MAX_KEY_LEN,
    Manifest, ResultRow, ResultsRevision, RunnerRef, Split, TimingPoint,
};
use super::course::Course;

/// Why a document could not be built. Each one needs the operator, not a retry.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum PublishError {
    /// The course describes a different race from the one being published.
    #[error("the course is for race {course}, not race {race}")]
    CourseMismatch {
        /// The race being published.
        race: RaceId,
        /// The race the course names.
        course: RaceId,
    },
    /// A distance key RaceDay Connect would refuse.
    #[error("distance key {key:?} must be 1 to {MAX_KEY_LEN} characters")]
    InvalidKey {
        /// The key.
        key: String,
    },
    /// Two races published under one key.
    #[error("two races are published as distance {key:?}")]
    DuplicateKey {
        /// The key.
        key: String,
    },
    /// A distance that is not a positive number of metres.
    #[error("race {race}: the distance must be more than 0 metres")]
    NoDistance {
        /// The race.
        race: RaceId,
    },
    /// A split the course does not place.
    #[error("race {race}: say how far along the course split {checkpoint:?} is")]
    SplitUnplaced {
        /// The race.
        race: RaceId,
        /// The checkpoint's name.
        checkpoint: String,
    },
    /// A split placed off the course.
    #[error("race {race}: split {checkpoint:?} at {meters} m is outside one lap of {lap} m")]
    SplitOffCourse {
        /// The race.
        race: RaceId,
        /// The checkpoint's name.
        checkpoint: String,
        /// Where the course places it.
        meters: f64,
        /// One lap's length.
        lap: f64,
    },
    /// Two checkpoints whose names reduce to one key.
    #[error("race {race}: checkpoints {first:?} and {second:?} both publish as {key:?}")]
    PointKeyCollision {
        /// The race.
        race: RaceId,
        /// The first checkpoint's name.
        first: String,
        /// The second checkpoint's name.
        second: String,
        /// The key both reduce to.
        key: String,
    },
    /// A revision too large for one request.
    #[error("the revision has {count} rows; RaceDay Connect accepts {MAX_ENTRIES_PER_REVISION}")]
    TooManyEntries {
        /// How many.
        count: usize,
    },
    /// A bib RaceDay Connect cannot store, in a revision — which would refuse it whole.
    #[error("bib {bib:?} is longer than the {MAX_BIB_LEN} characters RaceDay Connect stores")]
    BibTooLong {
        /// The bib.
        bib: String,
    },
}

/// The key a checkpoint publishes as: its name, lowercased, with anything but letters and
/// digits turned into single hyphens.
///
/// Derived from the name rather than the checkpoint's identifier so that the key reads
/// sensibly in a URL, and stays the same when an event is rebuilt from its configuration.
#[must_use]
pub fn point_key(name: &str) -> String {
    let mut key = String::with_capacity(name.len());
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            key.push(c.to_ascii_lowercase());
        } else if !key.is_empty() && !key.ends_with('-') {
            key.push('-');
        }
    }
    let key = key.trim_end_matches('-');
    let key: String = key.chars().take(MAX_KEY_LEN).collect();
    let key = key.trim_end_matches('-');
    if key.is_empty() {
        "point".to_owned()
    } else {
        key.to_owned()
    }
}

/// The course document for every race published to one RaceDay Connect race.
///
/// # Errors
///
/// A course that names another race, a key RaceDay Connect would refuse or that two races
/// share, a distance that is not positive, a split the course does not place or places off
/// the lap, or two checkpoints that publish under one key.
pub fn manifest(races: &[(&RaceConfig, &Course)]) -> Result<Manifest, PublishError> {
    let mut keys = BTreeSet::new();
    let mut distances = Vec::with_capacity(races.len());
    for (config, course) in races {
        if !keys.insert(course.key.as_str()) {
            return Err(PublishError::DuplicateKey {
                key: course.key.clone(),
            });
        }
        let points = points(config, course)?;
        distances.push(Distance {
            key: course.key.clone(),
            name: config.race.name.clone(),
            meters: course.meters,
            gun_at: config.gun_time(),
            timing_points: in_course_order(points),
            laps: laps(config),
        });
    }
    Ok(Manifest { distances })
}

/// Every runner's crossings in one race, and the ones that cannot be published.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Crossings {
    /// Each runner's crossings, in the order they happened.
    pub runners: BTreeMap<RunnerRef, Vec<Crossing>>,
    /// Timing events left out, and why. For the operator's status line, not an error.
    pub skipped: Vec<Skipped>,
}

/// A timing event that was not published.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    /// The event.
    pub event: TimingEventId,
    /// Why.
    pub reason: SkipReason,
}

/// Why a timing event was not published.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// Over before the gun: a warm-up lap, or a runner lining up across the start mat.
    /// ADR-0029 counts these as lap 0, and they are not part of the race.
    BeforeTheGun,
    /// The participant is not in this race's roster.
    UnknownParticipant,
    /// The bib is longer than RaceDay Connect stores.
    BibTooLong,
}

/// Every runner's publishable crossings in one race.
///
/// `events` may hold other races' events; only this race's checkpoints are read.
///
/// # Errors
///
/// As [`manifest`], for this race's course.
pub fn crossings(
    config: &RaceConfig,
    course: &Course,
    events: &[TimingEvent],
) -> Result<Crossings, PublishError> {
    let points = points(config, course)?;
    let gun = config.gun_time();
    let roster: HashMap<ParticipantId, (&str, &str)> = config
        .participants
        .iter()
        .map(|p| (p.id, (p.bib.as_str(), p.name.as_str())))
        .collect();

    let mut out = Crossings::default();
    for event in events {
        // Another race's mat: not this distance's to publish.
        let Some(point) = points.get(&event.checkpoint) else {
            continue;
        };
        let skip = |reason| Skipped {
            event: event.id,
            reason,
        };
        if event.lap == 0 {
            out.skipped.push(skip(SkipReason::BeforeTheGun));
            continue;
        }
        let Some(&(bib, name)) = roster.get(&event.participant) else {
            out.skipped.push(skip(SkipReason::UnknownParticipant));
            continue;
        };
        if bib.chars().count() > MAX_BIB_LEN {
            out.skipped.push(skip(SkipReason::BibTooLong));
            continue;
        }
        let runner = RunnerRef {
            distance_key: course.key.clone(),
            bib: bib.to_owned(),
        };
        out.runners.entry(runner).or_default().push(Crossing {
            distance_key: course.key.clone(),
            point_key: point.point.key.clone(),
            bib: bib.to_owned(),
            name: (!name.trim().is_empty()).then(|| name.to_owned()),
            elapsed_ms: gun.and_then(|gun| elapsed_ms(gun, event.at)),
            crossed_at: event.at,
            lap: event.lap,
        });
    }
    for list in out.runners.values_mut() {
        list.sort_by(|a, b| {
            (a.crossed_at, a.lap, &a.point_key).cmp(&(b.crossed_at, b.lap, &b.point_key))
        });
    }
    Ok(out)
}

/// The runners whose crossings differ between what was last sent and what is true now,
/// including runners who have none any more.
#[must_use]
pub fn changed(
    sent: &BTreeMap<RunnerRef, Vec<Crossing>>,
    now: &BTreeMap<RunnerRef, Vec<Crossing>>,
) -> Vec<RunnerRef> {
    let mut out: Vec<RunnerRef> = now
        .iter()
        .filter(|(runner, crossings)| sent.get(*runner) != Some(*crossings))
        .map(|(runner, _)| runner.clone())
        .collect();
    out.extend(sent.keys().filter(|r| !now.contains_key(*r)).cloned());
    out.sort();
    out
}

/// Batches that restate each of `runners` in full, at most `max` crossings or runners each.
///
/// A runner is never split across two batches: the batch that names a runner in
/// `replaces` must carry all of that runner's crossings, or the second half would
/// retract the first.
#[must_use]
pub fn batches(
    now: &BTreeMap<RunnerRef, Vec<Crossing>>,
    runners: &[RunnerRef],
    max: usize,
) -> Vec<CrossingsBatch> {
    let max = max.max(1);
    let mut out = Vec::new();
    let mut batch = CrossingsBatch {
        crossings: Vec::new(),
        replaces: Vec::new(),
    };
    for runner in runners {
        let theirs = now.get(runner).map(Vec::as_slice).unwrap_or_default();
        let full = batch.replaces.len() >= max || batch.crossings.len() + theirs.len() > max;
        if full && !batch.replaces.is_empty() {
            out.push(std::mem::replace(
                &mut batch,
                CrossingsBatch {
                    crossings: Vec::new(),
                    replaces: Vec::new(),
                },
            ));
        }
        batch.replaces.push(runner.clone());
        batch.crossings.extend_from_slice(theirs);
    }
    if !batch.replaces.is_empty() {
        out.push(batch);
    }
    out
}

/// One published revision, as a RaceDay Connect revision of this race's distance.
///
/// Splits are each runner's intermediate crossings, measured from the same start their
/// result is — their own start-line read under chip timing, the gun otherwise — so the
/// finisher page's segments add up to the time it shows. On a lap course a mat is listed
/// once, at the last lap it read the runner.
///
/// A runner with no name in the roster is published as `Bib <n>`: RaceDay Connect needs a
/// name on every row, and refusing the revision would withhold everyone's result over one
/// blank field.
///
/// # Errors
///
/// As [`manifest`]; and a revision too large for one request, or a bib RaceDay Connect
/// cannot store, either of which would have it refuse the whole revision.
pub fn revision(
    config: &RaceConfig,
    course: &Course,
    revision: &ResultRevision,
    events: &[TimingEvent],
) -> Result<ResultsRevision, PublishError> {
    if revision.race != course.race {
        return Err(PublishError::CourseMismatch {
            race: revision.race,
            course: course.race,
        });
    }
    let points = points(config, course)?;
    if revision.entries.len() > MAX_ENTRIES_PER_REVISION {
        return Err(PublishError::TooManyEntries {
            count: revision.entries.len(),
        });
    }
    let events: HashMap<TimingEventId, &TimingEvent> = events.iter().map(|e| (e.id, e)).collect();

    let mut entries = Vec::with_capacity(revision.entries.len());
    for entry in &revision.entries {
        let bib = entry.bib.as_str();
        if bib.chars().count() > MAX_BIB_LEN {
            return Err(PublishError::BibTooLong {
                bib: bib.to_owned(),
            });
        }
        let basis = match revision.policy.start_mode {
            StartMode::Chip => entry.start_at.or(revision.policy.gun_time),
            StartMode::Gun => revision.policy.gun_time,
        };
        entries.push(ResultRow {
            distance_key: course.key.clone(),
            bib: bib.to_owned(),
            name: if entry.name.trim().is_empty() {
                format!("Bib {bib}")
            } else {
                entry.name.clone()
            },
            status: entry.status.as_str().to_owned(),
            overall_place: entry.place,
            gun_ms: entry.gun_time_ms,
            chip_ms: entry.chip_time_ms,
            splits: basis
                .map(|basis| splits(entry, basis, &points, &events))
                .unwrap_or_default(),
        });
    }

    Ok(ResultsRevision {
        revision: revision.number,
        status: revision.status.as_str().to_owned(),
        note: (!revision.reason.trim().is_empty()).then(|| revision.reason.clone()),
        published_at: revision.generated_at,
        distance_key: course.key.clone(),
        digest: revision.digest(),
        entries,
    })
}

/// A mat, where it sits, and whether it is an intermediate split.
#[derive(Debug, Clone)]
struct Placed {
    point: TimingPoint,
    kind: CheckpointKind,
    sequence: u16,
}

/// Along the course, and by the operator's sequence where two mats share a position.
fn in_course_order(points: BTreeMap<CheckpointId, Placed>) -> Vec<TimingPoint> {
    let mut points: Vec<Placed> = points.into_values().collect();
    points.sort_by(|a, b| {
        a.point
            .meters
            .total_cmp(&b.point.meters)
            .then(a.sequence.cmp(&b.sequence))
    });
    points.into_iter().map(|p| p.point).collect()
}

/// This race's mats, placed on the course.
fn points(
    config: &RaceConfig,
    course: &Course,
) -> Result<BTreeMap<CheckpointId, Placed>, PublishError> {
    let race = config.race.id;
    if course.race != race {
        return Err(PublishError::CourseMismatch {
            race,
            course: course.race,
        });
    }
    let key_len = course.key.chars().count();
    if course.key.trim() != course.key || key_len == 0 || key_len > MAX_KEY_LEN {
        return Err(PublishError::InvalidKey {
            key: course.key.clone(),
        });
    }
    if !(course.meters.is_finite() && course.meters > 0.0) {
        return Err(PublishError::NoDistance { race });
    }
    let lap = course.meters / f64::from(laps(config));

    let mut by_key: HashMap<String, &Checkpoint> = HashMap::new();
    let mut out = BTreeMap::new();
    for checkpoint in &config.checkpoints {
        let meters = match checkpoint.kind {
            CheckpointKind::Start => 0.0,
            CheckpointKind::Finish | CheckpointKind::Lap => lap,
            CheckpointKind::Split => {
                let Some(&meters) = course.splits.get(&checkpoint.name) else {
                    return Err(PublishError::SplitUnplaced {
                        race,
                        checkpoint: checkpoint.name.clone(),
                    });
                };
                if !(meters.is_finite() && (0.0..=lap).contains(&meters)) {
                    return Err(PublishError::SplitOffCourse {
                        race,
                        checkpoint: checkpoint.name.clone(),
                        meters,
                        lap,
                    });
                }
                meters
            }
        };
        let key = point_key(&checkpoint.name);
        if let Some(first) = by_key.insert(key.clone(), checkpoint) {
            return Err(PublishError::PointKeyCollision {
                race,
                first: first.name.clone(),
                second: checkpoint.name.clone(),
                key,
            });
        }
        out.insert(
            checkpoint.id,
            Placed {
                point: TimingPoint {
                    key,
                    name: checkpoint.name.clone(),
                    meters,
                },
                kind: checkpoint.kind,
                sequence: checkpoint.sequence,
            },
        );
    }
    Ok(out)
}

fn laps(config: &RaceConfig) -> u16 {
    config.race.expected_laps.max(1)
}

/// Milliseconds from `from` to `to`, floored at zero: a lap-1 crossing can begin a moment
/// before the gun (ADR-0029), and RaceDay Connect refuses a negative time.
fn elapsed_ms(from: OffsetDateTime, to: OffsetDateTime) -> Option<i64> {
    i64::try_from((to - from).whole_milliseconds().max(0)).ok()
}

fn splits(
    entry: &ResultEntry,
    basis: OffsetDateTime,
    points: &BTreeMap<CheckpointId, Placed>,
    events: &HashMap<TimingEventId, &TimingEvent>,
) -> Vec<Split> {
    // One per mat, at the last lap it read the runner.
    let mut last: BTreeMap<CheckpointId, &TimingEvent> = BTreeMap::new();
    for id in &entry.timing_events {
        let Some(&event) = events.get(id) else {
            continue;
        };
        let Some(placed) = points.get(&event.checkpoint) else {
            continue;
        };
        if placed.kind != CheckpointKind::Split || event.lap == 0 {
            continue;
        }
        let keep = last
            .get(&event.checkpoint)
            .is_none_or(|held| (event.lap, event.at) > (held.lap, held.at));
        if keep {
            last.insert(event.checkpoint, event);
        }
    }
    let mut out: Vec<(f64, Split)> = last
        .into_iter()
        .filter_map(|(checkpoint, event)| {
            let placed = points.get(&checkpoint)?;
            Some((
                placed.point.meters,
                Split {
                    point_key: placed.point.key.clone(),
                    elapsed_ms: elapsed_ms(basis, event.at)?,
                },
            ))
        })
        .collect();
    out.sort_by(|a, b| {
        a.0.total_cmp(&b.0)
            .then_with(|| a.1.point_key.cmp(&b.1.point_key))
    });
    out.into_iter().map(|(_, split)| split).collect()
}

#[cfg(test)]
mod tests;
