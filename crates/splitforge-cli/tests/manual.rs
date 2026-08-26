//! What an operator writes down when the chip does not.
//!
//! The claim these tests hold is the one that decides whether manual entries were modelled
//! correctly: an entry is **evidence**, so it enters derivation as an input and every result
//! computed afterwards accounts for it — including results published before it was recorded,
//! once they are re-derived. Nothing here edits a result.
//!
//! The `five-k` fixture supplies the case for free. Bib 109 starts and never finishes: the
//! chip stops reporting on course, and the runner is scored `dnf` because that is what the
//! evidence says. A finish marshal saw them cross. Everything below is that correction.

mod common;

use std::path::Path;

use tempfile::TempDir;
use tokio::process::Command;

/// The `splitforge` binary, built by Cargo for this test run.
const BINARY: &str = env!("CARGO_BIN_EXE_splitforge");

/// The bib the fixture leaves unfinished.
const UNFINISHED: &str = "109";

/// A configured 5K with its reads already in the journal.
struct Device {
    _directory: TempDir,
    database: std::path::PathBuf,
}

impl Device {
    async fn new() -> Self {
        let directory = TempDir::new().expect("temp dir");
        let database = directory.path().join("event.db");
        run_ok(&["fixture", "load", "--fixture", "five-k"], &database).await;
        run_ok(&["simulate", "--scenario", "five-k"], &database).await;
        Self {
            _directory: directory,
            database,
        }
    }

    async fn run_ok(&self, args: &[&str]) -> String {
        run_ok(args, &self.database).await
    }

    async fn json(&self, args: &[&str]) -> serde_json::Value {
        json(&self.run_ok(args).await)
    }

    /// Records the marshal's entry for the runner whose chip died.
    async fn record_the_finish(&self, at: &str) -> serde_json::Value {
        self.json(&[
            "manual",
            "add",
            "--bib",
            UNFINISHED,
            "--checkpoint",
            "finish",
            "--at",
            at,
            "--reason",
            "chip stopped reporting on course; finish marshal recorded the bib",
        ])
        .await
    }

    /// One published revision's row for a bib.
    async fn row_for(&self, revision: &str, bib: &str) -> serde_json::Value {
        let shown = self
            .json(&["results", "show", "--revision", revision])
            .await;
        let rows = shown
            .as_array()
            .cloned()
            .or_else(|| shown["entries"].as_array().cloned())
            .unwrap_or_default();
        rows.into_iter()
            .find(|row| row["bib"] == bib)
            .unwrap_or_else(|| panic!("no row for bib {bib} in revision {revision}"))
    }
}

#[tokio::test]
async fn an_entry_carries_the_claimed_time_and_the_time_it_was_typed() {
    // These are different facts and the gap between them can be an hour of walking back
    // from a checkpoint. A record that kept only one of them could not answer a dispute.
    let device = Device::new().await;

    let entry = device.record_the_finish("2026-04-11T08:26:41Z").await;

    assert_eq!(entry["bib"], UNFINISHED);
    assert_eq!(entry["checkpoint"], "finish");
    assert_eq!(entry["at"], "2026-04-11T08:26:41Z");
    assert_eq!(entry["actor"], "operator");
    assert!(
        entry["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("finish marshal")),
        "the stated reason is what makes the entry auditable: {entry}"
    );

    let recorded = entry["recorded_at"].as_str().expect("a recorded_at");
    assert!(
        !recorded.starts_with("2026-04-11"),
        "the entry time must be when it was typed, not the claimed time: {entry}"
    );
}

#[tokio::test]
async fn an_entry_becomes_a_timing_event_in_the_next_derivation() {
    let device = Device::new().await;

    let before = device.json(&["derive"]).await;
    device.record_the_finish("2026-04-11T08:26:41Z").await;
    let after = device.json(&["derive"]).await;

    assert_eq!(
        after["raw_reads"], before["raw_reads"],
        "an entry is not a read and must not appear as one"
    );
    assert_eq!(
        after["accepted"], before["accepted"],
        "and it must not change how the reads were deduplicated"
    );
    assert_eq!(
        after["timing_events"].as_u64().expect("a count"),
        before["timing_events"].as_u64().expect("a count") + 1,
        "the entry has to produce exactly one timing event: {after}"
    );
}

#[tokio::test]
async fn the_runner_whose_chip_died_is_scored_from_the_marshals_entry() {
    // The headline. A `dnf` that is only a `dnf` because a chip failed becomes a finish,
    // with a place, computed by the same scoring rules as everybody else's.
    let device = Device::new().await;

    device
        .run_ok(&[
            "results",
            "publish",
            "--status",
            "provisional",
            "--reason",
            "provisional results",
        ])
        .await;

    let before = device.row_for("1", UNFINISHED).await;
    assert_eq!(before["status"], "dnf", "the fixture's premise: {before}");
    assert!(before["place"].is_null());
    assert!(before["finish_at"].is_null());

    device.record_the_finish("2026-04-11T08:26:41Z").await;
    let published = device
        .json(&[
            "results",
            "publish",
            "--status",
            "final",
            "--reason",
            "finish recorded by hand after a chip failure",
        ])
        .await;

    assert_eq!(published["revision"], 2);
    assert_eq!(published["dnf"], 0);
    assert_eq!(published["changed"], true);

    let after = device.row_for("2", UNFINISHED).await;
    assert_eq!(after["status"], "finished", "got: {after}");
    assert_eq!(after["finish_at"], "2026-04-11T08:26:41Z");
    assert!(
        after["place"].as_u64().is_some(),
        "a finisher takes a place: {after}"
    );

    // The gun time is measured from the gun, and the chip time from this runner's own start
    // crossing — which the chip *did* record. Both kinds of evidence in one result.
    assert!(
        after["chip_time_ms"].as_u64() < after["gun_time_ms"].as_u64(),
        "the chip time has to come off the real start read, not the gun: {after}"
    );
}

#[tokio::test]
async fn the_revision_published_before_the_entry_is_left_exactly_as_it_was() {
    // The reason this is an input to derivation rather than an edit to a result. Revision 1
    // went out with a `dnf` in it and somebody may have acted on it; it stays true to what
    // was known at the time, and the correction lives in revision 2.
    let device = Device::new().await;
    device
        .run_ok(&[
            "results",
            "publish",
            "--status",
            "provisional",
            "--reason",
            "provisional results",
        ])
        .await;
    let first = device.json(&["results", "list"]).await;

    device.record_the_finish("2026-04-11T08:26:41Z").await;
    device
        .run_ok(&[
            "results",
            "publish",
            "--status",
            "final",
            "--reason",
            "finish recorded by hand after a chip failure",
        ])
        .await;

    let still = device.row_for("1", UNFINISHED).await;
    assert_eq!(
        still["status"], "dnf",
        "a published revision is immutable, entry or no entry: {still}"
    );

    let now = device.json(&["results", "list"]).await;
    assert_eq!(
        first[0]["digest"], now[0]["digest"],
        "revision 1's digest must not move when later evidence arrives"
    );
    assert_ne!(
        now[0]["digest"], now[1]["digest"],
        "and the correction has to be visible between them: {now}"
    );
}

#[tokio::test]
async fn re_deriving_after_a_restart_reproduces_the_same_result() {
    // An entry is evidence, so it has to survive the process that recorded it and produce
    // byte-identical output afterwards. This is the property a result edit would not have.
    let device = Device::new().await;
    device.record_the_finish("2026-04-11T08:26:41Z").await;

    let first = device.run_ok(&["derive"]).await;
    let second = device.run_ok(&["derive"]).await;

    assert_eq!(
        first, second,
        "re-derivation must reproduce the derivation, not merely agree with it"
    );
}

#[tokio::test]
async fn listing_shows_the_entries_with_who_typed_them_and_why() {
    let device = Device::new().await;

    let empty = device.json(&["manual", "list"]).await;
    assert_eq!(empty["entries"], 0);
    assert_eq!(empty["race"], "5K");

    device.record_the_finish("2026-04-11T08:26:41Z").await;
    device
        .json(&[
            "manual",
            "add",
            "--bib",
            "104",
            "--checkpoint",
            "start",
            "--at",
            "2026-04-11T08:00:04Z",
            "--reason",
            "started from behind the mat",
        ])
        .await;

    let listing = device.json(&["manual", "list"]).await;
    assert_eq!(listing["entries"], 2);

    let rows = listing["manual_entries"].as_array().expect("an array");
    assert_eq!(rows[0]["bib"], UNFINISHED, "oldest first: {listing}");
    assert_eq!(rows[1]["bib"], "104");
    assert!(rows[0]["seq"].as_u64() < rows[1]["seq"].as_u64());
    for row in rows {
        assert_eq!(row["actor"], "operator");
        assert!(
            row["name"]
                .as_str()
                .is_some_and(|name| name.contains("Runner")),
            "an operator reads bibs and names, not participant UUIDs: {row}"
        );
    }
}

#[tokio::test]
async fn an_entry_is_written_into_the_audit_trail() {
    // Attribution is the whole difference between a manual entry and a made-up time.
    let device = Device::new().await;
    device.record_the_finish("2026-04-11T08:26:41Z").await;

    let audit = device.json(&["audit", "--limit", "5"]).await;
    let entry = audit
        .as_array()
        .expect("an array")
        .iter()
        .find(|row| row["action"] == "manual.add")
        .unwrap_or_else(|| panic!("no manual.add in the audit trail: {audit}"));

    assert_eq!(entry["subject"], UNFINISHED);

    // The detail carries the claim itself, so the audit trail answers "what did they say"
    // and not merely "somebody typed something".
    let detail = entry["detail"].to_string();
    assert!(detail.contains("finish"), "got: {detail}");
    assert!(detail.contains("2026-04-11T08:26:41Z"), "got: {detail}");
    assert!(detail.contains("finish marshal"), "got: {detail}");
}

#[tokio::test]
async fn a_bib_that_is_not_on_the_roster_is_refused_before_anything_is_written() {
    let device = Device::new().await;

    let (stderr, ok) = run(
        &[
            "manual",
            "add",
            "--bib",
            "999",
            "--checkpoint",
            "finish",
            "--at",
            "2026-04-11T08:26:41Z",
            "--reason",
            "a typo",
        ],
        &device.database,
    )
    .await;

    assert!(!ok, "an entry for nobody must not be recorded");
    assert!(
        stderr.contains("no participant with bib 999"),
        "got: {stderr}"
    );
    assert!(
        !stderr.contains("  "),
        "a message that lost its line continuation prints a run of spaces: {stderr}"
    );

    let listing = device.json(&["manual", "list"]).await;
    assert_eq!(listing["entries"], 0, "nothing may be left behind");
}

#[tokio::test]
async fn a_time_that_is_not_rfc_3339_is_refused_with_an_example() {
    // No default of "now". The time somebody types an entry is not the time the runner
    // crossed the line, and a parser that guessed would record the first as the second.
    let device = Device::new().await;

    let (stderr, ok) = run(
        &[
            "manual",
            "add",
            "--bib",
            UNFINISHED,
            "--checkpoint",
            "finish",
            "--at",
            "08:26:41",
            "--reason",
            "a half-typed time",
        ],
        &device.database,
    )
    .await;

    assert!(!ok);
    assert!(stderr.contains("2026-04-11T08:17:32Z"), "got: {stderr}");
    assert!(
        !stderr.contains("  "),
        "a message that lost its line continuation prints a run of spaces: {stderr}"
    );
}

#[tokio::test]
async fn an_unknown_checkpoint_is_refused_and_lists_the_ones_there_are() {
    let device = Device::new().await;

    let (stderr, ok) = run(
        &[
            "manual",
            "add",
            "--bib",
            UNFINISHED,
            "--checkpoint",
            "waterstop",
            "--at",
            "2026-04-11T08:26:41Z",
            "--reason",
            "a checkpoint nobody configured",
        ],
        &device.database,
    )
    .await;

    assert!(!ok);
    assert!(stderr.contains("waterstop"), "got: {stderr}");
    assert!(
        stderr.contains("start") && stderr.contains("finish"),
        "the message has to say what is configured: {stderr}"
    );
}

/// Runs the binary and returns its stderr and whether it succeeded.
async fn run(args: &[&str], database: &Path) -> (String, bool) {
    let output = Command::new(BINARY)
        .args(args)
        .arg("--format")
        .arg("compact")
        .arg("--database")
        .arg(database)
        .output()
        .await
        .expect("run splitforge");
    (
        String::from_utf8(output.stderr).expect("stderr is UTF-8"),
        output.status.success(),
    )
}

async fn run_ok(args: &[&str], database: &Path) -> String {
    let output = Command::new(BINARY)
        .args(args)
        .arg("--format")
        .arg("compact")
        .arg("--database")
        .arg(database)
        .output()
        .await
        .expect("run splitforge");
    assert!(
        output.status.success(),
        "`splitforge {}` failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("output is UTF-8")
}

fn json(text: &str) -> serde_json::Value {
    let line = text.lines().last().unwrap_or_default();
    serde_json::from_str(line).unwrap_or_else(|error| panic!("not JSON ({error}): {text}"))
}
