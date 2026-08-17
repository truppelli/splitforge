//! Recovery: what happens when the database is the thing that broke.
//!
//! > If the SQLite file is corrupt mid-event, what is the operator supposed to do?
//! > — `docs/open-questions.md`, Q7
//!
//! Everything here runs through the binary, because the answer to that question is a
//! procedure an operator performs under pressure, not a library call. A recovery path that
//! only works when driven from Rust is a recovery path that does not work.
//!
//! ## What these tests prove
//!
//! That a database which is deleted, truncated, or overwritten with garbage costs nothing
//! but the time to run two commands — and specifically that **no read is lost**, including
//! the ones written after the last backup was taken.
//!
//! ## What they do not prove
//!
//! That an SD card honors `fsync`. These tests destroy a file deliberately; they do not
//! reproduce the physics of a card losing its cache during a brownout. That is Milestone 5
//! and it needs hardware — see `docs/roadmap.md`.

mod common;

use std::path::Path;

use tempfile::TempDir;
use tokio::process::Command;

/// The `splitforge` binary, built by Cargo for this test run.
const BINARY: &str = env!("CARGO_BIN_EXE_splitforge");

/// A configured 5K with its reads recorded and a snapshot taken before the gun.
struct TimedRace {
    _directory: TempDir,
    database: std::path::PathBuf,
    snapshot: std::path::PathBuf,
    reads: u64,
}

async fn timed_race() -> TimedRace {
    let directory = TempDir::new().expect("temp dir");
    let database = directory.path().join("event.db");
    let snapshot = directory.path().join("pre-race.db");

    run_ok(&["fixture", "load", "--fixture", "five-k"], &database).await;

    // The pre-race snapshot: taken when the configuration is complete and no read exists.
    // This is the backup an organizer actually has when something goes wrong at 09:40.
    run_ok(
        &["backup", "create", snapshot.to_str().expect("utf-8 path")],
        &database,
    )
    .await;

    let simulated = json(&run_ok(&["simulate", "--scenario", "five-k"], &database).await);
    let reads = simulated["reads_persisted"].as_u64().expect("a count");
    assert!(reads > 0);

    TimedRace {
        _directory: directory,
        database,
        snapshot,
        reads,
    }
}

#[tokio::test]
async fn every_read_is_written_to_the_sidecar_as_well_as_the_database() {
    let race = timed_race().await;

    let sidecar = sidecar_of(&race.database);
    assert!(sidecar.exists(), "the sidecar must sit beside the database");

    let lines = std::fs::read_to_string(&sidecar)
        .expect("read sidecar")
        .lines()
        .filter(|line| !line.is_empty())
        .count();
    assert_eq!(
        lines as u64, race.reads,
        "one line per read, or the sidecar is not a superset of the journal"
    );

    // And it is text. The whole argument for this format is that a person can read it
    // without tooling, so assert that a person can.
    let first = std::fs::read_to_string(&sidecar).expect("read");
    let first = first.lines().next().expect("a line");
    assert!(first.starts_with("SFJ1 "), "got: {first}");
    assert!(first.contains("\"chip\":"), "got: {first}");

    assert_clean(&race.database).await;
}

#[tokio::test]
async fn a_database_destroyed_mid_event_is_restored_without_losing_a_single_read() {
    // The milestone claim, end to end. Take the pre-race snapshot, run the race, destroy
    // the database outright, then get everything back with two commands.
    let race = timed_race().await;
    let before = json(&run_ok(&["derive"], &race.database).await);

    destroy(&race.database);

    let restored = json(
        &run_ok(
            &[
                "backup",
                "restore",
                race.snapshot.to_str().expect("utf-8 path"),
                "--replace",
            ],
            &race.database,
        )
        .await,
    );
    assert_eq!(
        restored["raw_reads"], 0,
        "a pre-race snapshot knows the configuration and none of the reads"
    );

    let recovered = json(&run_ok(&["recover"], &race.database).await);
    assert_eq!(
        recovered["replayed_into_database"].as_u64(),
        Some(race.reads),
        "the sidecar must supply every read the snapshot predates"
    );
    assert_eq!(recovered["corrupt_lines"], 0);

    // The real assertion: the race scores the same afterwards. Not "roughly the same
    // number of reads" — the same conclusions, from the same evidence.
    let after = json(&run_ok(&["derive"], &race.database).await);
    assert_eq!(
        after, before,
        "a recovered event must derive identically to the one that was destroyed"
    );

    assert_clean(&race.database).await;
}

#[tokio::test]
async fn a_database_overwritten_with_garbage_is_survivable() {
    // Corruption is rarely a clean deletion. This is closer: the file is present, the
    // right sort of size, and completely unreadable.
    let race = timed_race().await;
    let before = json(&run_ok(&["derive"], &race.database).await);

    destroy(&race.database);
    std::fs::write(&race.database, vec![0xA5; 64 * 1024]).expect("scribble over it");

    run_ok(
        &[
            "backup",
            "restore",
            race.snapshot.to_str().expect("utf-8 path"),
            "--replace",
        ],
        &race.database,
    )
    .await;
    run_ok(&["recover"], &race.database).await;

    assert_eq!(json(&run_ok(&["derive"], &race.database).await), before);
}

#[tokio::test]
async fn the_sidecar_alone_rebuilds_the_evidence_when_there_is_no_snapshot_at_all() {
    // The worse case: nobody took a backup. Configuration is gone and has to be re-entered,
    // but the evidence — the part that cannot be recreated by typing — comes back.
    let race = timed_race().await;
    let sidecar = sidecar_of(&race.database);

    let fresh = race.database.with_file_name("rebuilt.db");
    let recovered = json(
        &run_ok(
            &[
                "recover",
                "--sidecar",
                sidecar.to_str().expect("utf-8 path"),
            ],
            &fresh,
        )
        .await,
    );
    assert_eq!(
        recovered["replayed_into_database"].as_u64(),
        Some(race.reads)
    );

    let reads = json(&run_ok(&["reads", "--limit", "1"], &fresh).await);
    assert_eq!(reads["seq"], 1, "the rebuilt journal numbers from one");
}

#[tokio::test]
async fn doctor_reports_a_divergence_and_declines_to_fix_it() {
    // `doctor` is a diagnosis. A diagnostic command that silently repairs is a command
    // whose output nobody can trust, because it describes a state it just changed.
    let race = timed_race().await;
    destroy(&race.database);

    // `doctor` exits non-zero when it finds errors, deliberately, so that
    // `doctor && race start` is a usable pre-race gate. The report is still on stdout.
    let (report, ok) = run(&["doctor"], &race.database).await;
    assert!(!ok, "a divergence this size must fail the pre-race gate");
    let report = json(&report);
    let findings = report["findings"].as_array().expect("findings");
    let sidecar_finding = findings
        .iter()
        .find(|f| f["check"] == "journal.sidecar")
        .unwrap_or_else(|| panic!("expected a journal.sidecar finding, got {findings:?}"));

    assert_eq!(sidecar_finding["severity"], "error");
    assert!(
        sidecar_finding["detail"]
            .as_str()
            .expect("detail")
            .contains("splitforge recover"),
        "a finding must name the command that fixes it: {sidecar_finding:?}"
    );

    // Run it again: the divergence is still there, because doctor did not touch it.
    let (again, _) = run(&["doctor"], &race.database).await;
    assert_eq!(json(&again)["errors"], report["errors"]);
}

#[tokio::test]
async fn recovering_twice_does_not_duplicate_the_evidence() {
    let race = timed_race().await;
    destroy(&race.database);
    run_ok(
        &[
            "backup",
            "restore",
            race.snapshot.to_str().expect("utf-8 path"),
            "--replace",
        ],
        &race.database,
    )
    .await;

    let first = json(&run_ok(&["recover"], &race.database).await);
    assert_eq!(first["replayed_into_database"].as_u64(), Some(race.reads));

    let second = json(&run_ok(&["recover"], &race.database).await);
    assert_eq!(
        second["replayed_into_database"], 0,
        "recovery must be idempotent, or an anxious operator doubles the race"
    );
    assert_eq!(second["backfilled_into_sidecar"], 0);

    let status = json(&run_ok(&["status"], &race.database).await);
    assert_eq!(status["raw_reads"].as_u64(), Some(race.reads));
}

#[tokio::test]
async fn restoring_over_a_live_database_is_refused_unless_the_operator_says_replace() {
    let race = timed_race().await;

    let output = Command::new(BINARY)
        .args([
            "backup",
            "restore",
            race.snapshot.to_str().expect("utf-8 path"),
        ])
        .arg("--database")
        .arg(&race.database)
        .output()
        .await
        .expect("run splitforge");

    assert!(
        !output.status.success(),
        "restoring over a database must fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--replace"), "got: {stderr}");

    // And the live database is untouched, which is the part that matters.
    let status = json(&run_ok(&["status"], &race.database).await);
    assert_eq!(status["raw_reads"].as_u64(), Some(race.reads));
}

#[tokio::test]
async fn a_restore_keeps_the_database_it_replaced() {
    let race = timed_race().await;

    let restored = json(
        &run_ok(
            &[
                "backup",
                "restore",
                race.snapshot.to_str().expect("utf-8 path"),
                "--replace",
            ],
            &race.database,
        )
        .await,
    );

    let displaced = restored["displaced"].as_array().expect("displaced");
    assert!(!displaced.is_empty(), "the old database must be kept");
    for path in displaced {
        let path = path.as_str().expect("a path");
        assert!(
            Path::new(path).exists(),
            "{path} was reported as displaced but is not on disk"
        );
    }
}

#[tokio::test]
async fn the_restore_is_recorded_in_the_audit_trail_of_the_database_it_produced() {
    let race = timed_race().await;
    run_ok(
        &[
            "backup",
            "restore",
            race.snapshot.to_str().expect("utf-8 path"),
            "--replace",
        ],
        &race.database,
    )
    .await;
    run_ok(&["recover"], &race.database).await;

    let audit = json(&run_ok(&["audit"], &race.database).await);
    let actions: Vec<&str> = audit
        .as_array()
        .expect("an array")
        .iter()
        .filter_map(|entry| entry["action"].as_str())
        .collect();

    assert!(
        actions.contains(&"backup.restore"),
        "a restore must leave a record: {actions:?}"
    );
    assert!(
        actions.contains(&"journal.recover"),
        "so must a recovery: {actions:?}"
    );
}

// ---- helpers --------------------------------------------------------------------

fn sidecar_of(database: &Path) -> std::path::PathBuf {
    let mut name = database.as_os_str().to_owned();
    name.push(".reads.jsonl");
    std::path::PathBuf::from(name)
}

/// Removes the database and everything SQLite keeps beside it. Never the sidecar.
fn destroy(database: &Path) {
    for suffix in ["", "-wal", "-shm"] {
        let mut victim = database.as_os_str().to_owned();
        victim.push(suffix);
        let _ = std::fs::remove_file(std::path::PathBuf::from(victim));
    }
}

/// Runs the binary and returns stdout plus whether it succeeded.
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
    let ok = output.status.success();
    (
        String::from_utf8(output.stdout).expect("output is UTF-8"),
        ok,
    )
}

async fn assert_clean(database: &Path) {
    let report = json(&run_ok(&["doctor"], database).await);
    assert_eq!(report["errors"], 0, "doctor found errors: {report}");
}

fn json(text: &str) -> serde_json::Value {
    serde_json::from_str(text).unwrap_or_else(|error| panic!("not JSON ({error}): {text}"))
}

/// Runs the binary against a database and returns stdout, failing loudly if it did not.
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
