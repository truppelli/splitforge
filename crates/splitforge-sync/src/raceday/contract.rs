//! The wire format RaceDay Connect's timing endpoints read.
//!
//! RaceDay Connect is the consumer and owns this shape; it is written down there as the
//! contract table in `docs/architecture/20-raceday-connect.md` of the SmartSponsor
//! repository, and enforced by `TimingIngestController`. Field names are camelCase because
//! that is what its JSON binder reads. The tests at the bottom of this file pin the exact
//! names, so a rename here fails a test rather than a race morning.
//!
//! | Endpoint | Body | Answer |
//! |---|---|---|
//! | `POST /api/v1/timing/pair` | [`PairRequest`] | [`PairResponse`] |
//! | `PUT /api/v1/timing/races/{id}/manifest` | [`Manifest`] | [`IngestAck`] |
//! | `POST /api/v1/timing/races/{id}/crossings` | [`CrossingsBatch`] | [`IngestAck`] |
//! | `POST /api/v1/timing/races/{id}/results` | [`ResultsRevision`] | [`IngestAck`] |
//!
//! Every request but pairing carries `Authorization: Bearer <token>`, the token pairing
//! returned. A token is scoped to one RaceDay Connect race, which may hold several
//! SplitForge races as its distances.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// Most crossings RaceDay Connect accepts in one batch.
pub const MAX_CROSSINGS_PER_BATCH: usize = 5000;

/// Most rows RaceDay Connect accepts in one revision.
pub const MAX_ENTRIES_PER_REVISION: usize = 50_000;

/// Longest bib RaceDay Connect stores.
pub const MAX_BIB_LEN: usize = 20;

/// Longest distance key RaceDay Connect accepts.
pub const MAX_KEY_LEN: usize = 40;

/// Trades the code a race director read out for this box's token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairRequest {
    /// The pairing code, e.g. `K7QM-4XPD`. Case and separators do not matter.
    pub code: String,
    /// What the race page calls this box.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_name: Option<String>,
}

/// What pairing returns. The token is shown once and only its hash is kept remotely.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairResponse {
    /// The RaceDay Connect race every later request names.
    pub race_id: String,
    /// The race's name, for the operator to confirm they paired the right one.
    pub race_name: String,
    /// The race's public address.
    pub slug: String,
    /// The bearer token.
    pub token: String,
}

/// The whole course: every distance, as one document. A send replaces the last one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    /// One per SplitForge race.
    pub distances: Vec<Distance>,
}

/// One distance, which is one SplitForge race.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Distance {
    /// Stable key that crossings and revisions name. At most [`MAX_KEY_LEN`] characters.
    pub key: String,
    /// What the page calls it.
    pub name: String,
    /// Certified distance in metres, for pace and the progress rail.
    pub meters: f64,
    /// The gun, when there is one. The live board derives elapsed time from it.
    #[serde(with = "time::serde::rfc3339::option")]
    pub gun_at: Option<OffsetDateTime>,
    /// Where the mats are.
    pub timing_points: Vec<TimingPoint>,
    /// Laps of the course. `1` for point to point.
    pub laps: u16,
}

/// One mat.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimingPoint {
    /// Stable key, unique within its distance.
    pub key: String,
    /// What the page calls it.
    pub name: String,
    /// How far along one lap it sits.
    pub meters: f64,
}

/// A batch of live crossings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrossingsBatch {
    /// Crossings to hold. Each replaces any held for the same runner, mat and lap.
    pub crossings: Vec<Crossing>,
    /// Runners this batch states in full: each keeps only the crossings sent here.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub replaces: Vec<RunnerRef>,
}

/// One runner crossing one mat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Crossing {
    /// The distance, as the manifest keys it.
    pub distance_key: String,
    /// The mat, as the manifest keys it.
    pub point_key: String,
    /// The runner.
    pub bib: String,
    /// The runner's name, when the roster has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Milliseconds since the gun, when one is known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<i64>,
    /// The wall-clock instant of the crossing.
    #[serde(with = "time::serde::rfc3339")]
    pub crossed_at: OffsetDateTime,
    /// Which lap, from 1.
    pub lap: u16,
}

/// One runner in one distance.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunnerRef {
    /// The distance.
    pub distance_key: String,
    /// The runner.
    pub bib: String,
}

/// One result revision of one distance, whole.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResultsRevision {
    /// The revision number, from 1, per distance.
    pub revision: u32,
    /// `provisional` or `final`.
    pub status: String,
    /// Why it was published.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// When SplitForge generated it.
    #[serde(with = "time::serde::rfc3339")]
    pub published_at: OffsetDateTime,
    /// The distance it scores.
    pub distance_key: String,
    /// SplitForge's content digest: the same number with another digest is refused.
    pub digest: String,
    /// One row per runner.
    pub entries: Vec<ResultRow>,
}

/// One runner's result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResultRow {
    /// The distance, which must be the revision's.
    pub distance_key: String,
    /// The runner.
    pub bib: String,
    /// The runner's name. Required by RaceDay Connect.
    pub name: String,
    /// `finished`, `dnf`, `dns` or `dq`.
    pub status: String,
    /// Overall place, for a finisher.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overall_place: Option<u32>,
    /// Time from the gun.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gun_ms: Option<i64>,
    /// Time from the runner's own start.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chip_ms: Option<i64>,
    /// Intermediate times, in course order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub splits: Vec<Split>,
}

/// One intermediate time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Split {
    /// The mat.
    pub point_key: String,
    /// Milliseconds from the same start the result's time is measured from.
    pub elapsed_ms: i64,
}

/// RaceDay Connect's answer to every write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IngestAck {
    /// How many items were stored.
    pub accepted: u32,
    /// The send was already held and changed nothing.
    #[serde(default)]
    pub replayed: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use time::macros::datetime;

    // These pin the field names RaceDay Connect binds. Each literal below is what its
    // RaceDayApiTests send, so a mismatch here is a mismatch there.

    #[test]
    fn a_manifest_serializes_to_the_names_raceday_connect_reads() {
        let manifest = Manifest {
            distances: vec![Distance {
                key: "5k".to_owned(),
                name: "5K".to_owned(),
                meters: 5000.0,
                gun_at: Some(datetime!(2026-10-11 11:45 UTC)),
                timing_points: vec![TimingPoint {
                    key: "finish".to_owned(),
                    name: "Finish".to_owned(),
                    meters: 2500.0,
                }],
                laps: 2,
            }],
        };
        assert_eq!(
            serde_json::to_value(&manifest).unwrap(),
            json!({
                "distances": [{
                    "key": "5k",
                    "name": "5K",
                    "meters": 5000.0,
                    "gunAt": "2026-10-11T11:45:00Z",
                    "timingPoints": [{ "key": "finish", "name": "Finish", "meters": 2500.0 }],
                    "laps": 2
                }]
            })
        );
    }

    #[test]
    fn a_crossing_batch_serializes_to_the_names_raceday_connect_reads() {
        let batch = CrossingsBatch {
            crossings: vec![Crossing {
                distance_key: "5k".to_owned(),
                point_key: "finish".to_owned(),
                bib: "501".to_owned(),
                name: Some("Runner 501".to_owned()),
                elapsed_ms: None,
                crossed_at: datetime!(2026-10-11 11:55 UTC),
                lap: 1,
            }],
            replaces: vec![RunnerRef {
                distance_key: "5k".to_owned(),
                bib: "501".to_owned(),
            }],
        };
        assert_eq!(
            serde_json::to_value(&batch).unwrap(),
            json!({
                "crossings": [{
                    "distanceKey": "5k",
                    "pointKey": "finish",
                    "bib": "501",
                    "name": "Runner 501",
                    "crossedAt": "2026-10-11T11:55:00Z",
                    "lap": 1
                }],
                "replaces": [{ "distanceKey": "5k", "bib": "501" }]
            })
        );
    }

    #[test]
    fn a_batch_with_nothing_to_replace_is_the_shape_every_older_sender_used() {
        let batch = CrossingsBatch {
            crossings: vec![],
            replaces: vec![],
        };
        assert_eq!(
            serde_json::to_value(&batch).unwrap(),
            json!({ "crossings": [] })
        );
    }

    #[test]
    fn a_revision_serializes_to_the_names_raceday_connect_reads() {
        let revision = ResultsRevision {
            revision: 2,
            status: "final".to_owned(),
            note: Some("DQ bib 12".to_owned()),
            published_at: datetime!(2026-10-11 13:00 UTC),
            distance_key: "half".to_owned(),
            digest: "abc".to_owned(),
            entries: vec![ResultRow {
                distance_key: "half".to_owned(),
                bib: "12".to_owned(),
                name: "Tomas Brandt".to_owned(),
                status: "finished".to_owned(),
                overall_place: Some(1),
                gun_ms: Some(4_169_000),
                chip_ms: Some(4_160_000),
                splits: vec![Split {
                    point_key: "10k".to_owned(),
                    elapsed_ms: 1_990_000,
                }],
            }],
        };
        assert_eq!(
            serde_json::to_value(&revision).unwrap(),
            json!({
                "revision": 2,
                "status": "final",
                "note": "DQ bib 12",
                "publishedAt": "2026-10-11T13:00:00Z",
                "distanceKey": "half",
                "digest": "abc",
                "entries": [{
                    "distanceKey": "half",
                    "bib": "12",
                    "name": "Tomas Brandt",
                    "status": "finished",
                    "overallPlace": 1,
                    "gunMs": 4_169_000,
                    "chipMs": 4_160_000,
                    "splits": [{ "pointKey": "10k", "elapsedMs": 1_990_000 }]
                }]
            })
        );
    }

    #[test]
    fn the_answers_raceday_connect_sends_parse() {
        let pair: PairResponse = serde_json::from_value(json!({
            "raceId": "6f1c1d1e-0000-0000-0000-000000000001",
            "raceName": "Harbor Point Half & 5K",
            "slug": "harbor-point-2026",
            "token": "rdc_abc"
        }))
        .unwrap();
        assert_eq!(pair.token, "rdc_abc");

        let ack: IngestAck = serde_json::from_value(json!({ "accepted": 3 })).unwrap();
        assert_eq!(
            ack,
            IngestAck {
                accepted: 3,
                replayed: false
            }
        );
    }
}
