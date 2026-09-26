//! What the operator sees of the gaps the service recorded.
//!
//! Milestone 3a's exit criterion asks that every disconnection is detected and recorded as a
//! bounded gap. The service records them; `reader gaps` is how anyone checks, after a bench
//! session of pulled cables, that each pull left one and that each ended. Before it, the only
//! gap visible without SQL was the one `/health` shows while it is open.
//!
//! The gaps here are written directly through storage, the way the service's read path and
//! silence watchdog write them, rather than by pulling a cable a test does not have.

use std::path::Path;

use splitforge_domain::{GapDetection, ReaderId};
use splitforge_storage::SqliteJournal;
use tempfile::TempDir;
use time::{Duration, OffsetDateTime};
use tokio::process::Command;

/// The `splitforge` binary, built by Cargo for this test run.
const BINARY: &str = env!("CARGO_BIN_EXE_splitforge");

/// A configured 5K, with whatever gaps the test records.
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

    /// Opens a gap at `started`, and closes it `lasted_ms` later if that is given.
    fn gap(&self, detection: GapDetection, started: OffsetDateTime, lasted_ms: Option<i64>) {
        let mut journal = SqliteJournal::open(&self.database).expect("open journal");
        let reader = ReaderId::new("mat");
        journal
            .open_reader_gap(&reader, detection, started, 1_000, Some("the port ended"))
            .expect("open a gap");
        if let Some(lasted) = lasted_ms {
            journal
                .close_reader_gap(
                    &reader,
                    started + Duration::milliseconds(lasted),
                    1_000 + u64::try_from(lasted).expect("positive"),
                    None,
                )
                .expect("close the gap");
        }
    }
}

#[tokio::test]
async fn a_device_that_never_lost_its_reader_lists_nothing() {
    let device = Device::new().await;
    let listing = json(&run_ok(&["reader", "gaps"], &device.database).await);

    assert_eq!(listing["gaps"], 0);
    assert_eq!(listing["open"], 0);
    assert_eq!(listing["longest_ms"], serde_json::Value::Null);
    assert_eq!(listing["entries"], serde_json::json!([]));
}

#[tokio::test]
async fn every_gap_is_listed_newest_first_with_how_it_was_noticed_and_how_long_it_lasted() {
    let device = Device::new().await;
    let start = OffsetDateTime::now_utc() - Duration::hours(1);
    device.gap(GapDetection::Confirmed, start, Some(10_000));
    device.gap(
        GapDetection::Suspected,
        start + Duration::minutes(10),
        Some(60_500),
    );
    device.gap(GapDetection::Confirmed, start + Duration::minutes(20), None);

    let listing = json(&run_ok(&["reader", "gaps"], &device.database).await);

    assert_eq!(listing["gaps"], 3, "{listing}");
    assert_eq!(listing["open"], 1);
    assert_eq!(listing["confirmed"], 2);
    assert_eq!(listing["suspected"], 1);
    assert_eq!(listing["longest_ms"], 60_500, "the longest that ended");

    let entries = listing["entries"].as_array().expect("entries");
    let newest = &entries[0];
    assert_eq!(newest["open"], true, "the newest is the one still open");
    assert_eq!(newest["ended_at"], serde_json::Value::Null);
    assert_eq!(
        newest["duration_ms"],
        serde_json::Value::Null,
        "an open gap has no length yet, rather than one that looks final"
    );
    assert_eq!(newest["reader"], "mat");
    assert_eq!(newest["detail"], "the port ended");

    assert_eq!(entries[1]["detection"], "suspected");
    assert_eq!(entries[1]["duration_ms"], 60_500);
    assert_eq!(entries[2]["detection"], "confirmed");
    assert_eq!(entries[2]["duration_ms"], 10_000);
    assert!(
        entries[2]["seq"].as_u64() < entries[1]["seq"].as_u64(),
        "newest first, by sequence rather than by a clock that may have moved"
    );
}

#[tokio::test]
async fn a_limit_keeps_the_newest() {
    let device = Device::new().await;
    let start = OffsetDateTime::now_utc() - Duration::hours(1);
    device.gap(GapDetection::Confirmed, start, Some(1_000));
    device.gap(
        GapDetection::Confirmed,
        start + Duration::minutes(5),
        Some(2_000),
    );

    let listing = json(&run_ok(&["reader", "gaps", "--limit", "1"], &device.database).await);
    assert_eq!(listing["gaps"], 1);
    assert_eq!(listing["entries"][0]["duration_ms"], 2_000);
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
