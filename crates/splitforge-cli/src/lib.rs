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
//! ## What this is in Milestone 2
//!
//! The local event console: the minimum vocabulary that lets a race be configured and
//! operated end to end without ever opening SQLite by hand.
//!
//! ```text
//! event create ─▶ race create ─▶ checkpoint add ─▶ reader add / map
//!                                     │
//!                 roster import ──────┼────── chips import
//!                                     ▼
//!                              policy set ─▶ race start
//!                                     │
//!  simulator ──▶ reader::Ingest ──▶ SQLite journal ──▶ engine::derive ──▶ export
//!   (synthetic)   (normalize)       (durable,           (dedup,           (csv/json)
//!                                    append-only)        chip→participant)
//! ```
//!
//! Configuration now lives in the database. `accepted`, `derive`, `export`, and `status`
//! load a [`RaceConfig`](splitforge_domain::RaceConfig) from SQLite rather than from a
//! compiled-in fixture, which is what makes the Milestone 2 exit criterion checkable at
//! all: if a command could still fall back to a fixture, "configured end to end" would
//! never actually be tested.
//!
//! Output is JSON on stdout, unconditionally, except for CSV exports. Being scriptable and
//! diffable is what lets "re-derive after a restart and compare" be an assertion.

mod cli;
mod configure;
mod operate;
mod report;
mod simulate;

use std::path::Path;

use anyhow::{Context, Result, bail};
use splitforge_domain::{RaceConfig, RawReadJournal, SessionAction};
use splitforge_engine::{DerivationInput, derive};
use splitforge_storage::{ConfigStore, SqliteJournal};

pub use cli::{
    BackupCommand, CheckpointCommand, ChipsCommand, Cli, Command, EventCommand, ExportCommand,
    ExportFormat, FixtureCommand, Format, PolicyCommand, RaceCommand, RaceRef, ReaderCommand,
    RosterCommand, Speed,
};
pub use configure::load_fixture;
pub use report::{
    AcceptedReadView, AssignmentView, AuditView, CheckpointView, DerivationReport, DoctorReport,
    EventView, Finding, ImportReport, InitReport, ParticipantView, PolicyView, RaceView,
    RawReadView, ReaderStatusView, SessionView, SimulationReport, StatusReport, reader_status,
};
pub use simulate::into_journal;

/// Runs one command.
///
/// # Errors
///
/// Returns an error if the database cannot be opened or migrated, the requested race cannot
/// be resolved, or a read cannot be made durable.
#[allow(clippy::too_many_lines)] // A command dispatcher. Splitting it would only move the match arms.
pub async fn run(cli: Cli) -> Result<()> {
    let database = cli.database.clone();
    let format = cli.format;
    let actor = cli.actor.clone();

    match cli.command {
        Command::Init => {
            let journal = open_journal(&database)?;
            let store = open_store(&database)?;
            emit(
                &InitReport {
                    database: database.display().to_string(),
                    schema_version: journal.schema_version()?,
                    reads: journal.count()?,
                    races: store.races()?.len(),
                },
                format,
            )
        }

        Command::Event(command) => {
            let mut store = open_store(&database)?;
            match command {
                EventCommand::Create { name } => {
                    let event = configure::create_event(&mut store, &actor, &name)?;
                    emit(&EventView::of(&event), format)
                }
                EventCommand::List => {
                    let views: Vec<EventView> = store.events()?.iter().map(EventView::of).collect();
                    emit(&views, format)
                }
            }
        }

        Command::Race(command) => {
            let mut store = open_store(&database)?;
            match command {
                RaceCommand::Create {
                    event,
                    name,
                    start,
                    laps,
                } => {
                    let event = configure::resolve_event(&store, event.as_deref())?;
                    let race = configure::create_race(
                        &mut store,
                        &actor,
                        &event,
                        &name,
                        start.as_deref(),
                        laps,
                    )?;
                    emit(&RaceView::of(&race), format)
                }
                RaceCommand::List => {
                    let views: Vec<RaceView> = store.races()?.iter().map(RaceView::of).collect();
                    emit(&views, format)
                }
                RaceCommand::Start { race, at, note } => record_session(
                    &mut store,
                    &actor,
                    race,
                    at,
                    note,
                    SessionAction::Start,
                    format,
                ),
                RaceCommand::Stop { race, at, note } => record_session(
                    &mut store,
                    &actor,
                    race,
                    at,
                    note,
                    SessionAction::Stop,
                    format,
                ),
            }
        }

        Command::Checkpoint(command) => {
            let mut store = open_store(&database)?;
            match command {
                CheckpointCommand::Add {
                    race,
                    name,
                    kind,
                    sequence,
                } => {
                    let race = configure::resolve_race(&store, race.race.as_deref())?;
                    let checkpoint = configure::add_checkpoint(
                        &mut store, &actor, &race, &name, &kind, sequence,
                    )?;
                    emit(&CheckpointView::of(&checkpoint), format)
                }
                CheckpointCommand::List { race } => {
                    let race = configure::resolve_race(&store, race.race.as_deref())?;
                    let views: Vec<CheckpointView> = store
                        .checkpoints(race.id)?
                        .iter()
                        .map(CheckpointView::of)
                        .collect();
                    emit(&views, format)
                }
            }
        }

        Command::Roster(command) => {
            let mut store = open_store(&database)?;
            match command {
                RosterCommand::Import { race, file } => {
                    let race = configure::resolve_race(&store, race.race.as_deref())?;
                    let summary = configure::import_roster(&mut store, &actor, &race, &file)?;
                    emit(
                        &ImportReport::of(&file.display().to_string(), &race.name, summary),
                        format,
                    )
                }
                RosterCommand::List { race } => {
                    let race = configure::resolve_race(&store, race.race.as_deref())?;
                    let assignments = store.assignments(race.id)?;
                    let views: Vec<ParticipantView> = store
                        .participants(race.id)?
                        .into_iter()
                        .map(|participant| ParticipantView {
                            chips: assignments
                                .iter()
                                .filter(|a| a.participant == participant.id)
                                .map(|a| a.chip.as_str().to_owned())
                                .collect(),
                            bib: participant.bib.as_str().to_owned(),
                            name: participant.name,
                            id: participant.id,
                        })
                        .collect();
                    emit(&views, format)
                }
            }
        }

        Command::Chips(command) => {
            let mut store = open_store(&database)?;
            match command {
                ChipsCommand::Import { race, file } => {
                    let race = configure::resolve_race(&store, race.race.as_deref())?;
                    let summary = configure::import_chips(&mut store, &actor, &race, &file)?;
                    emit(
                        &ImportReport::of(&file.display().to_string(), &race.name, summary),
                        format,
                    )
                }
                ChipsCommand::List { race } => {
                    let race = configure::resolve_race(&store, race.race.as_deref())?;
                    let roster = store.participants(race.id)?;
                    let views: Vec<AssignmentView> = store
                        .assignments(race.id)?
                        .into_iter()
                        .map(|assignment| AssignmentView {
                            bib: roster
                                .iter()
                                .find(|p| p.id == assignment.participant)
                                .map(|p| p.bib.as_str().to_owned()),
                            chip: assignment.chip.as_str().to_owned(),
                            valid_from: assignment.valid_from,
                            valid_until: assignment.valid_until,
                        })
                        .collect();
                    emit(&views, format)
                }
            }
        }

        Command::Reader(command) => {
            let mut store = open_store(&database)?;
            match command {
                ReaderCommand::Add {
                    id,
                    label,
                    endpoint,
                    trust,
                } => {
                    let reader = configure::add_reader(
                        &mut store,
                        &actor,
                        &id,
                        label.as_deref(),
                        endpoint.as_deref(),
                        &trust,
                    )?;
                    emit(&reader, format)
                }
                ReaderCommand::Map {
                    reader,
                    antenna,
                    checkpoint,
                    race,
                } => {
                    let race = configure::resolve_race(&store, race.race.as_deref())?;
                    configure::map_reader(
                        &mut store,
                        &actor,
                        &reader,
                        antenna,
                        &checkpoint,
                        &race,
                    )?;
                    emit(
                        &serde_json::json!({
                            "reader": reader,
                            "antenna": antenna,
                            "checkpoint": checkpoint,
                            "race": race.name,
                            "mappings": store.antenna_map()?.len(),
                        }),
                        format,
                    )
                }
                ReaderCommand::List => emit(&store.readers()?, format),
                ReaderCommand::Status { window_secs } => {
                    let journal = open_journal(&database)?;
                    let reads = journal.read_all()?;
                    let config = load_only_race_config(&store)?;
                    let views: Vec<ReaderStatusView> = store
                        .readers()?
                        .iter()
                        .map(|reader| report::reader_status(reader, &config, &reads, window_secs))
                        .collect();
                    emit(&views, format)
                }
            }
        }

        Command::Policy(command) => {
            let mut store = open_store(&database)?;
            match command {
                PolicyCommand::Set {
                    race,
                    checkpoint,
                    min_interval_ms,
                    min_lap_ms,
                    selection_rule,
                } => {
                    let race = configure::resolve_race(&store, race.race.as_deref())?;
                    let policy = configure::set_policy(
                        &mut store,
                        &actor,
                        &race,
                        checkpoint.as_deref(),
                        min_interval_ms,
                        min_lap_ms,
                        selection_rule.as_deref(),
                    )?;
                    emit(&policy_view(&store, &race, policy)?, format)
                }
                PolicyCommand::Show { race } => {
                    let race = configure::resolve_race(&store, race.race.as_deref())?;
                    let policy = store.policy(race.id)?;
                    emit(&policy_view(&store, &race, policy)?, format)
                }
            }
        }

        Command::Fixture(command) => {
            let mut store = open_store(&database)?;
            match command {
                FixtureCommand::Load { fixture } => {
                    let race = configure::load_fixture(&mut store, &actor, &fixture)?;
                    emit(&RaceView::of(&race), format)
                }
                FixtureCommand::List => emit(&splitforge_testkit::FixtureEvent::slugs(), format),
            }
        }

        Command::Simulate {
            race,
            scenario,
            seed,
            speed,
        } => {
            let store = open_store(&database)?;
            let config = load_config(&store, race.race.as_deref())?;
            let mut journal = open_journal(&database)?;
            let report =
                simulate::into_journal(&config, &scenario, &mut journal, seed, speed).await?;
            emit(&report, format)
        }

        Command::Reads {
            since,
            limit,
            follow,
            interval_ms,
        } => {
            let journal = open_journal(&database)?;
            operate::tail(&journal, since, limit, follow, interval_ms, format).await
        }

        Command::Accepted { race } => {
            let report = derivation_report(&database, race.race.as_deref())?;
            emit(&report.accepted_reads, format)
        }

        Command::Derive { race } => {
            let report = derivation_report(&database, race.race.as_deref())?;
            emit(&report, format)
        }

        Command::Export(ExportCommand::Crossings {
            race,
            shape,
            output,
        }) => {
            let store = open_store(&database)?;
            let config = load_config(&store, race.race.as_deref())?;
            let journal = open_journal(&database)?;
            operate::export_crossings(&config, &journal, shape, output.as_deref())?;
            Ok(())
        }

        Command::Backup(BackupCommand::Create { destination }) => {
            let mut store = open_store(&database)?;
            let report = splitforge_storage::create_backup(&store, &destination)?;
            store.record_audit(
                &actor,
                "backup.create",
                Some(&report.path),
                Some(&format!(r#"{{"raw_reads":{}}}"#, report.raw_reads)),
            )?;
            emit(
                &serde_json::json!({
                    "path": report.path,
                    "bytes": report.bytes,
                    "raw_reads": report.raw_reads,
                }),
                format,
            )
        }

        Command::Doctor { race } => {
            let store = open_store(&database)?;
            let journal = open_journal(&database)?;
            let configs = match race.race.as_deref() {
                Some(_) => vec![load_config(&store, race.race.as_deref())?],
                None => store
                    .races()?
                    .into_iter()
                    .map(|race| store.load(race.id))
                    .collect::<Result<Vec<_>, _>>()?,
            };
            let report = operate::doctor(&database, &store, &journal, &configs)?;
            let errors = report.errors;
            emit(&report, format)?;
            // A non-zero exit so `splitforge doctor && splitforge race start` is a usable
            // pre-race gate in a shell script.
            if errors > 0 {
                bail!("{errors} error(s) found; see the findings above");
            }
            Ok(())
        }

        Command::Audit { limit } => {
            let store = open_store(&database)?;
            let views: Vec<AuditView> = store
                .audit_trail(limit)?
                .iter()
                .map(AuditView::of)
                .collect();
            emit(&views, format)
        }

        Command::Status { race } => {
            let store = open_store(&database)?;
            let journal = open_journal(&database)?;
            let reads = journal.read_all()?;
            let config = match store.resolve_race(race.race.as_deref())? {
                splitforge_storage::RaceSelection::One(race) => Some(store.load(race.id)?),
                _ => None,
            };

            emit(
                &StatusReport {
                    database: database.display().to_string(),
                    schema_version: journal.schema_version()?,
                    race: config.as_ref().map(|c| c.race.name.clone()),
                    participants: config.as_ref().map_or(0, |c| c.participants.len()),
                    checkpoints: config.as_ref().map_or(0, |c| c.checkpoints.len()),
                    chip_assignments: config.as_ref().map_or(0, |c| c.assignments.len()),
                    readers: config.as_ref().map_or(0, |c| c.readers.len()),
                    antenna_mappings: config.as_ref().map_or(0, |c| c.antennas.len()),
                    gun_time: config.as_ref().and_then(RaceConfig::gun_time),
                    running: config.as_ref().is_some_and(RaceConfig::is_running),
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
                },
                format,
            )
        }
    }
}

/// Opens the journal, creating and migrating it if necessary.
fn open_journal(database: &Path) -> Result<SqliteJournal> {
    SqliteJournal::open(database)
        .with_context(|| format!("opening journal at {}", database.display()))
}

/// Opens the configuration store against the same file.
fn open_store(database: &Path) -> Result<ConfigStore> {
    ConfigStore::open(database)
        .with_context(|| format!("opening configuration at {}", database.display()))
}

/// Resolves a race and loads its complete configuration.
fn load_config(store: &ConfigStore, selector: Option<&str>) -> Result<RaceConfig> {
    let race = configure::resolve_race(store, selector)?;
    Ok(store.load(race.id)?)
}

/// Loads the only configured race, for commands that are not race-scoped but still need
/// checkpoint names to print. Falls back to an empty configuration rather than failing:
/// `reader status` is useful before any race exists.
fn load_only_race_config(store: &ConfigStore) -> Result<RaceConfig> {
    match store.resolve_race(None)? {
        splitforge_storage::RaceSelection::One(race) => Ok(store.load(race.id)?),
        _ => Ok(RaceConfig {
            event: splitforge_domain::Event {
                id: splitforge_domain::EventId::new(),
                name: String::new(),
            },
            race: splitforge_domain::Race {
                id: splitforge_domain::RaceId::new(),
                event: splitforge_domain::EventId::new(),
                name: String::new(),
                scheduled_start: None,
                expected_laps: 1,
            },
            checkpoints: Vec::new(),
            participants: Vec::new(),
            assignments: Vec::new(),
            readers: store.readers()?,
            antennas: store.antenna_map()?,
            policy: splitforge_domain::TimingPolicy::default(),
            sessions: Vec::new(),
        }),
    }
}

/// Records a start or a stop and reports it.
fn record_session(
    store: &mut ConfigStore,
    actor: &str,
    race: RaceRef,
    at: Option<String>,
    note: Option<String>,
    action: SessionAction,
    format: Format,
) -> Result<()> {
    let race = configure::resolve_race(store, race.race.as_deref())?;
    let at = configure::parse_instant(at.as_deref())?;
    let session = store.record_session(race.id, action, at, actor, note.as_deref())?;
    store.record_audit(
        actor,
        match action {
            SessionAction::Start => "race.start",
            SessionAction::Stop => "race.stop",
        },
        Some(&race.name),
        // RFC 3339, not `Display`. The audit trail is machine-readable evidence, and
        // `OffsetDateTime`'s `Display` renders `2026-04-11 8:00:00.0 +00:00:00`, which no
        // parser this project uses will accept back.
        Some(&format!(
            r#"{{"at":"{}"}}"#,
            at.format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_default()
        )),
    )?;
    emit(&SessionView::of(&session), format)
}

/// Builds a policy view with checkpoint names resolved.
fn policy_view(
    store: &ConfigStore,
    race: &splitforge_domain::Race,
    policy: splitforge_domain::TimingPolicy,
) -> Result<PolicyView> {
    let checkpoints = store.checkpoints(race.id)?;
    let per_checkpoint_names = policy
        .per_checkpoint
        .keys()
        .map(|id| {
            let name = checkpoints
                .iter()
                .find(|c| c.id == *id)
                .map_or_else(|| "(unknown)".to_owned(), |c| c.name.clone());
            (id.to_string(), name)
        })
        .collect();

    Ok(PolicyView {
        race: race.name.clone(),
        policy,
        per_checkpoint_names,
    })
}

/// Re-derives everything from the journal.
///
/// Nothing derived is stored: derivation is a pure function of the journal plus the policy,
/// so the honest thing is to recompute it on demand. That also means every invocation of
/// this command is a live test of the property Milestone 1 was about.
fn derivation_report(database: &Path, selector: Option<&str>) -> Result<DerivationReport> {
    let store = open_store(database)?;
    let config = load_config(&store, selector)?;
    let journal = open_journal(database)?;
    let reads = journal.read_all()?;
    let chips = config.chips().context("chip assignments overlap")?;

    let derivation = derive(&DerivationInput {
        reads: &reads,
        policy: &config.policy,
        chips: &chips,
        antennas: &config.antennas,
    });

    Ok(DerivationReport::build(&config, reads.len(), &derivation))
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
