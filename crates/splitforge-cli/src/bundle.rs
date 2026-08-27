//! The diagnostic bundle: everything needed to debug a device, and nothing about a person.
//!
//! A bundle is what an operator attaches to a bug report at 22:00 after an event that went
//! wrong. That framing settles almost every design question in this file.
//!
//! ## Safe to send without reading it first
//!
//! The bundle is going to be emailed, pasted into a public issue tracker, and dropped into a
//! chat channel with whoever is around. Nobody is going to open it first and check what is
//! inside — and a bundle that is only safe when somebody reviews it is a bundle that is
//! eventually unsafe.
//!
//! So the guarantee is unconditional: **no participant name, no bib number, and no raw chip
//! identifier appears in a bundle, ever.** Not redacted on request, not redacted by default —
//! never written. `crates/splitforge-cli/tests/bundle.rs` asserts it against the bytes of a
//! real bundle produced from a full fixture event, because a promise about privacy that is
//! kept by reading the code is a promise that lasts until the next contributor.
//!
//! ## What replaces them
//!
//! Chip identifiers become a **per-bundle salted hash** — `h:` followed by eight hex
//! characters of SHA-256 over a random salt and the identifier. The salt is generated for
//! each bundle and then discarded, which buys the property that actually matters for
//! debugging while giving up the one that is dangerous:
//!
//! - *Within* one bundle, the same chip hashes to the same token, so "this chip was read 300
//!   times and is on no roster" survives the anonymization intact.
//! - *Across* bundles — or against a roster somebody holds — the token is worth nothing. A
//!   fresh salt means the same chip produces a different token next time.
//!
//! Operator names are hashed the same way. They are the other personal data in the database,
//! and "who published revision 2" is answerable from the device's own audit trail by anyone
//! entitled to ask.
//!
//! ## Free text is withheld, not filtered
//!
//! Notes and reasons are operator prose — `--reason "bib 104 disqualified after review"` is
//! the documented example, and it names a competitor. There is no filter that reliably finds
//! a person in free text, so none is attempted: the bundle records that a note exists, and
//! the note stays on the device.
//!
//! The same logic governs `doctor` findings, whose details are also prose. Rather than
//! guessing, [`DETAIL_IS_SAFE_TO_SHARE`] lists the checks whose message is known to carry
//! only counts and configuration. Everything else is withheld — including any check added
//! after this list was written, which is the point of stating it as an allowlist rather than
//! a denylist.
//!
//! All of this is ADR-0020, in `docs/adr/`, which also records what it costs.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Serialize;
use sha2::{Digest as _, Sha256};
use splitforge_domain::{
    CheckpointKind, RaceConfig, RawReadJournal, StartMode, StoredRawRead, TimingPolicy,
};
use splitforge_storage::{ConfigStore, DiskSpace, ResultStore, SidecarStatus, SqliteJournal};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::clock_source::ClockSourceView;
use crate::report::{DoctorReport, Finding};

/// Identifies the shape of the file to whatever reads it.
pub const BUNDLE_FORMAT: &str = "splitforge.diagnostic-bundle";

/// Bumped when a field changes meaning or disappears. New fields do not bump it.
pub const BUNDLE_VERSION: u32 = 1;

/// How many audit entries a bundle carries.
///
/// Enough to cover the configuration of an event plus a race day's operations, bounded so a
/// device that has run a season does not produce a bundle nobody will open.
const AUDIT_LIMIT: usize = 500;

/// How many hashed chip tokens are listed for any one category.
///
/// A misconfigured event can produce thousands of unrostered chips, and after the first
/// handful the diagnosis is the count rather than the list.
const TOKEN_LIMIT: usize = 50;

/// The `doctor` checks whose `detail` may be copied into a bundle verbatim.
///
/// **Fail-closed and deliberately hand-maintained.** A check is listed here only after
/// someone has read every message it can produce and confirmed it carries nothing but
/// counts, names of races and checkpoints, and instructions. Adding a check to `doctor` does
/// not add it here; the bundle withholds unknown checks and says so.
///
/// `config.chips` is the check this list exists for. It reports which entrants have no chip
/// — by bib — and it surfaces the text of a chip-assignment error, which names chips. It is
/// among the most useful findings in the whole diagnostic and it can never be shared.
const DETAIL_IS_SAFE_TO_SHARE: &[&str] = &[
    "database.integrity",
    "database.foreign_keys",
    "storage.free_space",
    "journal.sidecar",
    "journal.sequence",
    "config.race",
    "config.checkpoints",
    "config.readers",
    "config.roster",
    "config.gun_time",
    "scoring.finish",
    "scoring.start",
    "scoring.gun",
    "clock.device",
    // Counts and millisecond magnitudes. A step is a property of the device's clock, and
    // the message names no race, no participant, and no path.
    "clock.steps",
    // Two compile-time constants with no interpolation at all — nothing from the database,
    // nothing from `chronyc`'s output, nothing from the filesystem. The check reports which
    // *kind* of time source a device was following, which is the first thing that explains
    // a set of times that all look shifted.
    "clock.source",
];

/// Hashes identifiers with a salt that lives only as long as one bundle.
///
/// Not a security boundary against a determined attacker who also holds the roster: the
/// input space of a bib number is tiny and a salt held in memory does not change that. It is
/// a boundary against the realistic case — a bundle sitting in an issue tracker, read by
/// someone who has no roster and no reason to attack one.
struct Pseudonymizer {
    salt: Uuid,
}

impl Pseudonymizer {
    /// Draws a fresh salt. Never stored, never emitted.
    fn new() -> Self {
        Self {
            salt: Uuid::new_v4(),
        }
    }

    /// A stable-within-this-bundle token for one identifier.
    fn token(&self, value: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.salt.as_bytes());
        hasher.update(b"\0");
        hasher.update(value.as_bytes());
        let digest = hasher.finalize();

        let mut token = String::from("h:");
        for byte in &digest[..4] {
            let _ = write!(token, "{byte:02x}");
        }
        token
    }
}

/// Replaces the device's own filesystem paths in a message with placeholders.
///
/// A path is the one piece of environment data that reliably names a person: `/home/ada`,
/// `C:\Users\ada`. It reaches the bundle through error text — `disk_space` reports "reading
/// free space at {path}", and `doctor` copies that into an allowlisted `storage.free_space`
/// finding — so allowlisting a check is not on its own enough to make its message safe.
///
/// This is a filter, which this module otherwise refuses to do. The difference is that it
/// substitutes a small set of **exactly known strings** rather than guessing at prose: the
/// database's own path and each of its ancestor directories, longest first so a parent
/// cannot be replaced out from under a child. That is reliable in a way that hunting for a
/// person's name in an operator's sentence is not.
fn redact_paths(message: &str, database: &Path) -> String {
    let mut out = message.to_owned();
    let file_name = database.file_name().map_or_else(
        || "<database>".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );

    for (depth, ancestor) in database.ancestors().enumerate() {
        let text = ancestor.display().to_string();
        if text.is_empty() || text == "." {
            continue;
        }
        // The database itself keeps its name, which is worth reading and gives away
        // nothing. Its directories do not.
        let replacement = if depth == 0 { &file_name } else { "<path>" };
        out = out.replace(&text, replacement);
    }
    out
}

/// What the bundle says about its own contents.
#[derive(Debug, Serialize)]
pub struct PrivacyNote {
    /// The promise, in one line, for whoever opens the file.
    pub policy: &'static str,
    /// What the `h:` tokens mean and what they are worth.
    pub identifiers: &'static str,
    /// Why some fields are absent.
    pub withheld: &'static str,
}

impl PrivacyNote {
    const fn new() -> Self {
        Self {
            policy: "Contains no participant names, bib numbers, or chip identifiers. Safe to \
                     attach to a public issue.",
            identifiers: "`h:` values are SHA-256 over a random per-bundle salt that was \
                          discarded. Comparable within this file only.",
            withheld: "Operator free text (notes, reasons) and any diagnostic detail not on \
                       the allowlist are withheld rather than filtered.",
        }
    }
}

/// One `doctor` finding, with its message included only if the check is on the allowlist.
#[derive(Debug, Serialize)]
pub struct BundledFinding {
    /// `error` or `warning`.
    pub severity: String,
    /// The dotted check name. Always present — the name is never personal data.
    pub check: String,
    /// The message, when the check is known to carry no personal data.
    pub detail: Option<String>,
    /// Why the message is absent, when it is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail_withheld: Option<&'static str>,
}

impl BundledFinding {
    /// Passes one finding through the allowlist, with the device's own paths taken out.
    #[must_use]
    fn of(finding: &Finding, database: &Path) -> Self {
        if DETAIL_IS_SAFE_TO_SHARE.contains(&finding.check.as_str()) {
            Self {
                severity: finding.severity.clone(),
                check: finding.check.clone(),
                detail: Some(redact_paths(&finding.detail, database)),
                detail_withheld: None,
            }
        } else {
            Self {
                severity: finding.severity.clone(),
                check: finding.check.clone(),
                detail: None,
                detail_withheld: Some(
                    "this check's message can name participants; run `splitforge doctor` on \
                     the device to read it",
                ),
            }
        }
    }
}

/// The device and the file the bundle was taken from.
#[derive(Debug, Serialize)]
pub struct DeviceSection {
    /// The database's **file name**, never its path.
    ///
    /// A full path on a Pi is `/var/lib/splitforge/event.db` and says nothing, but the same
    /// path on a laptop is under a home directory and names the operator.
    pub database: String,
    /// Schema version, which is the first thing to check against a build.
    pub schema_version: i64,
    /// The configured free-space floor.
    pub min_free_mb: u64,
    /// What the volume had when the bundle was taken.
    pub free_mb: Option<u64>,
    /// How big the volume is.
    pub total_mb: Option<u64>,
    /// Whether a race would be allowed to start.
    pub above_floor: Option<bool>,
    /// Why the volume could not be measured, when it could not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub space_error: Option<String>,
    /// Wall-clock discontinuities recorded on this device.
    pub clock_steps: u64,
    /// Manual entries recorded on this device, across every race.
    ///
    /// A count and nothing else. The bib, the claimed time, the operator and the reason
    /// they typed are all exactly the material [ADR-0020] keeps out of a bundle — but the
    /// fact that a device has hand-entered evidence at all is the first thing that explains
    /// a time no read accounts for.
    ///
    /// [ADR-0020]: ../../../docs/adr/0020-diagnostic-bundles-carry-no-participant-data.md
    pub manual_entries: u64,
    /// The largest of them by magnitude, keeping its sign. Negative went backwards.
    pub largest_clock_step_ms: Option<i64>,
    /// What the device's time source was when the bundle was taken.
    pub clock_source: BundledClockSource,
}

/// The device's time source, with everything that could name a network stripped out.
///
/// `clock_steps` above says the clock *moved*; this says what it was following, which is the
/// other half of the same question. A set of finish times that are all wrong by the same
/// amount is explained by this field and by almost nothing else, so a bundle that omitted it
/// would send a maintainer looking at the journal for a fault that is not in the journal.
///
/// Two things `doctor` shows on the device are deliberately **not** here:
///
/// - **The reference's name.** `chronyc` prints an address for a network source, so on a
///   race-day LAN that field is an internal IP. [`Self::reference_kind`] carries the part
///   that diagnoses anything — local clock or network peer — and the address stays home.
/// - **The text of a failure.** When `chronyc` cannot be read the device shows what it
///   said; a bundle records only [`Self::measurement`], which is one of four fixed tokens.
///   That is the same "withheld, not filtered" rule this module applies to operator prose,
///   applied to program output for the same reason: nobody can promise what is in it.
#[derive(Debug, Serialize)]
pub struct BundledClockSource {
    /// Which kind of answer `doctor` got: `measured`, `no_daemon_tool`,
    /// `daemon_unreachable`, or `unreadable`.
    pub measurement: &'static str,
    /// The determined state, or `null` when nothing could be measured.
    pub state: Option<String>,
    /// `local` or `network`. Absent when there was no reference.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference_kind: Option<&'static str>,
    /// Distance from a reference clock.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stratum: Option<u8>,
    /// Long-term RMS offset in milliseconds, against the ±0.1 s budget in
    /// `docs/clock-and-time-discipline.md`. The honest accuracy figure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rms_offset_ms: Option<f64>,
    /// Whether a leap second was pending, which Q12 asks about and nothing else records.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub leap_pending: Option<bool>,
}

impl BundledClockSource {
    /// Takes the shareable half of what `doctor` determined.
    fn of(view: &ClockSourceView) -> Self {
        Self {
            measurement: view.measurement,
            state: view.state.clone(),
            reference_kind: view.reference_kind,
            stratum: view.stratum,
            rms_offset_ms: view.rms_offset_ms,
            leap_pending: view.leap_pending,
        }
    }
}

/// What the journal holds, counted rather than listed.
#[derive(Debug, Serialize)]
pub struct JournalSection {
    /// Rows in `raw_reads`.
    pub raw_reads: usize,
    /// First insertion sequence.
    pub first_seq: Option<u64>,
    /// Last insertion sequence.
    pub last_seq: Option<u64>,
    /// How many places a read is missing from the sequence. Non-zero is evidence loss.
    pub sequence_gaps: usize,
    /// Earliest authoritative timestamp.
    #[serde(with = "time::serde::rfc3339::option")]
    pub first_read_at: Option<OffsetDateTime>,
    /// Latest authoritative timestamp.
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_read_at: Option<OffsetDateTime>,
    /// Reads taken while the device clock had no trustworthy source.
    pub untrusted_clock_reads: usize,
    /// Reads stamped by this device because the reader supplied nothing usable.
    pub device_timed_reads: usize,
    /// Largest measured reader-to-device offset, in milliseconds.
    pub max_clock_offset_ms: Option<i64>,
    // Deliberately no write-latency field. `recorded_at - received_at` looks like the
    // number that says whether storage kept up, and it is — but only for reads a device
    // actually took off a socket. The simulator stamps `received_at` from the fixture's
    // race day so a restart test can compare two derivations byte for byte, which makes
    // the same subtraction report how old the fixture is: the first bundle taken from a
    // real run reported 11,129,715,096 ms, or 128 days. Write latency is a hardware
    // measurement and belongs with the rest of the Milestone 5 work that needs a Pi.
    /// Distinct chips seen, as a count. The identifiers themselves are not listed.
    pub distinct_chips: usize,
    /// Reads per `reader/antenna`, which is how a dead antenna becomes visible.
    pub by_source: BTreeMap<String, usize>,
    /// The sidecar's agreement with the database.
    pub sidecar: Option<SidecarSection>,
    /// Whether a sidecar exists beside this database at all.
    pub sidecar_present: bool,
}

/// How far the sidecar and the database agree.
///
/// A view rather than [`SidecarStatus`] itself, for the reason `report.rs` gives: the
/// storage crate's types are not a wire contract, and a rename there should not silently
/// change the shape of a file somebody is parsing.
#[derive(Debug, Serialize)]
pub struct SidecarSection {
    /// Reads the sidecar holds and verified.
    pub records: u64,
    /// Lines that failed their digest. Real damage.
    pub corrupt_lines: u64,
    /// Trailing bytes from a write that never finished. Expected after a power cut.
    pub torn_tail_bytes: u64,
    /// Reads in the sidecar that the database does not have. The recoverable case.
    pub missing_from_database: u64,
    /// Reads in the database that the sidecar does not have. Evidence without a backstop.
    pub missing_from_sidecar: u64,
}

impl SidecarSection {
    /// Copies a survey.
    const fn of(status: &SidecarStatus) -> Self {
        Self {
            records: status.records,
            corrupt_lines: status.corrupt_lines,
            torn_tail_bytes: status.torn_tail_bytes,
            missing_from_database: status.missing_from_database,
            missing_from_sidecar: status.missing_from_sidecar,
        }
    }
}

/// A checkpoint, which is course geography rather than personal data.
#[derive(Debug, Serialize)]
pub struct CheckpointSummary {
    /// Operator-chosen name.
    pub name: String,
    /// Role on the course.
    pub kind: String,
    /// Position in the expected sequence.
    pub sequence: u16,
}

/// A configured reader.
#[derive(Debug, Serialize)]
pub struct ReaderSummary {
    /// Operator-assigned identifier, which appears on every read.
    pub id: String,
    /// Whether an endpoint is configured. The address itself is a LAN detail, not a fault.
    pub endpoint_configured: bool,
    /// How far the reader's own clock is trusted.
    pub timestamp_trust: String,
    /// Its mappings, as `antenna → checkpoint`.
    pub covers: Vec<String>,
}

/// A recorded start or stop.
#[derive(Debug, Serialize)]
pub struct SessionSummary {
    /// Sequence within the database.
    pub seq: u64,
    /// `start` or `stop`.
    pub action: String,
    /// The instant recorded.
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    /// The operator, hashed.
    pub actor: String,
    /// Whether the operator left a note. The note itself stays on the device.
    pub note_present: bool,
}

/// A published revision.
#[derive(Debug, Serialize)]
pub struct RevisionSummaryView {
    /// Revision number within the race.
    pub number: u32,
    /// Whether more corrections were expected.
    pub status: String,
    /// When it was published.
    #[serde(with = "time::serde::rfc3339")]
    pub generated_at: OffsetDateTime,
    /// Who published it, hashed.
    pub actor: String,
    /// Fingerprint of the scored rows. Derived from times and places, not from names.
    pub digest: String,
    /// How many entries it holds.
    pub entries: usize,
    /// Whether a reason was recorded. Reasons name competitors and are withheld.
    pub reason_present: bool,
}

/// One configured race, reduced to its shape.
#[derive(Debug, Serialize)]
pub struct RaceSection {
    /// The event's name.
    pub event: String,
    /// The race's name.
    pub race: String,
    /// Scheduled start, when one was set.
    #[serde(with = "time::serde::rfc3339::option")]
    pub scheduled_start: Option<OffsetDateTime>,
    /// The gun in force, recorded or scheduled.
    #[serde(with = "time::serde::rfc3339::option")]
    pub gun_time: Option<OffsetDateTime>,
    /// Laps expected.
    pub expected_laps: u16,
    /// Which clock placement is measured from.
    pub start_mode: String,
    /// The rules in force. Thresholds and windows — nothing about anybody.
    pub policy: TimingPolicy,
    /// Checkpoints, in sequence order.
    pub checkpoints: Vec<CheckpointSummary>,
    /// Readers configured for this race.
    pub readers: Vec<ReaderSummary>,
    /// Roster size.
    pub participants: usize,
    /// Chip assignments configured.
    pub assignments: usize,
    /// Entrants with no chip, who therefore cannot be timed. A count, never a bib list.
    pub participants_without_a_chip: usize,
    /// Timing evidence an operator produced by hand for this race. A count, never a list.
    pub manual_entries: usize,
    /// Chips read by a reader but assigned to nobody, as hashed tokens.
    ///
    /// The correlation the tokens exist for, and the reason a bundle is worth taking: a mat
    /// that works, reads that land, and nothing that scores.
    pub unassigned_chips_read: Vec<String>,
    /// How many there were, which is the number when the list above was truncated.
    pub unassigned_chips_read_total: usize,
    /// Recorded starts and stops.
    pub sessions: Vec<SessionSummary>,
    /// Published revisions.
    pub revisions: Vec<RevisionSummaryView>,
}

/// One audit entry, reduced to the fact that it happened.
///
/// `subject` and `detail` are dropped wholesale. A subject is a race name most of the time
/// and a bib the rest of the time, and a detail carries whatever an operator typed.
#[derive(Debug, Serialize)]
pub struct AuditSummary {
    /// Sequence within the database.
    pub seq: u64,
    /// When.
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    /// Who, hashed.
    pub actor: String,
    /// What was done. A dotted action name, fixed by the code that writes it.
    pub action: String,
}

/// `doctor`'s conclusion, with findings passed through the allowlist.
#[derive(Debug, Serialize)]
pub struct BundledDoctor {
    /// Checks that ran.
    pub checks_run: usize,
    /// Problems that will produce wrong results or lose data.
    pub errors: usize,
    /// Problems worth knowing before the gun.
    pub warnings: usize,
    /// The findings.
    pub findings: Vec<BundledFinding>,
}

/// The whole bundle.
#[derive(Debug, Serialize)]
pub struct Bundle {
    /// Identifies the shape of this file.
    pub format: &'static str,
    /// The shape's version.
    pub version: u32,
    /// The build that produced it.
    pub tool_version: &'static str,
    /// When it was taken.
    #[serde(with = "time::serde::rfc3339")]
    pub generated_at: OffsetDateTime,
    /// What is deliberately not in here.
    pub privacy: PrivacyNote,
    /// The device and its storage.
    pub device: DeviceSection,
    /// What `doctor` concluded.
    pub doctor: BundledDoctor,
    /// What the journal holds.
    pub journal: JournalSection,
    /// The configured races.
    pub races: Vec<RaceSection>,
    /// The tail of the audit trail.
    pub audit: Vec<AuditSummary>,
}

/// Builds a bundle from an already-open database.
///
/// Takes the `doctor` report rather than running `doctor` again, so the bundle describes
/// exactly the run the operator just saw rather than a second one taken a moment later.
///
/// # Errors
///
/// Returns an error if the database cannot be read.
pub(crate) fn build(
    database: &Path,
    store: &ConfigStore,
    results: &ResultStore,
    journal: &SqliteJournal,
    configs: &[RaceConfig],
    report: &DoctorReport,
) -> Result<Bundle> {
    let names = Pseudonymizer::new();
    let reads = journal.read_all().context("reading the journal")?;

    Ok(Bundle {
        format: BUNDLE_FORMAT,
        version: BUNDLE_VERSION,
        tool_version: env!("CARGO_PKG_VERSION"),
        generated_at: OffsetDateTime::now_utc(),
        privacy: PrivacyNote::new(),
        device: device_section(database, store, journal, &report.clock_source)?,
        doctor: BundledDoctor {
            checks_run: report.checks_run,
            errors: report.errors,
            warnings: report.warnings,
            findings: report
                .findings
                .iter()
                .map(|finding| BundledFinding::of(finding, database))
                .collect(),
        },
        journal: journal_section(journal, &reads),
        races: configs
            .iter()
            .map(|config| race_section(config, results, journal, &reads, &names))
            .collect::<Result<Vec<_>>>()?,
        audit: store
            .audit_trail(AUDIT_LIMIT)
            .context("reading the audit trail")?
            .iter()
            .map(|entry| AuditSummary {
                seq: entry.seq,
                at: entry.at,
                actor: names.token(&entry.actor),
                action: entry.action.clone(),
            })
            .collect(),
    })
}

/// Summarizes the device and its storage.
fn device_section(
    database: &Path,
    store: &ConfigStore,
    journal: &SqliteJournal,
    clock_source: &ClockSourceView,
) -> Result<DeviceSection> {
    let floor = store.min_free_bytes().context("reading the floor")?;
    let measured = splitforge_storage::disk_space(database);

    Ok(DeviceSection {
        database: database.file_name().map_or_else(
            || "(unnamed)".to_owned(),
            |name| name.to_string_lossy().into_owned(),
        ),
        schema_version: store.schema_version().context("reading schema version")?,
        min_free_mb: floor / (1024 * 1024),
        free_mb: measured.as_ref().ok().map(DiskSpace::available_mb),
        total_mb: measured.as_ref().ok().map(DiskSpace::total_mb),
        above_floor: measured.as_ref().ok().map(|space| space.is_above(floor)),
        // The failure text names the path it could not read, which is the one thing in this
        // section that must not travel.
        space_error: measured
            .as_ref()
            .err()
            .map(|error| redact_paths(&error.to_string(), database)),
        clock_steps: journal.clock_step_count().context("counting clock steps")?,
        manual_entries: journal
            .manual_entry_count()
            .context("counting manual entries")?,
        largest_clock_step_ms: journal
            .largest_clock_step_ms()
            .context("reading the largest clock step")?,
        clock_source: BundledClockSource::of(clock_source),
    })
}

/// Counts the journal without reproducing it.
fn journal_section(journal: &SqliteJournal, reads: &[StoredRawRead]) -> JournalSection {
    let mut by_source: BTreeMap<String, usize> = BTreeMap::new();
    let mut chips: BTreeSet<&str> = BTreeSet::new();
    let mut max_offset: Option<i64> = None;

    for stored in reads {
        let antenna = stored
            .read
            .antenna
            .map_or_else(|| "*".to_owned(), |antenna| antenna.to_string());
        *by_source
            .entry(format!("{}/{antenna}", stored.read.source.as_str()))
            .or_insert(0) += 1;
        chips.insert(stored.read.chip.as_str());

        if let Some(offset) = stored.read.clock_offset_ms {
            max_offset = Some(max_offset.map_or(offset, |seen: i64| {
                if offset.abs() > seen.abs() {
                    offset
                } else {
                    seen
                }
            }));
        }
    }

    JournalSection {
        raw_reads: reads.len(),
        first_seq: reads.first().map(|stored| stored.seq),
        last_seq: reads.last().map(|stored| stored.seq),
        sequence_gaps: reads
            .windows(2)
            .filter(|pair| pair[1].seq != pair[0].seq + 1)
            .count(),
        first_read_at: reads
            .iter()
            .map(|stored| stored.read.authoritative_timestamp())
            .min(),
        last_read_at: reads
            .iter()
            .map(|stored| stored.read.authoritative_timestamp())
            .max(),
        untrusted_clock_reads: reads
            .iter()
            .filter(|stored| !stored.read.device_clock_state.is_trustworthy())
            .count(),
        device_timed_reads: reads
            .iter()
            .filter(|stored| stored.read.reader_timestamp.is_none())
            .count(),
        max_clock_offset_ms: max_offset,
        distinct_chips: chips.len(),
        by_source,
        // `survey` reads the sidecar off disk and can legitimately fail — a missing file, a
        // permission problem. A bundle that refuses to be written because the thing it is
        // diagnosing is broken would be useless exactly when it is needed.
        sidecar: journal.survey().ok().as_ref().map(SidecarSection::of),
        sidecar_present: journal.sidecar_path().is_some_and(Path::exists),
    }
}

/// Reduces one race's configuration to its shape.
fn race_section(
    config: &RaceConfig,
    results: &ResultStore,
    journal: &SqliteJournal,
    reads: &[StoredRawRead],
    names: &Pseudonymizer,
) -> Result<RaceSection> {
    let assigned: BTreeSet<&str> = config
        .assignments
        .iter()
        .map(|assignment| assignment.chip.as_str())
        .collect();

    let unassigned: Vec<&str> = reads
        .iter()
        .map(|stored| stored.read.chip.as_str())
        .filter(|chip| !assigned.contains(chip))
        .collect::<BTreeSet<&str>>()
        .into_iter()
        .collect();

    let checkpoint_name = |id| {
        config
            .checkpoint_by_id(id)
            .map_or_else(|| "(unknown)".to_owned(), |c| c.name.clone())
    };

    Ok(RaceSection {
        event: config.event.name.clone(),
        race: config.race.name.clone(),
        scheduled_start: config.race.scheduled_start,
        gun_time: config.gun_time(),
        expected_laps: config.race.expected_laps,
        start_mode: match config.start_mode {
            StartMode::Gun => "gun",
            StartMode::Chip => "chip",
        }
        .to_owned(),
        policy: config.policy.clone(),
        checkpoints: config
            .checkpoints
            .iter()
            .map(|checkpoint| CheckpointSummary {
                name: checkpoint.name.clone(),
                kind: match checkpoint.kind {
                    CheckpointKind::Start => "start",
                    CheckpointKind::Finish => "finish",
                    CheckpointKind::Lap => "lap",
                    CheckpointKind::Split => "split",
                }
                .to_owned(),
                sequence: checkpoint.sequence,
            })
            .collect(),
        readers: config
            .readers
            .iter()
            .map(|reader| ReaderSummary {
                id: reader.id.as_str().to_owned(),
                endpoint_configured: reader.endpoint.is_some(),
                timestamp_trust: reader.timestamp_trust.as_str().to_owned(),
                covers: config
                    .antennas
                    .entries()
                    .filter(|(mapped, _, _)| **mapped == reader.id)
                    .map(|(_, antenna, checkpoint)| match antenna {
                        Some(antenna) => {
                            format!("antenna {antenna} → {}", checkpoint_name(checkpoint))
                        }
                        None => format!("all antennas → {}", checkpoint_name(checkpoint)),
                    })
                    .collect(),
            })
            .collect(),
        participants: config.participants.len(),
        assignments: config.assignments.len(),
        participants_without_a_chip: config
            .participants
            .iter()
            .filter(|participant| {
                !config
                    .assignments
                    .iter()
                    .any(|assignment| assignment.participant == participant.id)
            })
            .count(),
        manual_entries: journal
            .manual_entries(config.race.id)
            .context("counting manual entries for the race")?
            .len(),
        unassigned_chips_read: unassigned
            .iter()
            .take(TOKEN_LIMIT)
            .map(|chip| names.token(chip))
            .collect(),
        unassigned_chips_read_total: unassigned.len(),
        sessions: config
            .sessions
            .iter()
            .map(|session| SessionSummary {
                seq: session.seq,
                action: session.action.as_str().to_owned(),
                at: session.at,
                actor: names.token(&session.actor),
                note_present: session.note.is_some(),
            })
            .collect(),
        revisions: results
            .revisions(config.race.id)
            .context("reading result revisions")?
            .iter()
            .map(|revision| RevisionSummaryView {
                number: revision.number,
                status: revision.status.as_str().to_owned(),
                generated_at: revision.generated_at,
                actor: names.token(&revision.actor),
                digest: revision.digest.clone(),
                entries: revision.entries,
                reason_present: !revision.reason.is_empty(),
            })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_value_hashes_alike_within_a_bundle_and_differently_across_them() {
        let one = Pseudonymizer::new();
        let two = Pseudonymizer::new();

        assert_eq!(
            one.token("E28011606000020000000101"),
            one.token("E28011606000020000000101"),
            "within-bundle correlation is the whole point of the token"
        );
        assert_ne!(
            one.token("E28011606000020000000101"),
            two.token("E28011606000020000000101"),
            "a fresh salt per bundle is what stops tokens being tracked between them"
        );
        assert_ne!(
            one.token("E28011606000020000000101"),
            one.token("E28011606000020000000102"),
        );
    }

    #[test]
    fn a_token_never_contains_the_value_it_stands_for() {
        let names = Pseudonymizer::new();
        let token = names.token("E28011606000020000000101");

        assert!(token.starts_with("h:"));
        assert_eq!(token.len(), 10, "`h:` plus four bytes of hex");
        assert!(!token.contains("E2801"));
    }

    #[test]
    fn an_unknown_check_has_its_detail_withheld() {
        // The regression this guards is the one that matters: somebody adds a `doctor` check
        // that names participants, and the bundle publishes it because nobody remembered
        // this file existed. Fail-closed means they have to opt in instead.
        let database = Path::new("/var/lib/splitforge/event.db");
        let bundled = BundledFinding::of(
            &Finding::error("config.chips", "bib 104 has no chip"),
            database,
        );
        assert_eq!(bundled.detail, None);
        assert!(bundled.detail_withheld.is_some());
        assert_eq!(bundled.check, "config.chips", "the name still travels");

        let invented = Finding::warning("check.invented.tomorrow", "Runner 101 did something");
        assert_eq!(BundledFinding::of(&invented, database).detail, None);
    }

    #[test]
    fn an_allowlisted_check_keeps_its_detail() {
        let finding = Finding::error("storage.free_space", "12 MB free, below the 256 MB floor");
        let bundled = BundledFinding::of(&finding, Path::new("/srv/event.db"));

        assert_eq!(
            bundled.detail.as_deref(),
            Some("12 MB free, below the 256 MB floor")
        );
        assert_eq!(bundled.detail_withheld, None);
    }

    #[test]
    fn an_allowlisted_message_still_loses_the_paths_inside_it() {
        // Being on the allowlist is not enough. `storage.free_space` reports a measurement
        // failure by quoting the path it could not read, and a path under a home directory
        // names the operator — so the check name travels, the message travels, and the
        // directory does not.
        let database = Path::new("/home/ada/races/event.db");
        let finding = Finding::warning(
            "storage.free_space",
            "could not measure free space: reading free space at /home/ada/races: denied",
        );

        let detail = BundledFinding::of(&finding, database)
            .detail
            .expect("an allowlisted check keeps its message");

        assert!(!detail.contains("/home/ada"), "got: {detail}");
        assert!(!detail.contains("ada"), "got: {detail}");
        assert!(
            detail.contains("denied"),
            "the reason is the useful part and has to survive: {detail}"
        );
    }

    #[test]
    fn redaction_keeps_the_database_name_and_drops_its_directories() {
        let database = Path::new("/home/ada/races/event.db");

        assert_eq!(
            redact_paths("cannot open /home/ada/races/event.db", database),
            "cannot open event.db",
            "the file name is worth reading and gives away nothing"
        );
        assert_eq!(
            redact_paths("no directory above /home/ada to measure", database),
            "no directory above <path> to measure"
        );
        // Longest first: a parent must not be substituted out from under its child.
        assert_eq!(
            redact_paths("/home/ada/races/event.db beside /home/ada/races", database),
            "event.db beside <path>"
        );
        assert_eq!(
            redact_paths("638 reads, no paths here", database),
            "638 reads, no paths here",
            "a message with nothing to redact comes through untouched"
        );
    }

    #[test]
    fn config_chips_is_not_on_the_allowlist() {
        // Stated as its own test so that putting it on the list fails with a name that
        // explains itself, rather than quietly widening what a bundle publishes.
        assert!(
            !DETAIL_IS_SAFE_TO_SHARE.contains(&"config.chips"),
            "config.chips reports entrants by bib; its detail can never be shared"
        );
    }
}
