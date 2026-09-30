//! The clock error budget, as ADR-0048 enforces it: a start refuses on a clock known to be
//! bad, and a publish never refuses but says, on each result, what the clock did to it.
//!
//! The start cases put a fake `chronyc` first on the binary's `PATH`, which is how the real
//! one is found, so the binary under test takes exactly the path a Pi would. The publish
//! cases write a clock step through storage, as `tests/clock.rs` does, because a test cannot
//! move the machine's clock.
//!
//! Unix only: the fake daemon is a shell script.

#![cfg(unix)]

use std::path::PathBuf;

use splitforge_domain::ClockStep;
use splitforge_storage::SqliteJournal;
use tempfile::TempDir;
use time::macros::datetime;
use tokio::process::Command;

/// The `splitforge` binary, built by Cargo for this test run.
const BINARY: &str = env!("CARGO_BIN_EXE_splitforge");

/// `chronyc -c tracking` from a daemon following nothing.
const UNSYNCED: &str = "00000000,,0,0.000000000,0.000000000,0.000000000,0.000000000,\
     0.000,0.000,0.000,0.000000000,0.000000000,0.0,Not synchronised";

/// The same from a daemon following a LAN server.
const NTP_SYNCED: &str = "C0A80101,192.168.1.1,3,1776153600.123456,0.000123,0.000045,\
     0.000067,12.345,0.010,0.250,0.001200,0.004500,64.0,Normal";

/// The same daemon, with a leap second scheduled for the end of the UTC day.
const LEAP_PENDING: &str = "C0A80101,192.168.1.1,3,1776153600.123456,0.000123,0.000045,\
     0.000067,12.345,0.010,0.250,0.001200,0.004500,64.0,Insert second";

/// A configured 5K, and a directory for a fake time daemon.
struct Device {
    directory: TempDir,
    database: PathBuf,
    /// What the fake `chronyc` answers, or `None` for no `chronyc` on `PATH` at all.
    tracking: Option<&'static str>,
}

impl Device {
    async fn new(tracking: Option<&'static str>) -> Self {
        let directory = TempDir::new().expect("temp dir");
        let database = directory.path().join("event.db");
        let device = Self {
            directory,
            database,
            tracking,
        };
        if let Some(answer) = tracking {
            use std::os::unix::fs::PermissionsExt as _;
            let bin = device.directory.path().join("bin");
            std::fs::create_dir(&bin).expect("bin");
            let script = bin.join("chronyc");
            std::fs::write(&script, format!("#!/bin/sh\necho '{answer}'\n")).expect("script");
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
                .expect("executable");
        }
        device.ok(&["fixture", "load", "--fixture", "five-k"]).await;
        device
    }

    async fn run(&self, args: &[&str]) -> (String, String, bool) {
        let mut command = Command::new(BINARY);
        command
            .args(args)
            .arg("--format")
            .arg("compact")
            .arg("--database")
            .arg(&self.database);
        // First on the path, so it is the `chronyc` found. With no fake, an empty directory
        // stands in for "no daemon installed" whatever the machine running this has.
        let bin = self.directory.path().join("bin");
        command.env(
            "PATH",
            if self.tracking.is_some() {
                format!("{}:/usr/bin:/bin", bin.display())
            } else {
                bin.display().to_string()
            },
        );
        let output = command.output().await.expect("run splitforge");
        (
            String::from_utf8(output.stdout).expect("stdout is UTF-8"),
            String::from_utf8(output.stderr).expect("stderr is UTF-8"),
            output.status.success(),
        )
    }

    async fn ok(&self, args: &[&str]) -> serde_json::Value {
        let (out, stderr, ok) = self.run(args).await;
        assert!(ok, "`splitforge {}` failed: {stderr}", args.join(" "));
        json(&out)
    }

    async fn start_detail(&self) -> serde_json::Value {
        let audit = self.ok(&["audit"]).await;
        audit
            .as_array()
            .expect("an array")
            .iter()
            .find(|entry| entry["action"] == "race.start")
            .expect("a race.start entry")["detail"]
            .clone()
    }
}

#[tokio::test]
async fn an_unsynchronized_clock_refuses_the_start_and_says_how_to_proceed() {
    let device = Device::new(Some(UNSYNCED)).await;

    let (_out, stderr, ok) = device.run(&["race", "start"]).await;
    assert!(!ok, "a start on a clock known to be bad must be refused");
    assert!(stderr.contains("not synchronized"), "got: {stderr}");
    assert!(stderr.contains("--force --note"), "got: {stderr}");
    assert!(stderr.contains("untrusted_device_clock"), "got: {stderr}");

    let status = device.ok(&["status"]).await;
    assert_eq!(status["running"], false, "a refused start is not a start");
}

#[tokio::test]
async fn forcing_past_the_clock_records_the_clock_and_the_reason() {
    let device = Device::new(Some(UNSYNCED)).await;
    let started = device
        .ok(&["race", "start", "--force", "--note", "GPS still acquiring"])
        .await;
    assert_eq!(started["forced"], true);
    assert_eq!(started["clock"]["state"], "unsynced");

    let detail = device.start_detail().await;
    assert_eq!(detail["reason"], "GPS still acquiring");
    assert_eq!(detail["clock"]["measurement"], "measured");
    assert_eq!(detail["clock"]["state"], "unsynced");
}

#[tokio::test]
async fn a_synchronized_clock_starts_and_is_recorded() {
    let device = Device::new(Some(NTP_SYNCED)).await;
    let started = device.ok(&["race", "start"]).await;
    assert!(started.get("forced").is_none(), "{started}");
    let clock = &device.start_detail().await["clock"];
    assert_eq!(clock["state"], "ntp_synced");
    assert_eq!(
        clock["leap_pending"], false,
        "ADR-0049: recorded either way"
    );
}

#[tokio::test]
async fn a_scheduled_leap_second_is_warned_about_and_recorded_but_does_not_block() {
    // ADR-0049: the clock is good and will step once at midnight UTC. The service records the
    // step and results spanning it are flagged, so the start goes ahead, saying so.
    let device = Device::new(Some(LEAP_PENDING)).await;

    let report = device.ok(&["doctor"]).await;
    let warned = report["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .any(|finding| {
            finding["check"] == "clock.source"
                && finding["detail"]
                    .as_str()
                    .is_some_and(|detail| detail.contains("leap second"))
        });
    assert!(warned, "{report}");
    assert_eq!(report["clock_source"]["leap_pending"], true);

    let started = device.ok(&["race", "start"]).await;
    assert!(started.get("forced").is_none(), "{started}");
    assert_eq!(device.start_detail().await["clock"]["leap_pending"], true);
}

#[tokio::test]
async fn no_time_daemon_starts_and_records_that_nothing_was_measured() {
    // A laptop, or a Pi without chrony. Not measured is not measured bad, and the start says
    // which it was so a reviewer can see the clock was never established.
    let device = Device::new(None).await;
    device.ok(&["race", "start"]).await;
    let clock = &device.start_detail().await["clock"];
    assert_eq!(clock["measurement"], "no_daemon_tool");
    assert!(clock["state"].is_null(), "{clock}");
}

#[tokio::test]
async fn a_gun_recorded_after_the_fact_does_not_ask_the_clock() {
    // `--at` is a gun somebody timed another way, and the reads it matters to are already
    // taken. Refusing it because the clock is bad *now* would block the recovery.
    let device = Device::new(Some(UNSYNCED)).await;
    let started = device
        .ok(&["race", "start", "--at", "2026-04-11T08:00:00Z"])
        .await;
    assert!(started.get("clock").is_none(), "{started}");
}

#[tokio::test]
async fn a_publish_across_a_clock_step_is_published_and_flags_every_result_it_spans() {
    let device = Device::new(None).await;
    device.ok(&["simulate", "--scenario", "five-k"]).await;

    let clean = device
        .run(&[
            "results",
            "publish",
            "--status",
            "provisional",
            "--reason",
            "before",
        ])
        .await;
    assert!(clean.2, "{}", clean.1);
    assert_eq!(json(&clean.0)["clock_caveats"], 0);
    assert!(
        !clean.1.contains("warning"),
        "nothing to warn about: {}",
        clean.1
    );

    // The gun is 08:00 and every finish is after 08:17. A forward step at 08:10 puts the two
    // ends of every finisher's time on different clocks.
    SqliteJournal::open(&device.database)
        .expect("open journal")
        .record_clock_step(&ClockStep {
            observed_before: datetime!(2026-04-11 08:10:00 UTC),
            observed_after: datetime!(2026-04-11 08:10:12 UTC),
            monotonic_ms: 10_000,
            step_ms: 2_000,
        })
        .expect("record a step");

    let (out, stderr, ok) = device
        .run(&[
            "results",
            "publish",
            "--status",
            "final",
            "--reason",
            "after",
            "--allow-unchanged",
        ])
        .await;
    assert!(ok, "a clock problem never refuses a publish: {stderr}");
    let published = json(&out);
    assert_eq!(
        published["clock_caveats"], published["finished"],
        "every finisher spans the step: {published}"
    );
    assert!(stderr.contains("warning:"), "said loudly: {stderr}");
    assert!(
        !stderr.contains("  "),
        "no lost line continuation: {stderr:?}"
    );

    let shown = device.ok(&["results", "show"]).await;
    for entry in shown["entries"].as_array().expect("entries") {
        let flagged = entry["flags"]
            .as_array()
            .is_some_and(|flags| flags.iter().any(|flag| flag == "clock_step_during_result"));
        assert_eq!(flagged, entry["status"] == "finished", "{entry}");
    }
}

fn json(text: &str) -> serde_json::Value {
    let line = text.lines().last().unwrap_or_default();
    serde_json::from_str(line).unwrap_or_else(|error| panic!("not JSON ({error}): {text}"))
}
