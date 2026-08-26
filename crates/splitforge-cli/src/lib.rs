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

mod bundle;
mod cli;
mod configure;
mod operate;
mod report;
mod results;
mod simulate;

use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use splitforge_domain::{
    ManualEntry, ManualEntryId, RaceConfig, RaceId, RawReadJournal, SessionAction,
};
use splitforge_engine::{DerivationInput, derive};
use splitforge_storage::{ConfigStore, ResultStore, SqliteJournal};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

pub use bundle::{BUNDLE_FORMAT, BUNDLE_VERSION, Bundle};
pub use cli::{
    BackupCommand, CheckpointCommand, ChipsCommand, Cli, ClockCommand, Command, DeviceCommand,
    EventCommand, ExportCommand, ExportFormat, FixtureCommand, Format, ManualCommand,
    PolicyCommand, RaceCommand, RaceRef, ReaderCommand, ResultsCommand, RosterCommand, Speed,
};
pub use configure::load_fixture;
pub use report::{
    AcceptedReadView, AssignmentView, AuditView, CheckpointView, ClockStepView, ClockStepsView,
    DerivationReport, DoctorReport, EventView, Finding, ImportReport, InitReport,
    ManualEntriesView, ManualEntryView, ParticipantView, PolicyView, RaceView, RawReadView,
    ReaderStatusView, SessionView, SimulationReport, StatusReport, reader_status,
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
                RaceCommand::Start {
                    race,
                    at,
                    note,
                    force,
                } => {
                    // The pre-race gate. Checked here rather than in `doctor` alone because
                    // the checklist in docs/threat-model.md § 6 is only blocking if
                    // something actually blocks — a warning in a different command is a
                    // warning nobody ran.
                    let space = operate::check_free_space(&database, &store, force)?;
                    record_session(
                        &mut store,
                        &actor,
                        race,
                        at,
                        note,
                        SessionAction::Start,
                        format,
                        Some(space),
                        force,
                    )
                }
                RaceCommand::Stop { race, at, note } => record_session(
                    &mut store,
                    &actor,
                    race,
                    at,
                    note,
                    SessionAction::Stop,
                    format,
                    None,
                    false,
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
                    start_mode,
                } => {
                    let race = configure::resolve_race(&store, race.race.as_deref())?;
                    let policy = configure::set_policy(
                        &mut store,
                        &actor,
                        &race,
                        &configure::PolicyChange {
                            checkpoint: checkpoint.as_deref(),
                            min_interval_ms,
                            min_lap_ms,
                            selection_rule: selection_rule.as_deref(),
                            start_mode: start_mode.as_deref(),
                        },
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
            // The writing path, so recovery runs here. A mechanism exercised only during a
            // disaster is a mechanism whose first real execution is during a disaster.
            let (mut journal, recovery) = SqliteJournal::open_recovering(&database)
                .with_context(|| format!("opening journal at {}", database.display()))?;
            if recovery.repaired_anything() {
                emit(&operate::RecoveryView::of(&recovery), format)?;
            }
            let report =
                simulate::into_journal(&config, &scenario, &mut journal, seed, speed).await?;
            emit(&report, format)
        }

        Command::Device(command) => {
            let mut store = open_store(&database)?;
            match command {
                DeviceCommand::Set { min_free_mb } => {
                    let Some(mb) = min_free_mb else {
                        bail!("nothing to set; pass --min-free-mb");
                    };
                    let bytes = mb.saturating_mul(1024 * 1024);
                    store.set_min_free_bytes(bytes)?;
                    store.record_audit(
                        &actor,
                        "device.set",
                        Some("min_free_mb"),
                        Some(&format!(r#"{{"min_free_mb":{mb}}}"#)),
                    )?;
                    emit(&serde_json::json!({ "min_free_mb": mb }), format)
                }
                DeviceCommand::Show => {
                    let floor = store.min_free_bytes()?;
                    let space = splitforge_storage::disk_space(&database)?;
                    emit(
                        &serde_json::json!({
                            "database": database.display().to_string(),
                            "min_free_mb": floor / (1024 * 1024),
                            "free_mb": space.available_mb(),
                            "total_mb": space.total_mb(),
                            "above_floor": space.is_above(floor),
                        }),
                        format,
                    )
                }
            }
        }

        Command::Manual(command) => {
            let mut store = open_store(&database)?;
            match command {
                ManualCommand::Add {
                    race,
                    bib,
                    checkpoint,
                    at,
                    reason,
                } => {
                    let config = load_config(&store, race.race.as_deref())?;
                    let participant = config.participant(&bib).with_context(|| {
                        format!(
                            "no participant with bib {bib} in race {:?}; import the \
                             roster first, or check the bib",
                            config.race.name
                        )
                    })?;
                    let checkpoint_id = config
                        .checkpoint(&checkpoint)
                        .map(|found| found.id)
                        .with_context(|| {
                            let known: Vec<&str> = config
                                .checkpoints
                                .iter()
                                .map(|found| found.name.as_str())
                                .collect();
                            format!(
                                "no checkpoint named {checkpoint:?}; configured: {}",
                                if known.is_empty() {
                                    "none".to_owned()
                                } else {
                                    known.join(", ")
                                }
                            )
                        })?;
                    let at = OffsetDateTime::parse(&at, &Rfc3339).map_err(|error| {
                        anyhow!(
                            "{at:?} is not an RFC 3339 timestamp; expected something \
                             like 2026-04-11T08:17:32Z ({error})"
                        )
                    })?;

                    let entry = ManualEntry {
                        id: ManualEntryId::new(),
                        race: config.race.id,
                        participant: participant.id,
                        checkpoint: checkpoint_id,
                        at,
                        actor: actor.clone(),
                        reason: reason.clone(),
                    };

                    let mut journal = open_journal(&database)?;
                    let stored = journal.record_manual_entry(&entry)?;
                    store.record_audit(
                        &actor,
                        "manual.add",
                        Some(&bib),
                        Some(
                            &serde_json::json!({
                                "checkpoint": checkpoint,
                                "at": at.format(&Rfc3339).unwrap_or_default(),
                                "reason": reason,
                            })
                            .to_string(),
                        ),
                    )?;
                    emit(&report::ManualEntryView::of(&stored, &config), format)
                }
                ManualCommand::List { race } => {
                    let config = load_config(&store, race.race.as_deref())?;
                    let journal = open_journal(&database)?;
                    let entries = journal.manual_entries(config.race.id)?;
                    emit(&report::ManualEntriesView::of(&entries, &config), format)
                }
            }
        }

        Command::Clock(command) => {
            let journal = SqliteJournal::open(&database)
                .with_context(|| format!("opening journal at {}", database.display()))?;
            match command {
                ClockCommand::Steps { limit } => {
                    let steps = journal.recent_clock_steps(limit)?;
                    emit(&report::ClockStepsView::of(&steps), format)
                }
            }
        }

        Command::Recover { sidecar } => {
            let mut journal = match sidecar {
                Some(path) => SqliteJournal::open_with_sidecar(&database, path),
                None => SqliteJournal::open(&database),
            }
            .with_context(|| format!("opening journal at {}", database.display()))?;

            let recovery = journal.reconcile()?;
            let mut store = open_store(&database)?;
            store.record_audit(
                &actor,
                "journal.recover",
                journal
                    .sidecar_path()
                    .map(|p| p.display().to_string())
                    .as_deref(),
                Some(
                    &serde_json::to_string(&operate::RecoveryView::of(&recovery))
                        .context("encoding recovery detail")?,
                ),
            )?;
            emit(&operate::RecoveryView::of(&recovery), format)
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

        Command::Results(command) => run_results(&database, &actor, format, command),

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

        Command::Export(ExportCommand::Results {
            race,
            revision,
            shape,
            output,
        }) => {
            let store = open_store(&database)?;
            let config = load_config(&store, race.race.as_deref())?;
            let results = open_results(&database)?;
            let revision = results::load_revision(&results, &config, revision)?;
            let body = results::export(&revision, shape)?;
            match output.as_deref() {
                Some(path) => std::fs::write(path, &body)
                    .with_context(|| format!("writing export to {}", path.display()))?,
                None => println!("{body}"),
            }
            Ok(())
        }

        Command::Backup(BackupCommand::Create { destination }) => {
            let mut store = open_store(&database)?;
            // A snapshot is roughly a second copy of the database, so the question is not
            // "is there room" in general but "is there room for this, and still a floor
            // afterwards". Refusing here keeps the journal writable, which is the ordering
            // docs/architecture.md § 4 asks for: shed the non-essential write first.
            let floor = store.min_free_bytes()?;
            let space = splitforge_storage::disk_space(&database)?;
            let needed = std::fs::metadata(&database).map(|m| m.len()).unwrap_or(0);
            if space.available_bytes < needed.saturating_add(floor) {
                bail!(
                    "{} MB free; a snapshot of this database needs {} MB and must leave the \
                     {} MB floor behind it. The journal keeps writing — it is the backup \
                     that is refused.",
                    space.available_mb(),
                    // Round up, so a database under a megabyte reads as "needs 1 MB"
                    // rather than "needs about 0 MB", which is both odd and untrue.
                    needed.div_ceil(1024 * 1024),
                    floor / (1024 * 1024)
                );
            }
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

        Command::Backup(BackupCommand::Restore { snapshot, replace }) => {
            let report = splitforge_storage::restore_backup(&snapshot, &database, replace)?;
            // Audited into the *restored* database, so the record travels with the file it
            // describes rather than living in the one that was moved aside.
            let mut store = open_store(&database)?;
            store.record_audit(
                &actor,
                "backup.restore",
                Some(&report.source),
                Some(&format!(
                    r#"{{"raw_reads":{},"displaced":{}}}"#,
                    report.raw_reads,
                    serde_json::to_string(&report.displaced).context("encoding displaced")?
                )),
            )?;
            emit(
                &serde_json::json!({
                    "source": report.source,
                    "destination": report.destination,
                    "displaced": report.displaced,
                    "bytes": report.bytes,
                    "raw_reads": report.raw_reads,
                    "next": "run `splitforge recover` to replay reads the snapshot predates",
                }),
                format,
            )
        }

        Command::Doctor { race, bundle } => {
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

            // Written before the findings are printed and long before the non-zero exit
            // below. A bundle is wanted precisely when `doctor` is failing, so tying its
            // creation to a clean run would withhold it in every case it exists for.
            if let Some(path) = bundle.as_deref() {
                let results = open_results(&database)?;
                let contents =
                    bundle::build(&database, &store, &results, &journal, &configs, &report)?;
                let json = serde_json::to_string_pretty(&contents)
                    .context("serializing the diagnostic bundle")?;
                std::fs::write(path, json)
                    .with_context(|| format!("writing bundle to {}", path.display()))?;
                eprintln!("wrote diagnostic bundle to {}", path.display());
            }

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

/// Opens the results store against the same file.
fn open_results(database: &Path) -> Result<ResultStore> {
    ResultStore::open(database)
        .with_context(|| format!("opening results at {}", database.display()))
}

/// Dispatches the `results` subtree.
///
/// Split out of [`run`] rather than inlined because that match arm is already long enough
/// to need `#[allow(clippy::too_many_lines)]`, and adding seven more branches to it would
/// make the one function nobody wants to read even harder to read.
fn run_results(
    database: &Path,
    actor: &str,
    format: Format,
    command: ResultsCommand,
) -> Result<()> {
    let store = open_store(database)?;

    match command {
        ResultsCommand::Preview { race } => {
            let config = load_config(&store, race.race.as_deref())?;
            let journal = open_journal(database)?;
            let results = open_results(database)?;
            emit(&results::preview(&config, &journal, &results)?, format)
        }

        ResultsCommand::Publish {
            race,
            status,
            reason,
            allow_unchanged,
        } => {
            let config = load_config(&store, race.race.as_deref())?;
            let journal = open_journal(database)?;
            let mut results = open_results(database)?;
            let status = results::parse_revision_status(&status)?;
            let view = results::publish(
                &config,
                &journal,
                &mut results,
                actor,
                status,
                &reason,
                allow_unchanged,
            )?;

            // The audit trail has to explain what changed between revisions, so the digest
            // and the counts go in the row, not just the revision number.
            let mut audit = open_store(database)?;
            audit.record_audit(
                actor,
                "results.publish",
                Some(&config.race.name),
                Some(&serde_json::to_string(&view).unwrap_or_default()),
            )?;
            emit(&view, format)
        }

        ResultsCommand::Show { race, revision } => {
            let config = load_config(&store, race.race.as_deref())?;
            let results = open_results(database)?;
            let revision = results::load_revision(&results, &config, revision)?;
            emit(
                &results::RevisionView::build(&revision, &config.race.name),
                format,
            )
        }

        ResultsCommand::List { race } => {
            let config = load_config(&store, race.race.as_deref())?;
            let results = open_results(database)?;
            emit(&results::list(&results, &config)?, format)
        }

        ResultsCommand::Diff { race, from, to } => {
            let config = load_config(&store, race.race.as_deref())?;
            let results = open_results(database)?;
            emit(&results::diff(&results, &config, from, to)?, format)
        }

        ResultsCommand::Declare {
            race,
            bib,
            status,
            reason,
        } => {
            let config = load_config(&store, race.race.as_deref())?;
            let mut results = open_results(database)?;
            let status = results::parse_status(&status)?;
            let declaration =
                results::declare(&mut results, &config, actor, &bib, status, &reason)?;

            let mut audit = open_store(database)?;
            audit.record_audit(
                actor,
                "results.declare",
                Some(&config.race.name),
                Some(
                    &serde_json::json!({
                        "bib": bib,
                        "status": status.as_str(),
                        "reason": reason,
                        "seq": declaration.seq,
                    })
                    .to_string(),
                ),
            )?;
            emit(
                &serde_json::json!({
                    "seq": declaration.seq,
                    "bib": bib,
                    "status": status.as_str(),
                    "reason": reason,
                    "actor": declaration.actor,
                    "declared_at": declaration
                        .declared_at
                        .format(&time::format_description::well_known::Rfc3339)
                        .unwrap_or_default(),
                }),
                format,
            )
        }

        ResultsCommand::Declarations { race } => {
            let config = load_config(&store, race.race.as_deref())?;
            let results = open_results(database)?;
            emit(&results::declarations(&results, &config)?, format)
        }
    }
}

/// The operator's own entries for one race, as derivation input.
///
/// Loaded on every derivation rather than cached: a manual entry recorded after results were
/// published has to change the next revision, which only works if re-deriving picks it up
/// (timing model § 6).
pub(crate) fn manual_entries_for(
    journal: &SqliteJournal,
    race: RaceId,
) -> Result<Vec<ManualEntry>> {
    Ok(journal
        .manual_entries(race)?
        .into_iter()
        .map(|stored| stored.entry)
        .collect())
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
            start_mode: splitforge_domain::StartMode::default(),
            sessions: Vec::new(),
        }),
    }
}

/// Records a start or a stop and reports it.
#[allow(clippy::too_many_arguments)] // Two of these exist only to record the override.
fn record_session(
    store: &mut ConfigStore,
    actor: &str,
    race: RaceRef,
    at: Option<String>,
    note: Option<String>,
    action: SessionAction,
    format: Format,
    space: Option<splitforge_storage::DiskSpace>,
    forced: bool,
) -> Result<()> {
    let race = configure::resolve_race(store, race.race.as_deref())?;
    let at = configure::parse_instant(at.as_deref())?;
    let session = store.record_session(race.id, action, at, actor, note.as_deref())?;

    // RFC 3339, not `Display`. The audit trail is machine-readable evidence, and
    // `OffsetDateTime`'s `Display` renders `2026-04-11 8:00:00.0 +00:00:00`, which no
    // parser this project uses will accept back.
    let mut detail = serde_json::json!({
        "at": at
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default(),
    });
    if let Some(space) = space {
        detail["free_mb"] = serde_json::json!(space.available_mb());
    }
    if forced {
        // The whole point of the override is that it is legible afterwards. A start that
        // went ahead below the floor must say so in the record, not only on the terminal
        // of whoever typed it.
        detail["forced"] = serde_json::json!(true);
        detail["reason"] = serde_json::json!(note);
    }

    store.record_audit(
        actor,
        match action {
            SessionAction::Start => "race.start",
            SessionAction::Stop => "race.stop",
        },
        Some(&race.name),
        Some(&serde_json::to_string(&detail).context("encoding session detail")?),
    )?;

    let mut view = serde_json::to_value(SessionView::of(&session)).context("encoding session")?;
    if let Some(space) = space {
        view["free_mb"] = serde_json::json!(space.available_mb());
    }
    if forced {
        view["forced"] = serde_json::json!(true);
    }
    emit(&view, format)
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
        start_mode: store.start_mode(race.id)?.as_str(),
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

    let manual = manual_entries_for(&journal, config.race.id)?;
    let derivation = derive(&DerivationInput {
        reads: &reads,
        policy: &config.policy,
        chips: &chips,
        antennas: &config.antennas,
        manual: &manual,
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
