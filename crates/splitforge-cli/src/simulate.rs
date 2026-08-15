//! The read path, end to end, with a synthetic reader on the front of it.
//!
//! This loop is the shape the production edge service will have in Milestone 3, with an
//! LLRP adapter in place of the simulator and a real clock in place of [`Transport`]. The
//! ordering constraint is the part that must not change:
//!
//! > **The journal write completes before anything else is told the read exists.**
//!
//! If the process dies between the append and whatever comes next, the read survives and
//! derivation recomputes it on restart. Reordered, a crash could produce a timing event
//! with no evidence behind it — see `docs/architecture.md` § 3.

use anyhow::{Context, Result};
use splitforge_domain::{DeviceClockState, RawReadJournal};
use splitforge_reader::{Ingest, ReaderProvider};
use splitforge_simulator::{BurstProfile, Scenario, SimulatedReader, Transport, detection_time};
use splitforge_storage::SqliteJournal;
use splitforge_testkit::FixtureEvent;

use crate::cli::Speed;
use crate::report::SimulationReport;

/// The clock state a simulated device reports.
///
/// A GPS-disciplined Pi is the deployment this project recommends, so it is what the
/// simulation assumes. Degraded clock states are exercised by unit tests in
/// `splitforge-reader`, not by pretending the fixture hardware is broken.
const SIMULATED_CLOCK_STATE: DeviceClockState = DeviceClockState::GpsLocked;

/// Runs a fixture event into a journal.
///
/// # Errors
///
/// Returns an error if any read cannot be made durable. That is fatal on purpose: a timer
/// that cannot persist evidence must stop and say so, not keep counting.
pub async fn into_journal(
    fixture: &FixtureEvent,
    journal: &mut SqliteJournal,
    seed: u64,
    speed: Speed,
) -> Result<SimulationReport> {
    let reader_id = fixture
        .crossings
        .first()
        .map(|crossing| crossing.reader.clone())
        .context("the fixture plans no crossings")?;

    let mut scenario = Scenario::new(reader_id.clone(), BurstProfile::default(), seed);
    for crossing in &fixture.crossings {
        scenario = scenario.crossing(crossing.chip.clone(), crossing.antenna, crossing.at);
    }

    let provider = SimulatedReader::new(&scenario, speed.into());
    let reads_scripted = provider.scripted_reads();

    let ingest = Ingest::default();
    // Seeded from the scenario so that transport delay is part of the same reproducible
    // race. Offset so the two streams do not share a sequence of random values.
    let mut transport = Transport::new(seed ^ 0xA5A5_A5A5_A5A5_A5A5);

    let mut receiver = Box::new(provider).start();
    let mut reads_received = 0_usize;
    let mut reads_persisted = 0_usize;
    let mut first_seq = None;
    let mut last_seq = None;

    while let Some(message) = receiver.recv().await {
        reads_received += 1;

        // A reader that supplies no timestamp still has to be given a receipt time. Using
        // the previous receipt keeps the simulation reproducible; the real device clock
        // takes over here in Milestone 3.
        let sent_at = detection_time(&message).unwrap_or_else(|| {
            fixture
                .race
                .scheduled_start
                .unwrap_or(splitforge_testkit::RACE_START)
        });
        let receipt = transport.receipt(sent_at);

        let read = ingest.normalize(
            message,
            receipt.received_at,
            Some(receipt.monotonic_ns),
            SIMULATED_CLOCK_STATE,
        );

        let stored = journal
            .append(&read)
            .with_context(|| format!("appending read {} to the journal", read.id))?;

        reads_persisted += 1;
        first_seq.get_or_insert(stored.seq);
        last_seq = Some(stored.seq);
    }

    Ok(SimulationReport {
        fixture: fixture.slug.to_owned(),
        reader: reader_id,
        seed,
        planned_crossings: fixture.crossings.len(),
        reads_scripted,
        reads_received,
        reads_persisted,
        journal_total: journal.count()?,
        first_seq,
        last_seq,
    })
}
