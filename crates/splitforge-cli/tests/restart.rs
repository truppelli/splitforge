//! Durability: what survives the process going away.
//!
//! > ...both before and after a process restart.
//!
//! ## What these tests can and cannot prove
//!
//! They prove that reads acknowledged by the journal are present after the process that
//! wrote them is gone, including when it is killed without warning mid-write, and that
//! re-deriving from the recovered journal reproduces the same conclusions **bit for bit**,
//! identifiers included.
//!
//! They do **not** prove power-loss durability. `synchronous = FULL` asks the operating
//! system to flush to the storage device; whether the device honors that is a property of
//! the SD card and the power supply, and only pulling the plug on real hardware settles it.
//! That is Milestone 5, and `docs/roadmap.md` deliberately keeps it there.

mod common;

use std::process::Stdio;

use splitforge_domain::{RawReadJournal, StoredRawRead};
use splitforge_storage::SqliteJournal;
use splitforge_testkit::{FixtureEvent, five_k, multi_lap};
use tempfile::TempDir;
use tokio::process::Command;

/// The `splitforge` binary, built by Cargo for this test run.
const BINARY: &str = env!("CARGO_BIN_EXE_splitforge");

#[tokio::test]
async fn reopening_the_database_yields_the_same_derivation_bit_for_bit() {
    for fixture in [five_k(), multi_lap()] {
        let slug = fixture.slug;
        let slice = common::run(fixture).await;

        let recovered = common::reopen(&slice.database);
        assert_eq!(
            recovered, slice.reads,
            "{slug}: the journal changed across a reopen"
        );

        let rederived = common::derive_from(&slice.fixture, &recovered);
        assert_eq!(
            rederived, slice.derivation,
            "{slug}: re-deriving after a restart reached a different conclusion"
        );
    }
}

#[tokio::test]
async fn a_second_process_sees_every_read_the_first_one_wrote() {
    let directory = TempDir::new().expect("temp dir");
    let database = directory.path().join("event.db");

    let simulate = Command::new(BINARY)
        .args(["simulate", "--format", "compact", "--fixture", "five-k"])
        .arg("--database")
        .arg(&database)
        .output()
        .await
        .expect("run splitforge simulate");
    assert!(
        simulate.status.success(),
        "simulate failed: {}",
        String::from_utf8_lossy(&simulate.stderr)
    );
    let written: serde_json::Value =
        serde_json::from_slice(&simulate.stdout).expect("simulate emits JSON");
    let persisted = written["reads_persisted"].as_u64().expect("a count");
    assert!(persisted > 0);

    // A completely separate process, opening the same file from scratch.
    let derive = Command::new(BINARY)
        .args(["derive", "--format", "compact", "--fixture", "five-k"])
        .arg("--database")
        .arg(&database)
        .output()
        .await
        .expect("run splitforge derive");
    assert!(
        derive.status.success(),
        "derive failed: {}",
        String::from_utf8_lossy(&derive.stderr)
    );
    let derived: serde_json::Value =
        serde_json::from_slice(&derive.stdout).expect("derive emits JSON");

    assert_eq!(derived["raw_reads"].as_u64(), Some(persisted));
    assert_eq!(derived["accepted"], 24);
    assert_eq!(derived["timing_events"], 23);
    assert_eq!(derived["unassigned_crossings"], 1);
}

#[tokio::test]
async fn deriving_twice_in_two_processes_produces_identical_output() {
    // The strongest form of the idempotence claim: not "equivalent", but the same bytes,
    // produced by two processes that share nothing but the database file.
    let directory = TempDir::new().expect("temp dir");
    let database = directory.path().join("event.db");

    run_ok(&["simulate", "--fixture", "multi-lap"], &database).await;
    let first = run_ok(&["derive", "--fixture", "multi-lap"], &database).await;
    let second = run_ok(&["derive", "--fixture", "multi-lap"], &database).await;

    assert_eq!(first, second);
    assert!(first.contains("\"accepted\": 30"));
}

#[tokio::test]
async fn a_process_killed_mid_race_leaves_an_intact_journal() {
    let directory = TempDir::new().expect("temp dir");
    let database = directory.path().join("event.db");

    // Accelerated rather than immediate, so the process is genuinely mid-write — an
    // immediate run would very likely finish before the kill arrives, and a test that
    // silently stops testing anything is worse than no test. At 300x the 29-minute fixture
    // takes about six seconds, which leaves room on both sides of the kill.
    let mut child = Command::new(BINARY)
        .args(["simulate", "--fixture", "five-k", "--speed", "300"])
        .arg("--database")
        .arg(&database)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn splitforge simulate");

    // Wait for the writer to get properly underway rather than sleeping a fixed interval
    // and hoping. A slow CI runner would otherwise be killed during process startup, and
    // the test would fail for a reason that has nothing to do with durability.
    wait_for_reads(&database, 20).await;
    child.kill().await.expect("kill the timer mid-race");
    let _ = child.wait().await;

    let recovered: Vec<StoredRawRead> = SqliteJournal::open(&database)
        .expect("a killed process must leave a database that opens")
        .read_all()
        .expect("every surviving row must still decode");

    assert!(
        !recovered.is_empty(),
        "the process was killed after writing reads, so some must have survived"
    );

    let fixture = five_k();
    let last_planned = fixture
        .crossings
        .last()
        .expect("the fixture plans crossings")
        .at;
    let last_recorded = recovered
        .last()
        .expect("checked non-empty")
        .read
        .authoritative_timestamp();
    assert!(
        last_recorded < last_planned,
        "the race finished before the kill landed, so this test proved nothing about \
         interrupting a write; shorten the sleep or slow the simulation down"
    );

    // Sequence numbers are contiguous from 1: nothing was written, acknowledged, and then
    // lost from the middle.
    for (index, stored) in recovered.iter().enumerate() {
        assert_eq!(
            stored.seq,
            index as u64 + 1,
            "a gap in the journal sequence means a read went missing"
        );
    }

    // A partial journal is still a valid journal, and derivation must handle it without
    // special-casing anything.
    let derivation = common::derive_from(&fixture, &recovered);
    assert_eq!(
        derivation.accepted.len() + derivation.rejected.len(),
        recovered.len()
    );
}

#[tokio::test]
async fn a_restart_appends_rather_than_restarting_the_sequence() {
    // Two events in one database is not a supported deployment, but the journal must not
    // reuse a sequence number under any circumstances — that is the property that makes
    // "did we lose a read?" answerable at all.
    let directory = TempDir::new().expect("temp dir");
    let database = directory.path().join("event.db");
    let fixture = FixtureEvent::by_slug("five-k").expect("fixture");

    let mut journal = SqliteJournal::open(&database).expect("open");
    let first =
        splitforge_cli::into_journal(&fixture, &mut journal, 1, splitforge_cli::Speed::Immediate)
            .await
            .expect("first run");
    drop(journal);

    let mut journal = SqliteJournal::open(&database).expect("reopen");
    let second =
        splitforge_cli::into_journal(&fixture, &mut journal, 2, splitforge_cli::Speed::Immediate)
            .await
            .expect("second run");

    assert_eq!(first.first_seq, Some(1));
    assert_eq!(
        second.first_seq,
        first.last_seq.map(|seq| seq + 1),
        "the second run must continue the sequence, not restart it"
    );
    assert_eq!(
        journal.count().expect("count") as usize,
        first.reads_persisted + second.reads_persisted
    );
}

/// Blocks until the journal holds at least `wanted` reads, or gives up and says why.
///
/// Reads the database from a second connection while the writer still holds it, which WAL
/// mode permits — and which is itself worth exercising, since `reads tail` will do exactly
/// this during a live event.
async fn wait_for_reads(database: &std::path::Path, wanted: u64) {
    const ATTEMPTS: u32 = 100;

    for _ in 0..ATTEMPTS {
        if let Ok(journal) = SqliteJournal::open(database) {
            if journal.count().unwrap_or(0) >= wanted {
                return;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("the journal never reached {wanted} reads; the simulation is not writing");
}

/// Runs the binary against a database and returns stdout, failing loudly if it did not.
async fn run_ok(args: &[&str], database: &std::path::Path) -> String {
    let output = Command::new(BINARY)
        .args(args)
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
