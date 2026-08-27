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
    assert!(
        finding(&report, "clock.steps").is_none(),
        "a device that has recorded no step must not report one: {report}"
    );

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

    assert_eq!(
        report["errors"], 0,
        "a jump is not a reason to refuse to operate — it is a reason to know"
    );

    let finding = finding(&report, "clock.steps").expect("a clock.steps finding");
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
    let detail = finding(&report, "clock.steps").expect("a clock.steps finding")["detail"]
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

    let finding = finding(&json["doctor"], "clock.steps").expect("a clock.steps finding");
    assert!(
        finding["detail"].as_str().is_some(),
        "an allowlisted check travels with its message, not withheld: {finding}"
    );
    assert!(finding["detail_withheld"].is_null(), "got: {finding}");
}

#[tokio::test]
async fn doctor_always_says_what_the_time_source_is_or_why_it_could_not_tell() {
    // The field is unconditional. An operator checking a device before a race has to be
    // able to see that the question was *asked* — an absent field would leave "this clock
    // is following nothing" and "nobody looked" indistinguishable, and those call for
    // opposite reactions.
    let device = Device::new().await;
    let report = json(&run_ok(&["doctor"], &device.database).await);

    let source = &report["clock_source"];
    assert!(
        source.is_object(),
        "clock_source is always present: {report}"
    );

    // Which of the four this machine gives depends on whether it runs chrony, which a test
    // cannot arrange. That it is one of them is the contract, and the unit tests in
    // `clock_source.rs` own each branch individually.
    let measurement = source["measurement"].as_str().expect("a measurement token");
    assert!(
        matches!(
            measurement,
            "measured" | "no_daemon_tool" | "daemon_unreachable" | "unreadable"
        ),
        "unexpected measurement token {measurement:?}: {source}"
    );
    assert!(
        source["detail"]
            .as_str()
            .is_some_and(|text| !text.is_empty()),
        "the token is for a machine; the detail is the sentence a person reads: {source}"
    );

    // The defect this guards has shipped twice in this repository: a multi-line message
    // that lost its line-continuation backslash renders as a run of spaces. It is
    // invisible in a passing suite because the string is still one string.
    let detail = source["detail"].as_str().expect("a detail");
    assert!(!detail.contains("  "), "collapsed continuation: {detail:?}");

    if measurement == "measured" {
        assert!(source["state"].as_str().is_some(), "got: {source}");
    } else {
        assert!(
            source["state"].is_null(),
            "not measuring is not the same as measuring `unsynced`: {source}"
        );
    }
}

#[tokio::test]
async fn a_bundle_says_which_kind_of_clock_without_naming_the_network() {
    // A set of finish times that are all shifted by the same amount is explained by the
    // device's time source and by almost nothing else, so the bundle carries it. What it
    // must not carry is the reference's *name*: on a race-day LAN that is an internal
    // address, and ADR-0020's rule is that a bundle is safe to attach unread.
    let device = Device::new().await;
    let bundle = device.database.with_file_name("bundle.json");
    run_ok(
        &["doctor", "--bundle", bundle.to_str().expect("utf-8")],
        &device.database,
    )
    .await;

    let raw = std::fs::read_to_string(&bundle).expect("read the bundle");
    let bundled: serde_json::Value = serde_json::from_str(&raw).expect("valid JSON");
    let source = &bundled["device"]["clock_source"];

    assert!(source.is_object(), "got: {bundled}");
    assert!(source["measurement"].as_str().is_some(), "got: {source}");

    // The two fields `doctor` shows on the device and a bundle deliberately drops. Asserted
    // by absence from the parsed section *and* from the file's bytes, because the leak that
    // matters is the one in whichever field nobody thought to check.
    assert!(
        source["reference"].is_null(),
        "the reference name stays on the device: {source}"
    );
    assert!(
        source.get("detail").is_none(),
        "failure text is program output and is withheld, not filtered: {source}"
    );

    let on_device = json(&run_ok(&["doctor"], &device.database).await);
    if let Some(reference) = on_device["clock_source"]["reference"].as_str()
        && !reference.is_empty()
    {
        assert!(
            !raw.contains(reference),
            "the bundle's bytes contain the reference name {reference:?}"
        );
    }
}

/// The one `doctor` finding with the given check name, if it fired.
///
/// Looked up by name rather than by index because `doctor`'s finding list is no longer
/// fixed: `clock.source` fires or stays quiet depending on what time daemon the host
/// happens to run, which a test cannot control and should not assert on.
fn finding<'a>(container: &'a serde_json::Value, check: &str) -> Option<&'a serde_json::Value> {
    container["findings"]
        .as_array()?
        .iter()
        .find(|finding| finding["check"] == check)
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
