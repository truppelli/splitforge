//! # splitforge-cli
//!
//! Configuration, diagnostics, exports, backup/restore, and admin commands.
//!
//! ## Boundaries
//!
//! - **May depend on:** any crate except splitforge-llrp internals
//! - **Must never depend on:** nothing specific — but it is a boundary crate, so anyhow stops here
//!
//! These rules come from ADR-0001 and are tabulated in `docs/architecture.md`.
//! They are enforced by `crates/splitforge-testkit/tests/dependency_rules.rs`, not left to
//! review (ADR-0012).
//!
//! ## What this is in Milestone 1
//!
//! The vertical slice, and nothing more. It wires the four pieces that Milestone 1 exists
//! to prove together:
//!
//! ```text
//! simulator ──▶ reader::Ingest ──▶ SQLite journal ──▶ engine::derive ──▶ JSON
//!  (synthetic)   (normalize)        (durable,          (dedup,           (stdout)
//!                                    append-only)       chip→participant)
//! ```
//!
//! Every command reads its event configuration from a **fixture**, because Milestone 1
//! stores raw reads and nothing else. Rosters, checkpoints, and readers become database
//! records in Milestone 2 (`splitforge event create`, `roster import`, `reader add`), and
//! the `--fixture` flag goes away with them.
//!
//! Output is JSON on stdout, unconditionally. Human-readable tables are a Milestone 2
//! concern; being scriptable and diffable is a Milestone 1 requirement, because
//! "re-derive after a restart and compare" is the exit criterion.

mod cli;
mod report;
mod simulate;

use std::path::Path;

use anyhow::{Context, Result};
use splitforge_domain::RawReadJournal;
use splitforge_engine::{DerivationInput, derive};
use splitforge_storage::SqliteJournal;
use splitforge_testkit::FixtureEvent;

pub use cli::{Cli, Command, Format, Speed};
pub use report::{
    AcceptedReadView, DerivationReport, InitReport, RawReadView, SimulationReport, StatusReport,
};
pub use simulate::into_journal;

/// Runs one command.
///
/// # Errors
///
/// Returns an error if the database cannot be opened or migrated, the fixture is unknown,
/// or a read cannot be made durable.
pub async fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Init { database } => {
            let journal = open(&database)?;
            let report = InitReport {
                database: database.display().to_string(),
                schema_version: journal.schema_version()?,
                reads: journal.count()?,
            };
            emit(&report, cli.format)
        }
        Command::Simulate {
            database,
            fixture,
            seed,
            speed,
        } => {
            let fixture = load_fixture(&fixture)?;
            let mut journal = open(&database)?;
            let report = simulate::into_journal(&fixture, &mut journal, seed, speed).await?;
            emit(&report, cli.format)
        }
        Command::Reads {
            database,
            since,
            limit,
        } => {
            let journal = open(&database)?;
            let stored = match since {
                Some(seq) => journal.read_since(seq)?,
                None => journal.read_all()?,
            };
            let views: Vec<RawReadView> = stored
                .iter()
                .take(limit.unwrap_or(usize::MAX))
                .map(RawReadView::of)
                .collect();
            emit(&views, cli.format)
        }
        Command::Accepted { database, fixture } => {
            let report = derivation_report(&database, &fixture)?;
            emit(&report.accepted_reads, cli.format)
        }
        Command::Derive { database, fixture } => {
            let report = derivation_report(&database, &fixture)?;
            emit(&report, cli.format)
        }
        Command::Status { database, fixture } => {
            let journal = open(&database)?;
            let reads = journal.read_all()?;
            let report = StatusReport {
                database: database.display().to_string(),
                schema_version: journal.schema_version()?,
                raw_reads: reads.len(),
                first_read_at: reads
                    .first()
                    .map(|read| read.read.authoritative_timestamp()),
                last_read_at: reads.last().map(|read| read.read.authoritative_timestamp()),
                untrusted_clock_reads: reads
                    .iter()
                    .filter(|read| !read.read.device_clock_state.is_trustworthy())
                    .count(),
                device_timed_reads: reads
                    .iter()
                    .filter(|read| !read.read.is_reader_timed())
                    .count(),
                fixture: fixture.clone(),
            };
            emit(&report, cli.format)
        }
    }
}

/// Opens the journal, creating and migrating it if necessary.
fn open(database: &Path) -> Result<SqliteJournal> {
    SqliteJournal::open(database)
        .with_context(|| format!("opening journal at {}", database.display()))
}

/// Resolves a fixture slug, listing the alternatives rather than guessing.
fn load_fixture(slug: &str) -> Result<FixtureEvent> {
    FixtureEvent::by_slug(slug).with_context(|| {
        let known: Vec<&str> = FixtureEvent::all().iter().map(|f| f.slug).collect();
        format!(
            "unknown fixture {slug:?}; known fixtures: {}",
            known.join(", ")
        )
    })
}

/// Re-derives everything from the journal.
///
/// Nothing derived is stored: derivation is a pure function of the journal plus the policy,
/// so the honest thing is to recompute it on demand. That also means every invocation of
/// this command is a live test of the property the milestone is about.
fn derivation_report(database: &Path, slug: &str) -> Result<DerivationReport> {
    let fixture = load_fixture(slug)?;
    let journal = open(database)?;
    let reads = journal.read_all()?;
    let chips = fixture
        .chips()
        .context("fixture chip assignments overlap")?;

    let derivation = derive(&DerivationInput {
        reads: &reads,
        policy: &fixture.policy,
        chips: &chips,
        antennas: &fixture.antennas,
    });

    Ok(DerivationReport::build(&fixture, reads.len(), &derivation))
}

/// Writes a value to stdout as JSON.
fn emit<T: serde::Serialize>(value: &T, format: Format) -> Result<()> {
    let json = match format {
        Format::Pretty => serde_json::to_string_pretty(value),
        Format::Compact => serde_json::to_string(value),
    }
    .context("serializing output")?;
    println!("{json}");
    Ok(())
}
