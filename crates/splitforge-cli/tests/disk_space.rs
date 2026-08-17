//! The free-space floor: refusing to start a race that cannot finish recording.
//!
//! > Warn at a configurable threshold, refuse non-essential writes first (diagnostic
//! > captures, exports), keep the journal writable as long as possible, surface loudly.
//! > — `docs/architecture.md` § 4
//!
//! The below-the-floor cases here work by raising the floor above the machine's actual free
//! space rather than by filling a disk. That makes them deterministic and fast, and it
//! tests the decision — which is the part with the logic in it. What it does **not** test
//! is what SQLite and the sidecar do when a write genuinely fails partway; that needs a
//! real full volume and belongs with the rest of the hardware work in Milestone 5.

mod common;

use std::path::Path;

use tempfile::TempDir;
use tokio::process::Command;

/// The `splitforge` binary, built by Cargo for this test run.
const BINARY: &str = env!("CARGO_BIN_EXE_splitforge");

/// A floor no real machine can satisfy: about a petabyte.
const IMPOSSIBLE_FLOOR_MB: &str = "999999999";

async fn configured() -> (TempDir, std::path::PathBuf) {
    let directory = TempDir::new().expect("temp dir");
    let database = directory.path().join("event.db");
    run_ok(&["fixture", "load", "--fixture", "five-k"], &database).await;
    (directory, database)
}

#[tokio::test]
async fn a_race_starts_normally_when_there_is_room() {
    let (_dir, database) = configured().await;

    let started = json(&run_ok(&["race", "start"], &database).await);
    assert_eq!(started["action"], "start");
    assert!(
        started["free_mb"].as_u64().is_some(),
        "a start records what the disk looked like: {started}"
    );
    assert!(
        started.get("forced").is_none(),
        "an unforced start must not claim to be forced: {started}"
    );
}

#[tokio::test]
async fn a_race_refuses_to_start_below_the_floor_and_says_how_to_proceed() {
    let (_dir, database) = configured().await;
    run_ok(
        &["device", "set", "--min-free-mb", IMPOSSIBLE_FLOOR_MB],
        &database,
    )
    .await;

    let (_out, stderr, ok) = run(&["race", "start"], &database).await;
    assert!(!ok, "starting below the floor must fail");

    // The message has to carry the numbers and the way out. Somebody is reading this
    // outdoors with somewhere else to be.
    assert!(stderr.contains("below the"), "got: {stderr}");
    assert!(stderr.contains("device set --min-free-mb"), "got: {stderr}");
    assert!(stderr.contains("--force"), "got: {stderr}");

    // And nothing was recorded — a refused start is not a start.
    let status = json(&run_ok(&["status"], &database).await);
    assert_eq!(status["running"], false);
}

#[tokio::test]
async fn forcing_past_the_floor_works_and_leaves_the_reason_in_the_audit_trail() {
    let (_dir, database) = configured().await;
    run_ok(
        &["device", "set", "--min-free-mb", IMPOSSIBLE_FLOOR_MB],
        &database,
    )
    .await;

    let started = json(
        &run_ok(
            &[
                "race",
                "start",
                "--force",
                "--note",
                "USB SSD attached, floor is stale",
            ],
            &database,
        )
        .await,
    );
    assert_eq!(started["forced"], true);

    let audit = json(&run_ok(&["audit"], &database).await);
    let start = audit
        .as_array()
        .expect("an array")
        .iter()
        .find(|entry| entry["action"] == "race.start")
        .expect("a race.start entry");

    // `AuditView::detail` is already parsed JSON, not a string holding JSON.
    let detail = &start["detail"];

    assert_eq!(detail["forced"], true);
    assert_eq!(detail["reason"], "USB SSD attached, floor is stale");
    assert!(
        detail["free_mb"].as_u64().is_some(),
        "the override must record what the disk actually had: {detail}"
    );
}

#[tokio::test]
async fn the_parser_refuses_to_force_a_start_without_a_reason() {
    let (_dir, database) = configured().await;
    let (_out, stderr, ok) = run(&["race", "start", "--force"], &database).await;
    assert!(!ok, "--force alone must not be accepted");
    assert!(stderr.contains("--note"), "got: {stderr}");
}

#[tokio::test]
async fn doctor_reports_the_floor_as_an_error_and_names_the_consequence() {
    let (_dir, database) = configured().await;
    run_ok(
        &["device", "set", "--min-free-mb", IMPOSSIBLE_FLOOR_MB],
        &database,
    )
    .await;

    let (out, _stderr, ok) = run(&["doctor"], &database).await;
    assert!(!ok, "an unstartable race must fail the pre-race gate");

    let finding = json(&out)["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .find(|f| f["check"] == "storage.free_space")
        .cloned()
        .expect("a storage.free_space finding");

    assert_eq!(finding["severity"], "error");
    assert!(
        finding["detail"]
            .as_str()
            .expect("detail")
            .contains("race start"),
        "the finding must say what it will block: {finding}"
    );
}

#[tokio::test]
async fn doctor_is_clean_at_the_default_floor() {
    // The regression this guards: a default so high that a normal machine fails its own
    // pre-race check would make the whole gate noise, and noise gets ignored.
    let (_dir, database) = configured().await;
    let report = json(&run_ok(&["doctor"], &database).await);
    assert_eq!(report["errors"], 0, "got: {report}");

    let space_findings: Vec<&serde_json::Value> = report["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .filter(|f| f["check"] == "storage.free_space")
        .collect();
    assert!(
        space_findings.is_empty(),
        "a machine with room should hear nothing: {space_findings:?}"
    );
}

#[tokio::test]
async fn a_backup_is_shed_before_the_journal_is() {
    // architecture.md § 4 orders the shedding: non-essential writes go first and the
    // journal keeps writing as long as possible. A snapshot is the large non-essential one.
    let (_dir, database) = configured().await;
    run_ok(
        &["device", "set", "--min-free-mb", IMPOSSIBLE_FLOOR_MB],
        &database,
    )
    .await;

    let snapshot = database.with_file_name("snap.db");
    let (_out, stderr, ok) = run(
        &["backup", "create", snapshot.to_str().expect("utf-8")],
        &database,
    )
    .await;
    assert!(!ok, "a snapshot below the floor must be refused");
    assert!(
        stderr.contains("journal keeps writing"),
        "the message must say what is still working: {stderr}"
    );
    assert!(!snapshot.exists(), "nothing should have been written");

    // The journal is the thing that must survive the shedding, so prove it still accepts
    // reads with the floor still impossibly high.
    let simulated = json(&run_ok(&["simulate", "--scenario", "five-k"], &database).await);
    assert_eq!(
        simulated["reads_persisted"].as_u64(),
        Some(638),
        "shedding a backup must not stop the timer"
    );
}

#[tokio::test]
async fn the_floor_round_trips_through_device_show() {
    let (_dir, database) = configured().await;

    let before = json(&run_ok(&["device", "show"], &database).await);
    assert_eq!(before["min_free_mb"], 256, "the documented default");
    assert_eq!(before["above_floor"], true);
    assert!(before["free_mb"].as_u64().is_some());
    assert!(before["total_mb"].as_u64().expect("total") > 0);

    run_ok(&["device", "set", "--min-free-mb", "512"], &database).await;
    let after = json(&run_ok(&["device", "show"], &database).await);
    assert_eq!(after["min_free_mb"], 512);

    let audit = json(&run_ok(&["audit"], &database).await);
    assert!(
        audit
            .as_array()
            .expect("an array")
            .iter()
            .any(|entry| entry["action"] == "device.set"),
        "changing a device setting must be recorded"
    );
}

// ---- helpers --------------------------------------------------------------------

fn json(text: &str) -> serde_json::Value {
    serde_json::from_str(text).unwrap_or_else(|error| panic!("not JSON ({error}): {text}"))
}

async fn run(args: &[&str], database: &Path) -> (String, String, bool) {
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
        String::from_utf8(output.stdout).expect("stdout is UTF-8"),
        String::from_utf8(output.stderr).expect("stderr is UTF-8"),
        output.status.success(),
    )
}

async fn run_ok(args: &[&str], database: &Path) -> String {
    let (out, stderr, ok) = run(args, database).await;
    assert!(ok, "`splitforge {}` failed: {stderr}", args.join(" "));
    out
}
