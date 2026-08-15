//! Shared harness: run a fixture event into a real database, then derive from it.

// Each integration test file compiles this module separately and uses a different part of
// it, so "unused" and "unreachable" here mean "by one test binary", not at all.
#![allow(dead_code, unreachable_pub)]

use std::path::{Path, PathBuf};

use splitforge_cli::Speed;
use splitforge_domain::{
    AcceptedRead, Checkpoint, ChipId, Derivation, RawReadJournal, RejectionReason, StoredRawRead,
};
use splitforge_engine::{DerivationInput, derive};
use splitforge_storage::SqliteJournal;
use splitforge_testkit::{CrossingExpectation, FixtureEvent, PlannedCrossing};
use tempfile::TempDir;
use time::{Duration, OffsetDateTime};

/// Fixed so a failure is reproducible from the test name alone.
pub const SEED: u64 = 0x5F17_F03E;

/// How far a credited crossing may sit from the instant the fixture planned.
///
/// A burst is up to a couple of seconds wide and the `First` rule credits its earliest
/// read, so the credited time is legitimately earlier than closest approach. Wide enough
/// to allow that; far narrower than the gap between any two crossings of one chip.
pub const TOLERANCE: Duration = Duration::seconds(2);

/// One fixture event, simulated, journalled, and derived.
pub struct Slice {
    /// Kept alive so the database outlives the test body.
    _directory: TempDir,
    /// Path to the database.
    pub database: PathBuf,
    /// The fixture that was run.
    pub fixture: FixtureEvent,
    /// Every raw read, in journal order.
    pub reads: Vec<StoredRawRead>,
    /// What the engine concluded.
    pub derivation: Derivation,
}

/// Runs a fixture end to end.
pub async fn run(fixture: FixtureEvent) -> Slice {
    let directory = TempDir::new().expect("temp dir");
    let database = directory.path().join("event.db");

    let mut journal = SqliteJournal::open(&database).expect("open journal");
    let report = splitforge_cli::into_journal(&fixture, &mut journal, SEED, Speed::Immediate)
        .await
        .expect("simulation must not lose a read");

    assert_eq!(
        report.reads_received, report.reads_scripted,
        "the pipeline dropped reads between the reader and the journal"
    );
    assert_eq!(
        report.reads_persisted, report.reads_received,
        "a read was received but never made durable"
    );
    assert_eq!(report.journal_total as usize, report.reads_persisted);

    let reads = journal.read_all().expect("read journal");
    let derivation = derive_from(&fixture, &reads);
    Slice {
        _directory: directory,
        database,
        fixture,
        reads,
        derivation,
    }
}

/// Derives from an explicit set of reads. Used to re-derive after a reopen.
pub fn derive_from(fixture: &FixtureEvent, reads: &[StoredRawRead]) -> Derivation {
    let chips = fixture.chips().expect("fixture assignments");
    derive(&DerivationInput {
        reads,
        policy: &fixture.policy,
        chips: &chips,
        antennas: &fixture.antennas,
    })
}

/// Opens an existing database and reads every row.
pub fn reopen(database: &Path) -> Vec<StoredRawRead> {
    SqliteJournal::open(database)
        .expect("reopen journal")
        .read_all()
        .expect("read journal")
}

impl Slice {
    /// The crossings credited to one chip at one checkpoint, in time order.
    pub fn crossings_of(&self, chip: &ChipId, checkpoint: &Checkpoint) -> Vec<&AcceptedRead> {
        self.derivation
            .accepted
            .iter()
            .filter(|crossing| crossing.chip == *chip && crossing.checkpoint == checkpoint.id)
            .collect()
    }

    /// The reasons given for suppressing reads within `TOLERANCE` of an instant.
    pub fn rejections_near(&self, chip: &ChipId, at: OffsetDateTime) -> Vec<&RejectionReason> {
        let ids: Vec<_> = self
            .reads
            .iter()
            .filter(|stored| {
                stored.read.chip == *chip
                    && (stored.read.authoritative_timestamp() - at).abs() <= TOLERANCE
            })
            .map(|stored| stored.read.id)
            .collect();
        self.derivation
            .rejected
            .iter()
            .filter(|entry| ids.contains(&entry.raw_read))
            .map(|entry| &entry.reason)
            .collect()
    }

    /// Asserts that one planned crossing produced exactly what the fixture said it would.
    pub fn assert_matches(&self, planned: &PlannedCrossing) {
        let near: Vec<&AcceptedRead> = self
            .derivation
            .accepted
            .iter()
            .filter(|crossing| {
                crossing.chip == planned.chip
                    && crossing.checkpoint == planned.checkpoint
                    && (crossing.at - planned.at).abs() <= TOLERANCE
            })
            .collect();

        match planned.expectation {
            CrossingExpectation::Accepted => {
                assert_eq!(
                    near.len(),
                    1,
                    "chip {} at {} produced {} crossings, expected exactly one",
                    planned.chip,
                    planned.at,
                    near.len()
                );
                assert!(
                    near[0].participant.is_some(),
                    "chip {} is on the roster but was not credited to anybody",
                    planned.chip
                );
                assert!(
                    near[0].burst_size() > 1,
                    "a crossing must be backed by the whole burst, not one read"
                );
            }
            CrossingExpectation::AcceptedUnassigned => {
                assert_eq!(near.len(), 1, "chip {} produced no crossing", planned.chip);
                assert_eq!(
                    near[0].participant, None,
                    "chip {} is not on the roster and must not be credited to anybody",
                    planned.chip
                );
            }
            CrossingExpectation::SuppressedAsReRead => {
                assert!(
                    near.is_empty(),
                    "a re-read at the mat was credited as a lap: {near:?}"
                );
                let reasons = self.rejections_near(&planned.chip, planned.at);
                assert!(
                    reasons
                        .iter()
                        .any(|reason| matches!(reason, RejectionReason::BelowMinLap { .. })),
                    "the re-read was suppressed, but not by the rule that should have done it: \
                     {reasons:?}"
                );
            }
        }
    }
}
