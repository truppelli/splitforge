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
//!
//! ## Where the pieces come from now
//!
//! The *crossing plan* — who crosses which antenna when — comes from a fixture, because
//! inventing a plausible race is what the testkit is for. Everything else (the reader, its
//! trust setting, which antenna covers which checkpoint) comes from the **database**, which
//! is the Milestone 2 change: the simulator now runs against configuration an operator
//! could have typed, rather than configuration compiled into the binary.

use anyhow::{Context, Result, anyhow, bail};
use splitforge_domain::{DeviceClockState, RaceConfig, RawReadJournal, ReaderId};
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

/// A simulated reader built against a database's configuration, ready to be started.
///
/// Two callers compose one and drain it differently. `splitforge simulate` drains it with a
/// scripted clock, so a run is reproducible byte for byte; `splitforge-edge` drains it with
/// the device's own clock and its own measured [`splitforge_domain::DeviceClockState`],
/// because a service writes evidence rather than a fixture. What they must not do is
/// disagree about *which* reader a scenario emits from or *how* the operator configured it,
/// which is why that part is built here once.
#[derive(Debug)]
pub struct ScriptedReader {
    /// The reader itself.
    pub provider: SimulatedReader,
    /// How its reads are normalized — the operator's trust setting, from the database.
    pub ingest: Ingest,
    /// Which reader the scenario emits from.
    pub reader: ReaderId,
    /// How many crossings the scenario plans, which is not how many reads it emits — one
    /// crossing produces a burst, and reducing a burst to one accepted read is what
    /// derivation is for.
    pub planned_crossings: usize,
}

/// Builds a scenario's reader against the configuration a race actually has.
///
/// # Errors
///
/// Returns an error if the scenario is unknown, plans no crossings, or emits from a reader
/// the race has not configured.
pub fn scripted_reader(
    config: &RaceConfig,
    scenario_slug: &str,
    seed: u64,
    speed: Speed,
) -> Result<ScriptedReader> {
    let fixture = FixtureEvent::by_slug(scenario_slug).ok_or_else(|| {
        anyhow!(
            "unknown scenario {scenario_slug:?}; known scenarios: {}",
            FixtureEvent::slugs().join(", ")
        )
    })?;

    let reader_id = fixture
        .crossings
        .first()
        .map(|crossing| crossing.reader.clone())
        .context("the scenario plans no crossings")?;

    // The scenario names a reader; the database has to agree it exists. Otherwise the run
    // would write reads that derivation then rejects as unmapped, and the operator would be
    // left comparing two counts with no explanation for the gap.
    if !config.readers.iter().any(|reader| reader.id == reader_id) {
        let known: Vec<&str> = config
            .readers
            .iter()
            .map(|reader| reader.id.as_str())
            .collect();
        bail!(
            "scenario {scenario_slug:?} emits from reader {:?}, which race {:?} has not \
             configured. Configured readers: {}. Run `splitforge fixture load --fixture \
             {scenario_slug}`, or add the reader with `splitforge reader add`.",
            reader_id.as_str(),
            config.race.name,
            if known.is_empty() {
                "none".to_owned()
            } else {
                known.join(", ")
            }
        );
    }

    let mut scenario = Scenario::new(reader_id.clone(), BurstProfile::default(), seed);
    for crossing in &fixture.crossings {
        scenario = scenario.crossing(crossing.chip.clone(), crossing.antenna, crossing.at);
    }

    Ok(ScriptedReader {
        provider: SimulatedReader::new(&scenario, speed.into()),
        // The trust setting is the operator's, read back from the database — the same value
        // a real reader will be ingested under in Milestone 3.
        ingest: Ingest {
            trust: config
                .readers
                .iter()
                .find(|reader| reader.id == reader_id)
                .map(|reader| reader.timestamp_trust)
                .unwrap_or_default(),
            ..Ingest::default()
        },
        reader: reader_id,
        planned_crossings: fixture.crossings.len(),
    })
}

/// Runs a scenario's crossings into a journal, against configuration from the database.
///
/// # Errors
///
/// Returns an error if the scenario is unknown, the race has no reader configured, or any
/// read cannot be made durable. The last is fatal on purpose: a timer that cannot persist
/// evidence must stop and say so, not keep counting.
pub async fn into_journal(
    config: &RaceConfig,
    scenario_slug: &str,
    journal: &mut SqliteJournal,
    seed: u64,
    speed: Speed,
) -> Result<SimulationReport> {
    let ScriptedReader {
        provider,
        ingest,
        reader: reader_id,
        planned_crossings,
    } = scripted_reader(config, scenario_slug, seed, speed)?;
    let reads_scripted = provider.scripted_reads();

    // Seeded from the scenario so that transport delay is part of the same reproducible
    // race. Offset so the two streams do not share a sequence of random values.
    let mut transport = Transport::new(seed ^ 0xA5A5_A5A5_A5A5_A5A5);

    let mut receiver = Box::new(provider).start();
    let mut reads_received = 0_usize;
    let mut reads_persisted = 0_usize;
    let mut first_seq = None;
    let mut last_seq = None;

    let fallback = config.gun_time().unwrap_or(splitforge_testkit::RACE_START);

    while let Some(message) = receiver.recv().await {
        reads_received += 1;

        // A reader that supplies no timestamp still has to be given a receipt time. Using
        // the race's own start keeps the simulation reproducible; the real device clock
        // takes over here in Milestone 3.
        let sent_at = detection_time(&message).unwrap_or(fallback);
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
        scenario: scenario_slug.to_owned(),
        race: config.race.name.clone(),
        reader: reader_id,
        seed,
        planned_crossings,
        reads_scripted,
        reads_received,
        reads_persisted,
        journal_total: journal.count()?,
        first_seq,
        last_seq,
    })
}
