//! Running an event: watching reads arrive, checking the database, exporting, backing up.

use std::io::Write as _;
use std::path::Path;
use std::time::Duration as StdDuration;

use anyhow::{Context, Result};
use splitforge_domain::{CheckpointKind, RaceConfig, RawReadJournal, StartMode};
use splitforge_engine::{DerivationInput, derive};
use splitforge_storage::{ConfigStore, RecoveryReport, SqliteJournal};

use crate::cli::{ExportFormat, Format};
use crate::report::{DoctorReport, Finding, RawReadView};

/// Streams raw reads, optionally following the journal as it grows.
///
/// Polling rather than a notification: SQLite has no cross-process change feed worth
/// relying on, and a poll against a WAL database is a cheap read that never blocks the
/// writer. An operator watching this at the finish line is a second process reading a live
/// journal, which is exactly the case `SqliteJournal::open` was fixed to support.
pub(crate) async fn tail(
    journal: &SqliteJournal,
    since: Option<u64>,
    limit: Option<usize>,
    follow: bool,
    interval_ms: u64,
    format: Format,
) -> Result<()> {
    let mut cursor = since.unwrap_or(0);
    let mut emitted = 0_usize;
    let limit = limit.unwrap_or(usize::MAX);

    loop {
        let batch = journal.read_since(cursor)?;
        for stored in &batch {
            if emitted >= limit {
                return Ok(());
            }
            cursor = cursor.max(stored.seq);
            emitted += 1;
            // One JSON object per line while following, so the stream stays pipeable into
            // `jq` or a log. Pretty-printing a stream produces something nothing can parse
            // incrementally.
            let view = RawReadView::of(stored);
            let json = if follow || format == Format::Compact {
                serde_json::to_string(&view)
            } else {
                serde_json::to_string_pretty(&view)
            }
            .context("serializing read")?;
            println!("{json}");
        }

        if !follow || emitted >= limit {
            return Ok(());
        }
        // Flush before sleeping: a follower that buffers is a follower that shows nothing
        // for a minute and then everything at once.
        std::io::stdout().flush().ok();
        tokio::time::sleep(StdDuration::from_millis(interval_ms.max(50))).await;
    }
}

/// What a reconciliation moved, as the operator sees it.
#[derive(Debug, serde::Serialize)]
pub(crate) struct RecoveryView {
    /// Reads the sidecar holds and verified.
    pub sidecar_records: u64,
    /// Reads replayed out of the sidecar into the database.
    pub replayed_into_database: u64,
    /// Reads copied into the sidecar so it is a superset again.
    pub backfilled_into_sidecar: u64,
    /// Lines that failed their digest. Damage, and unrecoverable from this file.
    pub corrupt_lines: u64,
    /// Trailing bytes from an interrupted write. Expected after a power cut.
    pub torn_tail_bytes: u64,
}

impl RecoveryView {
    pub(crate) const fn of(report: &RecoveryReport) -> Self {
        Self {
            sidecar_records: report.found.records,
            replayed_into_database: report.replayed_into_database,
            backfilled_into_sidecar: report.backfilled_into_sidecar,
            corrupt_lines: report.found.corrupt_lines,
            torn_tail_bytes: report.found.torn_tail_bytes,
        }
    }
}

/// Checks that the database and its configuration are sound.
///
/// Deliberately opinionated about what counts as an error rather than a warning: an error
/// is something that will produce wrong results or lose data, and a warning is something an
/// operator should know before the gun. A check that cannot decide which it is has not been
/// thought through.
pub(crate) fn doctor(
    database: &Path,
    store: &ConfigStore,
    journal: &SqliteJournal,
    configs: &[RaceConfig],
) -> Result<DoctorReport> {
    let mut findings = Vec::new();
    let mut checks_run = 0_usize;

    checks_run += 1;
    for problem in store.integrity_check()? {
        findings.push(Finding::error("database.integrity", problem));
    }

    checks_run += 1;
    for problem in store.foreign_key_check()? {
        findings.push(Finding::error("database.foreign_keys", problem));
    }

    // The sidecar is the independent answer to "is the evidence recoverable if this file
    // is not?". Reported, never repaired: `doctor` is a diagnosis and repair is `recover`.
    checks_run += 1;
    let sidecar = journal.survey()?;
    if sidecar.missing_from_database > 0 {
        findings.push(Finding::error(
            "journal.sidecar",
            format!(
                "{} read(s) are in the sidecar but not in the database. \
                 Run `splitforge recover` to replay them.",
                sidecar.missing_from_database
            ),
        ));
    }
    if sidecar.missing_from_sidecar > 0 {
        findings.push(Finding::warning(
            "journal.sidecar",
            format!(
                "{} read(s) are in the database but not in the sidecar, so they have no \
                 second copy. Run `splitforge recover` to back them up.",
                sidecar.missing_from_sidecar
            ),
        ));
    }
    if sidecar.corrupt_lines > 0 {
        findings.push(Finding::error(
            "journal.sidecar",
            format!(
                "{} sidecar line(s) failed their checksum. Those reads cannot be recovered \
                 from this file; the database is now the only copy.",
                sidecar.corrupt_lines
            ),
        ));
    }
    if sidecar.torn_tail_bytes > 0 {
        findings.push(Finding::warning(
            "journal.sidecar",
            format!(
                "the sidecar ends with {} byte(s) of an unfinished write, which is the \
                 expected shape after a power cut. The reads before it are intact.",
                sidecar.torn_tail_bytes
            ),
        ));
    }

    // The journal's sequence numbers are the independent answer to "did we lose a read?".
    // A gap means rows left a table that has no DELETE path, which is worth shouting about.
    checks_run += 1;
    let reads = journal.read_all()?;
    if let Some(first) = reads.first() {
        if first.seq != 1 {
            findings.push(Finding::warning(
                "journal.sequence",
                format!(
                    "the journal starts at sequence {} rather than 1; reads may predate this file",
                    first.seq
                ),
            ));
        }
        let gaps: Vec<u64> = reads
            .windows(2)
            .filter(|pair| pair[1].seq != pair[0].seq + 1)
            .map(|pair| pair[0].seq)
            .collect();
        if !gaps.is_empty() {
            findings.push(Finding::error(
                "journal.sequence",
                format!(
                    "the journal has {} gap(s), after sequence {}. Evidence is missing.",
                    gaps.len(),
                    gaps.iter()
                        .take(5)
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ));
        }
    }

    checks_run += 1;
    if configs.is_empty() {
        findings.push(Finding::warning(
            "config.race",
            "no races are configured; `splitforge race create` is the next step",
        ));
    }

    for config in configs {
        let race = &config.race;

        checks_run += 1;
        if config.checkpoints.is_empty() {
            findings.push(Finding::error(
                "config.checkpoints",
                format!(
                    "race {:?} has no checkpoints; no read can be interpreted",
                    race.name
                ),
            ));
        } else if !config
            .checkpoints
            .iter()
            .any(|c| matches!(c.kind, CheckpointKind::Finish | CheckpointKind::Lap))
        {
            findings.push(Finding::warning(
                "config.checkpoints",
                format!(
                    "race {:?} has no finish or lap checkpoint; nothing will ever complete",
                    race.name
                ),
            ));
        }

        // Scoring needs exactly one finish and, under chip timing, exactly one start. Both
        // are checked here rather than only at `results publish`, because publishing happens
        // after the race and a checkpoint cannot be added retroactively to a mat that was
        // never there.
        checks_run += 1;
        let finishes = config
            .checkpoints
            .iter()
            .filter(|c| c.kind == CheckpointKind::Finish)
            .count();
        if finishes > 1 {
            findings.push(Finding::error(
                "scoring.finish",
                format!(
                    "race {:?} has {finishes} finish checkpoints; scoring needs exactly one, \
                     and choosing between them arbitrarily would silently decide which line \
                     counted",
                    race.name
                ),
            ));
        }

        checks_run += 1;
        let starts = config
            .checkpoints
            .iter()
            .filter(|c| c.kind == CheckpointKind::Start)
            .count();
        if starts > 1 {
            findings.push(Finding::error(
                "scoring.start",
                format!(
                    "race {:?} has {starts} start checkpoints; scoring needs exactly one",
                    race.name
                ),
            ));
        } else if config.start_mode == StartMode::Chip && starts == 0 {
            findings.push(Finding::error(
                "scoring.start",
                format!(
                    "race {:?} is scored on chip time but has no start checkpoint to measure \
                     from; add one, or switch to `--start-mode gun`",
                    race.name
                ),
            ));
        }

        // Gun-time scoring measures from the gun, and there may not be one. Recoverable
        // afterwards with `race start --at`, but far easier to notice now.
        checks_run += 1;
        if config.start_mode == StartMode::Gun && config.gun_time().is_none() {
            findings.push(Finding::warning(
                "scoring.gun",
                format!(
                    "race {:?} is scored from the gun, but no start has been recorded and it \
                     has no scheduled start; finishers will have no time until one exists",
                    race.name
                ),
            ));
        }

        checks_run += 1;
        if config.participants.is_empty() {
            findings.push(Finding::warning(
                "config.roster",
                format!(
                    "race {:?} has no roster; every crossing will be unassigned",
                    race.name
                ),
            ));
        }

        // Overlapping assignments are refused at import, but a database edited by hand — or
        // written by an older build — can still hold them.
        checks_run += 1;
        if let Err(error) = config.chips() {
            findings.push(Finding::error("config.chips", error.to_string()));
        }

        checks_run += 1;
        let unassigned: Vec<&str> = config
            .participants
            .iter()
            .filter(|participant| {
                !config
                    .assignments
                    .iter()
                    .any(|a| a.participant == participant.id)
            })
            .map(|participant| participant.bib.as_str())
            .collect();
        if !unassigned.is_empty() {
            findings.push(Finding::warning(
                "config.chips",
                format!(
                    "race {:?}: {} entrant(s) have no chip and cannot be timed ({}{})",
                    race.name,
                    unassigned.len(),
                    unassigned
                        .iter()
                        .take(10)
                        .copied()
                        .collect::<Vec<_>>()
                        .join(", "),
                    if unassigned.len() > 10 { ", …" } else { "" }
                ),
            ));
        }

        checks_run += 1;
        if config.antennas.is_empty() {
            findings.push(Finding::error(
                "config.readers",
                format!(
                    "race {:?} has no antenna mapped to a checkpoint; every read will be \
                     rejected as unmapped",
                    race.name
                ),
            ));
        }

        // A reader that appears in the journal but is mapped to nothing is the single most
        // common configuration mistake: the mat works, the reads land, and nothing scores.
        checks_run += 1;
        for stored in &reads {
            if config
                .antennas
                .resolve(&stored.read.source, stored.read.antenna)
                .is_none()
            {
                findings.push(Finding::error(
                    "config.readers",
                    format!(
                        "reader {:?} (antenna {}) has reads in the journal but is not mapped to \
                         any checkpoint of race {:?}",
                        stored.read.source.as_str(),
                        stored
                            .read
                            .antenna
                            .map_or_else(|| "none".to_owned(), |a| a.to_string()),
                        race.name
                    ),
                ));
                break;
            }
        }

        checks_run += 1;
        if config.gun_time().is_none() {
            findings.push(Finding::warning(
                "config.gun_time",
                format!(
                    "race {:?} has neither a scheduled start nor a recorded one; gun time \
                     scoring will have nothing to measure from",
                    race.name
                ),
            ));
        }
    }

    checks_run += 1;
    let untrusted = reads
        .iter()
        .filter(|stored| !stored.read.device_clock_state.is_trustworthy())
        .count();
    if untrusted > 0 {
        findings.push(Finding::warning(
            "clock.device",
            format!(
                "{untrusted} read(s) were taken while the device clock had no trustworthy \
                 source; results derived from them need an accuracy caveat"
            ),
        ));
    }

    let errors = findings.iter().filter(|f| f.severity == "error").count();
    let warnings = findings.len() - errors;

    Ok(DoctorReport {
        database: database.display().to_string(),
        schema_version: store.schema_version()?,
        checks_run,
        errors,
        warnings,
        findings,
    })
}

/// Exports derived crossings.
///
/// Crossings, not results. Placement, statuses, and gun/chip time are Milestone 4, and a
/// column named `place` produced before the scoring rules exist would be a number somebody
/// publishes and nobody can defend.
pub(crate) fn export_crossings(
    config: &RaceConfig,
    journal: &SqliteJournal,
    format: ExportFormat,
    output: Option<&Path>,
) -> Result<usize> {
    let reads = journal.read_all()?;
    let chips = config.chips().context("chip assignments overlap")?;
    let derivation = derive(&DerivationInput {
        reads: &reads,
        policy: &config.policy,
        chips: &chips,
        antennas: &config.antennas,
    });

    let report = crate::report::DerivationReport::build(config, reads.len(), &derivation);
    let rows = report.accepted_reads;

    let body = match format {
        ExportFormat::Json => {
            serde_json::to_string_pretty(&rows).context("serializing crossings")?
        }
        ExportFormat::Csv => {
            let mut writer = csv::Writer::from_writer(Vec::new());
            writer
                .write_record([
                    "checkpoint",
                    "at",
                    "bib",
                    "name",
                    "chip",
                    "lap",
                    "rssi_dbm",
                    "burst_reads",
                    "accepted_read_id",
                    "source_raw_read",
                ])
                .context("writing csv header")?;

            for row in &rows {
                writer
                    .write_record([
                        row.checkpoint.clone(),
                        row.at
                            .format(&time::format_description::well_known::Rfc3339)
                            .unwrap_or_default(),
                        row.bib.clone().unwrap_or_default(),
                        row.name.clone().unwrap_or_default(),
                        row.chip.as_str().to_owned(),
                        row.lap.map(|lap| lap.to_string()).unwrap_or_default(),
                        row.rssi_dbm.map(|r| r.to_string()).unwrap_or_default(),
                        row.burst_reads.to_string(),
                        row.id.to_string(),
                        row.source_raw_read.to_string(),
                    ])
                    .context("writing csv row")?;
            }

            let bytes = writer.into_inner().context("finishing csv")?;
            String::from_utf8(bytes).context("csv was not valid utf-8")?
        }
    };

    match output {
        Some(path) => {
            std::fs::write(path, &body)
                .with_context(|| format!("writing export to {}", path.display()))?;
        }
        None => println!("{body}"),
    }

    Ok(rows.len())
}
