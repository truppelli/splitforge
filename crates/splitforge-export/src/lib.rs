//! # splitforge-export
//!
//! Versioned CSV and JSON export contracts.
//!
//! ## Boundaries
//!
//! - **May depend on:** splitforge-domain, splitforge-results
//! - **Must never depend on:** splitforge-llrp, splitforge-api, splitforge-sync
//!
//! These rules come from ADR-0001 and are tabulated in `docs/architecture.md`.
//! They are enforced by `crates/splitforge-testkit/tests/dependency_rules.rs`, not left to
//! review (ADR-0012). It currently uses only `splitforge-domain`: a published revision is a
//! domain value, and rendering one needs no access to the scoring rules that produced it.
//!
//! ## What belongs here, and what does not
//!
//! This crate owns **published contracts** — output another system reads, and that
//! therefore cannot change shape without notice. Results are that. The crossings dump
//! behind `splitforge export crossings` is not: it is a diagnostic view of a derivation,
//! shaped for an operator squinting at why a runner is missing, and it stays in the CLI
//! where it can change whenever the diagnostic needs to.
//!
//! The distinction is worth keeping. A file with a version number on it is a promise, and
//! promises should be made deliberately and in one place.
//!
//! ## Versioning
//!
//! [`RESULTS_FORMAT`] and [`RESULTS_VERSION`] are emitted in the JSON envelope and are the
//! contract's identity. The CSV's contract is its **column list and order**, which is
//! asserted by a test in this crate — if a column moves, that test fails, and moving it
//! anyway means saying so in the version.

// No panicking on any path reachable during an event: a corrupt frame, a missing
// field, or an out-of-range value must become an error the caller can act on, never a
// timer that stops mid-race. Test code is exempt, where panicking on the unexpected is
// the point. See CONTRIBUTING.md, "Code standards".
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use serde::Serialize;
use splitforge_domain::{ResultEntry, ResultRevision};
use time::format_description::well_known::Rfc3339;

/// The JSON export's format identifier.
pub const RESULTS_FORMAT: &str = "splitforge.results";

/// The JSON export's contract version.
///
/// Bumped when a field changes meaning or disappears. Adding a field does not require a
/// bump — a consumer that ignores unknown fields keeps working, and one that does not was
/// going to break on anything.
pub const RESULTS_VERSION: u32 = 1;

/// The results CSV's columns, in order. This ordering is the contract.
pub const RESULTS_CSV_COLUMNS: &[&str] = &[
    "place",
    "bib",
    "name",
    "status",
    "status_source",
    "status_reason",
    "gun_time",
    "chip_time",
    "scoring_time",
    "scoring_time_ms",
    "start_at",
    "finish_at",
    "flags",
];

/// Why an export could not be produced.
#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    /// The CSV writer failed.
    #[error("writing results csv: {0}")]
    Csv(#[from] csv::Error),
    /// The CSV bytes were not valid UTF-8.
    #[error("results csv was not valid utf-8: {0}")]
    Encoding(#[from] std::string::FromUtf8Error),
    /// Serializing to JSON failed.
    #[error("serializing results json: {0}")]
    Json(#[from] serde_json::Error),
}

/// The JSON envelope. Publication metadata and the policy snapshot travel with the rows,
/// because a results file that does not say what rules produced it cannot be checked.
#[derive(Debug, Serialize)]
struct ResultsDocument<'a> {
    format: &'static str,
    version: u32,
    revision: RevisionHeader<'a>,
    entries: &'a [ResultEntry],
}

#[derive(Debug, Serialize)]
struct RevisionHeader<'a> {
    id: String,
    race: String,
    number: u32,
    status: &'static str,
    generated_at: String,
    actor: &'a str,
    reason: &'a str,
    policy: &'a splitforge_domain::ScoringPolicy,
    digest: String,
}

/// Renders a revision as JSON.
///
/// # Errors
///
/// Returns [`ExportError::Json`] if serialization fails.
pub fn results_json(revision: &ResultRevision) -> Result<String, ExportError> {
    let document = ResultsDocument {
        format: RESULTS_FORMAT,
        version: RESULTS_VERSION,
        revision: RevisionHeader {
            id: revision.id.to_string(),
            race: revision.race.to_string(),
            number: revision.number,
            status: revision.status.as_str(),
            generated_at: revision.generated_at.format(&Rfc3339).unwrap_or_default(),
            actor: &revision.actor,
            reason: &revision.reason,
            policy: &revision.policy,
            digest: revision.digest(),
        },
        entries: &revision.entries,
    };
    Ok(serde_json::to_string_pretty(&document)?)
}

/// Renders a revision as CSV, one row per participant.
///
/// # Errors
///
/// Returns [`ExportError::Csv`] or [`ExportError::Encoding`] if the row cannot be written.
pub fn results_csv(revision: &ResultRevision) -> Result<String, ExportError> {
    let mut writer = csv::Writer::from_writer(Vec::new());
    writer.write_record(RESULTS_CSV_COLUMNS)?;

    for entry in &revision.entries {
        writer.write_record([
            entry
                .place
                .map(|place| place.to_string())
                .unwrap_or_default(),
            entry.bib.as_str().to_owned(),
            entry.name.clone(),
            entry.status.as_str().to_owned(),
            status_source(entry).to_owned(),
            entry.status_reason.clone().unwrap_or_default(),
            format_optional_ms(entry.gun_time_ms),
            format_optional_ms(entry.chip_time_ms),
            format_optional_ms(entry.scoring_time_ms),
            entry
                .scoring_time_ms
                .map(|ms| ms.to_string())
                .unwrap_or_default(),
            format_optional_instant(entry.start_at),
            format_optional_instant(entry.finish_at),
            entry
                .flags
                .iter()
                .map(|flag| flag.label())
                .collect::<Vec<_>>()
                .join(" "),
        ])?;
    }

    Ok(String::from_utf8(writer.into_inner().map_err(
        |error| -> csv::Error { error.into_error().into() },
    )?)?)
}

fn status_source(entry: &ResultEntry) -> &'static str {
    match entry.status_source {
        splitforge_domain::StatusSource::Derived => "derived",
        splitforge_domain::StatusSource::Declared => "declared",
    }
}

fn format_optional_instant(value: Option<time::OffsetDateTime>) -> String {
    value
        .and_then(|at| at.format(&Rfc3339).ok())
        .unwrap_or_default()
}

fn format_optional_ms(value: Option<i64>) -> String {
    value.map(format_duration_ms).unwrap_or_default()
}

/// Formats a duration as `H:MM:SS.mmm`.
///
/// Zero-padded and always hour-prefixed so that sorting the column as text matches sorting
/// it as time — a spreadsheet is where most of these files are opened, and `9:59.0` sorting
/// after `10:00.0` is exactly the kind of thing nobody notices until prize-giving.
#[must_use]
pub fn format_duration_ms(total_ms: i64) -> String {
    let sign = if total_ms < 0 { "-" } else { "" };
    let magnitude = total_ms.unsigned_abs();
    let millis = magnitude % 1_000;
    let seconds = (magnitude / 1_000) % 60;
    let minutes = (magnitude / 60_000) % 60;
    let hours = magnitude / 3_600_000;
    format!("{sign}{hours}:{minutes:02}:{seconds:02}.{millis:03}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use splitforge_domain::{
        Bib, ParticipantId, RaceId, ResultFlag, ResultRevisionId, ResultStatus, RevisionStatus,
        ScoringPolicy, StatusSource,
    };
    use time::OffsetDateTime;

    fn entry(bib: &str, place: Option<u32>, status: ResultStatus, ms: Option<i64>) -> ResultEntry {
        ResultEntry {
            participant: ParticipantId::new(),
            bib: Bib::new(bib),
            name: format!("Runner {bib}"),
            status,
            status_source: StatusSource::Derived,
            status_reason: None,
            start_at: Some(OffsetDateTime::UNIX_EPOCH),
            finish_at: ms.map(|ms| OffsetDateTime::UNIX_EPOCH + time::Duration::milliseconds(ms)),
            gun_time_ms: ms,
            chip_time_ms: ms,
            scoring_time_ms: ms,
            place,
            flags: Vec::new(),
            timing_events: Vec::new(),
        }
    }

    fn revision(entries: Vec<ResultEntry>) -> ResultRevision {
        ResultRevision {
            id: ResultRevisionId::new(),
            race: RaceId::new(),
            number: 2,
            status: RevisionStatus::Final,
            generated_at: OffsetDateTime::UNIX_EPOCH,
            actor: "operator".to_owned(),
            reason: "bib 217 disqualified".to_owned(),
            policy: ScoringPolicy::gun(Some(OffsetDateTime::UNIX_EPOCH)),
            entries,
        }
    }

    #[test]
    fn durations_are_zero_padded_so_text_sort_matches_time_sort() {
        assert_eq!(format_duration_ms(0), "0:00:00.000");
        assert_eq!(format_duration_ms(1_234), "0:00:01.234");
        assert_eq!(format_duration_ms(599_000), "0:09:59.000");
        assert_eq!(format_duration_ms(600_000), "0:10:00.000");
        assert!(
            format_duration_ms(599_000) < format_duration_ms(600_000),
            "9:59 must sort before 10:00 as text, which is the whole point of the padding"
        );
        assert_eq!(format_duration_ms(3_661_500), "1:01:01.500");
    }

    #[test]
    fn a_negative_duration_keeps_its_sign() {
        // Only ever a diagnostic — scoring refuses to place a negative time — but it must
        // not render as a plausible positive one.
        assert_eq!(format_duration_ms(-1_500), "-0:00:01.500");
    }

    #[test]
    fn the_csv_column_list_is_the_contract() {
        // Changing this assertion means changing a published contract. Do it deliberately,
        // and say so in RESULTS_VERSION.
        assert_eq!(
            RESULTS_CSV_COLUMNS,
            [
                "place",
                "bib",
                "name",
                "status",
                "status_source",
                "status_reason",
                "gun_time",
                "chip_time",
                "scoring_time",
                "scoring_time_ms",
                "start_at",
                "finish_at",
                "flags",
            ]
        );
    }

    #[test]
    fn csv_has_a_header_and_one_row_per_entry() {
        let published = revision(vec![
            entry("101", Some(1), ResultStatus::Finished, Some(1_100_000)),
            entry("102", None, ResultStatus::Dnf, None),
        ]);

        let csv = results_csv(&published).expect("render csv");
        let lines: Vec<&str> = csv.lines().collect();

        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("place,bib,name,status"));
        assert!(lines[1].starts_with("1,101,Runner 101,finished,derived,,0:18:20.000"));
        assert_eq!(
            lines[2], ",102,Runner 102,dnf,derived,,,,,,1970-01-01T00:00:00Z,,",
            "a DNF has no place and no finish time, but it keeps the start crossing that \
             proves they started — that is what makes it a DNF rather than a DNS"
        );
    }

    #[test]
    fn a_name_containing_a_comma_is_quoted_not_split() {
        let mut row = entry("101", Some(1), ResultStatus::Finished, Some(1_000));
        row.name = "Ó Séaghdha, Rónán".to_owned();
        let csv = results_csv(&revision(vec![row])).expect("render csv");
        assert!(
            csv.contains("\"Ó Séaghdha, Rónán\""),
            "the name must survive as one field: {csv}"
        );
    }

    #[test]
    fn flags_are_space_separated_in_one_column() {
        let mut row = entry("101", None, ResultStatus::Finished, Some(1_000));
        row.flags = vec![
            ResultFlag::NoStartReadUnderChipTime,
            ResultFlag::FinishBeforeStart,
        ];
        let csv = results_csv(&revision(vec![row])).expect("render csv");
        assert!(csv.contains("no_start_read_under_chip_time finish_before_start"));
    }

    #[test]
    fn json_carries_the_policy_and_the_digest_that_produced_it() {
        let published = revision(vec![entry(
            "101",
            Some(1),
            ResultStatus::Finished,
            Some(1_100_000),
        )]);
        let json = results_json(&published).expect("render json");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");

        assert_eq!(parsed["format"], RESULTS_FORMAT);
        assert_eq!(parsed["version"], RESULTS_VERSION);
        assert_eq!(parsed["revision"]["number"], 2);
        assert_eq!(parsed["revision"]["status"], "final");
        assert_eq!(parsed["revision"]["reason"], "bib 217 disqualified");
        assert_eq!(parsed["revision"]["policy"]["start_mode"], "gun");
        assert_eq!(
            parsed["revision"]["digest"],
            *published.digest(),
            "the digest travels with the file, so two exports can be compared without the database"
        );
        assert_eq!(parsed["entries"][0]["bib"], "101");
        assert_eq!(parsed["entries"][0]["place"], 1);
    }

    #[test]
    fn an_empty_revision_still_exports_a_usable_file() {
        // A race where nobody finished is a real outcome, and a zero-byte file is not a
        // usable answer to it.
        let published = revision(Vec::new());
        let csv = results_csv(&published).expect("render csv");
        assert_eq!(csv.lines().count(), 1, "the header alone");

        let json = results_json(&published).expect("render json");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(parsed["entries"].as_array().map(Vec::len), Some(0));
    }
}
