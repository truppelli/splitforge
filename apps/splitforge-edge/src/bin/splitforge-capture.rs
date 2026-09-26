//! `splitforge-capture`: checks a serial capture against the journal (ADR-0041).
//!
//! Milestone 3a's exit criterion says the journal never disagrees with what arrived. A capture
//! (ADR-0040) holds what arrived. This reads it back with the service's own reassembler and
//! decoder, and compares the reads, payload for payload, with the journal's reads received in
//! the same span.
//!
//! A second binary in the composition root's package, because only this package may name the
//! protocol adapter, and the comparison needs the adapter and storage together.

use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use splitforge_domain::ReaderId;
use splitforge_storage::{ConfigStore, SqliteJournal};
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime};

/// Checks what `splitforge-edge --capture` recorded against the event journal.
#[derive(Debug, Parser)]
#[command(name = "splitforge-capture", version)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Compare the reads in a capture with the reads the journal holds for the same span.
    ///
    /// Exits 0 when they agree and the capture is complete, 1 when they disagree, 2 when
    /// nothing is missing from the journal but the capture cannot show that, and 3 when the
    /// check could not be made.
    Check {
        /// The file `splitforge-edge --capture` wrote.
        capture: PathBuf,

        /// The event database.
        #[arg(
            long,
            value_name = "PATH",
            default_value = "/var/lib/splitforge/event.db"
        )]
        database: PathBuf,

        /// Compare only the journal's reads from this reader.
        #[arg(long, value_name = "ID")]
        reader: Option<String>,

        /// How many left-over payloads to list on each side.
        #[arg(long, value_name = "N", default_value_t = 10)]
        examples: usize,
    },
}

/// How long after a capture's last record a read can still have been stamped as received.
///
/// The service stamps a read after the capture has its bytes, and a write held up behind a full
/// or locked disk is retried until it lands (ADR-0031).
const AFTER: Duration = Duration::minutes(1);

/// The most audit rows read looking for reads set aside in the span.
const AUDIT_ROWS: usize = 1_000_000;

fn main() -> ExitCode {
    match run(Args::parse()) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("splitforge-capture: {error:#}");
            ExitCode::from(3)
        }
    }
}

fn run(args: Args) -> Result<ExitCode> {
    let Command::Check {
        capture,
        database,
        reader,
        examples,
    } = args.command;

    // A payload counts +1 for each time the capture has it and -1 for each time the journal
    // does. What is left over is the disagreement.
    let mut counts: HashMap<Vec<u8>, i64> = HashMap::new();
    let file = File::open(&capture)
        .with_context(|| format!("opening the capture {}", capture.display()))?;
    let found = splitforge_thingmagic::replay(
        BufReader::new(file),
        ReaderId::new(reader.as_deref().unwrap_or("capture")),
        |read| *counts.entry(read.raw_payload).or_insert(0) += 1,
    )
    .with_context(|| format!("reading the capture {}", capture.display()))?;

    let wanted = |source: &str| reader.as_deref().is_none_or(|wanted| source == wanted);
    let mut journal_reads = 0_u64;
    let mut set_aside = 0_u64;
    let window = found.first_at.zip(found.last_at.map(|last| last + AFTER));
    if let Some((from, to)) = window {
        let journal = SqliteJournal::open(&database)
            .with_context(|| format!("opening the journal at {}", database.display()))?;
        journal.for_each_read_received_between(from, to, |stored| {
            if wanted(stored.read.source.as_str()) {
                journal_reads += 1;
                *counts.entry(stored.read.raw_payload).or_insert(0) -= 1;
            }
        })?;

        // A read the journal refused for a value it cannot hold arrived, and is accounted for on
        // the audit trail with its payload rather than in `raw_reads` (ADR-0031).
        let store = ConfigStore::open(&database)
            .with_context(|| format!("opening the configuration at {}", database.display()))?;
        for entry in store.audit_trail(AUDIT_ROWS)? {
            if entry.action != "journal.unstorable" || entry.at < from || entry.at > to {
                continue;
            }
            if !entry.subject.as_deref().is_some_and(wanted) {
                continue;
            }
            let payload = entry
                .detail
                .as_deref()
                .and_then(|detail| serde_json::from_str::<serde_json::Value>(detail).ok())
                .and_then(|detail| detail["raw_payload_hex"].as_str().and_then(unhex));
            if let Some(payload) = payload {
                set_aside += 1;
                *counts.entry(payload).or_insert(0) -= 1;
            }
        }
    }

    let mut only_in_capture: Vec<String> = Vec::new();
    let mut only_in_journal: Vec<String> = Vec::new();
    let (mut missing_from_journal, mut missing_from_capture) = (0_i64, 0_i64);
    for (payload, count) in &counts {
        if *count > 0 {
            missing_from_journal += count;
            only_in_capture.push(hex(payload));
        } else if *count < 0 {
            missing_from_capture -= count;
            only_in_journal.push(hex(payload));
        }
    }
    only_in_capture.sort_unstable();
    only_in_journal.sort_unstable();
    only_in_capture.truncate(examples);
    only_in_journal.truncate(examples);

    // A capture that dropped records, could not be read in places, or holds nothing cannot show
    // that nothing is missing from the journal. A read it has and the journal does not is the
    // failure the criterion forbids whatever else is true.
    let incomplete =
        found.dropped_records > 0 || found.unreadable_lines > 0 || found.first_at.is_none();
    let (verdict, code) = if missing_from_journal > 0 {
        ("disagree", 1)
    } else if incomplete {
        ("incomplete", 2)
    } else if missing_from_capture > 0 {
        ("disagree", 1)
    } else {
        ("agree", 0)
    };

    let rfc3339 = |at: Option<OffsetDateTime>| at.and_then(|at| at.format(&Rfc3339).ok());
    let report = serde_json::json!({
        "verdict": verdict,
        "capture": {
            "path": capture.display().to_string(),
            "first_at": rfc3339(found.first_at),
            "last_at": rfc3339(found.last_at),
            "connections": found.connections,
            "reads": found.reads,
            "tag_reports": found.tag_reports,
            "refused": found.refused,
            "end_of_cycle": found.end_of_cycle,
            "other_status": found.other_status,
            "answers": found.answers,
            "framing_faults": found.framing_faults,
            "dropped_records": found.dropped_records,
            "unreadable_lines": found.unreadable_lines,
            "sent_bytes": found.sent_bytes,
            "received_bytes": found.received_bytes,
        },
        "journal": {
            "database": database.display().to_string(),
            "reader": reader,
            "window_to": rfc3339(window.map(|(_, to)| to)),
            "reads": journal_reads,
            "set_aside": set_aside,
        },
        "missing_from_journal": missing_from_journal,
        "missing_from_capture": missing_from_capture,
        "only_in_capture": only_in_capture,
        "only_in_journal": only_in_journal,
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&report).context("encoding the report")?
    );
    Ok(ExitCode::from(code))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    text.as_bytes()
        .chunks(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok())
        .collect()
}
