//! What the operator sees after the device's clock has jumped.
//!
//! The service detects a step and records it; these tests cover the other half — that a
//! recorded step reaches the two places somebody actually looks, `doctor` and
//! `clock steps`, before a result gets published on top of it.
//!
//! The steps here are written directly through storage rather than by moving the machine's
//! clock, which a test cannot do. What that costs is covered elsewhere:
//! `splitforge-domain`'s tests own the arithmetic that decides whether a pair of samples is
//! a step, and `docs/clock-and-time-discipline.md` records the end-to-end run against a
//! deliberately stepped clock.

mod common;

use std::path::Path;

use splitforge_domain::ClockStep;
use splitforge_storage::SqliteJournal;
use tempfile::TempDir;
use time::{Duration, OffsetDateTime};
use tokio::process::Command;

/// The `splitforge` binary, built by Cargo for this test run.
const BINARY: &str = env!("CARGO_BIN_EXE_splitforge");

/// A configured 5K, with whatever clock history the test asks for.
struct Device {
    _directory: TempDir,
    database: std::path::PathBuf,
}

impl Device {
    async fn new() -> Self {
        let directory = TempDir::new().expect("temp dir");
        let database = directory.path().join("event.db");
        run_ok(&["fixture", "load", "--fixture", "five-k"], &database).await;
        Self {
            _directory: directory,
            database,
        }
    }

    /// Records one step of `step_ms`, the way the running service would have.
    fn record(&self, step_ms: i64) {
        let mut journal = SqliteJournal::open(&self.database).expect("open journal");
        let before = OffsetDateTime::now_utc();
        journal
            .record_clock_step(&ClockStep {
                observed_before: before,
                observed_after: before + Duration::milliseconds(10_000 + step_ms),
                monotonic_ms: 10_000,
                step_ms,
            })
            .expect("record a step");
    }
}

#[tokio::test]
async fn a_clean_device_says_nothing_about_its_clock() {
    let device = Device::new().await;
    let report = json(&run_ok(&["doctor"], &device.database).await);

    assert_eq!(report["errors"], 0);
    assert_eq!(report["warnings"], 0, "got: {report}");

    let listing = json(&run_ok(&["clock", "steps"], &device.database).await);
    assert_eq!(listing["steps"], 0);
    assert!(listing["largest_ms"].is_null());
    assert_eq!(listing["entries"].as_array().expect("an array").len(), 0);
}

#[tokio::test]
async fn doctor_warns_about_a_jump_and_names_its_size() {
    let device = Device::new().await;
    device.record(3_600_000);

    let report = json(&run_ok(&["doctor"], &device.database).await);

    assert_eq!(report["warnings"], 1, "got: {report}");
    assert_eq!(
        report["errors"], 0,
        "a jump is not a reason to refuse to operate — it is a reason to know"
    );

    let finding = &report["findings"][0];
    assert_eq!(finding["check"], "clock.steps");
    assert_eq!(finding["severity"], "warning");

    let detail = finding["detail"].as_str().expect("a message");
    assert!(detail.contains("3600000 ms"), "got: {detail}");
    assert!(detail.contains("forwards"), "got: {detail}");
    assert!(
        detail.contains("clock steps"),
        "the finding has to say where to look next: {detail}"
    );
}

#[tokio::test]
async fn the_largest_jump_keeps_its_direction() {
    // The dangerous direction is backwards: it can give a later read an earlier timestamp
    // than one recorded before it. Reporting the biggest *signed* number would report the
    // forward jump here and bury the backward one.
    let device = Device::new().await;
    device.record(2_000);
    device.record(-45_000);
    device.record(900);

    let report = json(&run_ok(&["doctor"], &device.database).await);
    let detail = report["findings"][0]["detail"]
        .as_str()
        .expect("a message")
        .to_owned();

    assert!(detail.contains("3 time(s)"), "got: {detail}");
    assert!(detail.contains("backwards by 45000 ms"), "got: {detail}");

    let listing = json(&run_ok(&["clock", "steps"], &device.database).await);
    assert_eq!(listing["steps"], 3);
    assert_eq!(listing["largest_ms"], -45_000);
}

#[tokio::test]
async fn the_listing_is_newest_first_and_honors_its_limit() {
    let device = Device::new().await;
    for step_ms in [1_000, 2_000, 3_000] {
        device.record(step_ms);
    }

    let listing = json(&run_ok(&["clock", "steps", "--limit", "2"], &device.database).await);

    assert_eq!(listing["steps"], 2);
    let entries = listing["entries"].as_array().expect("an array");
    assert_eq!(entries[0]["step_ms"], 3_000, "newest first");
    assert_eq!(entries[1]["step_ms"], 2_000);
    assert_eq!(
        entries[0]["seq"], 3,
        "the sequence is the trustworthy order"
    );
    assert_eq!(entries[0]["direction"], "forwards");
    assert_eq!(entries[0]["monotonic_ms"], 10_000);
}

#[tokio::test]
async fn a_bundle_carries_the_jump_because_it_says_nothing_about_anybody() {
    // `clock.steps` is on the bundle's fail-closed allowlist: counts and milliseconds, no
    // race, no participant, no path. A maintainer looking at a bundle should be able to see
    // that the device's clock moved, because it explains times that otherwise look wrong.
    let device = Device::new().await;
    device.record(-45_000);

    let bundle = device.database.with_file_name("bundle.json");
    run_ok(
        &["doctor", "--bundle", bundle.to_str().expect("utf-8")],
        &device.database,
    )
    .await;

    let raw = std::fs::read_to_string(&bundle).expect("read the bundle");
    let json: serde_json::Value = serde_json::from_str(&raw).expect("valid JSON");

    assert_eq!(json["device"]["clock_steps"], 1);
    assert_eq!(json["device"]["largest_clock_step_ms"], -45_000);

    let finding = &json["doctor"]["findings"][0];
    assert_eq!(finding["check"], "clock.steps");
    assert!(
        finding["detail"].as_str().is_some(),
        "an allowlisted check travels with its message, not withheld: {finding}"
    );
    assert!(finding["detail_withheld"].is_null(), "got: {finding}");
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
