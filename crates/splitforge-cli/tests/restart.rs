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

        let rederived = common::derive_from(&slice.config, &recovered);
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

    seed_config(&database, "five-k").await;
    let written: serde_json::Value = serde_json::from_str(
        &run_ok(
            &["simulate", "--format", "compact", "--scenario", "five-k"],
            &database,
        )
        .await,
    )
    .expect("simulate emits JSON");
    let persisted = written["reads_persisted"].as_u64().expect("a count");
    assert!(persisted > 0);

    // A completely separate process, opening the same file from scratch — and reading its
    // roster, checkpoints, and policy out of the database rather than from a fixture.
    let derived: serde_json::Value =
        serde_json::from_str(&run_ok(&["derive", "--format", "compact"], &database).await)
            .expect("derive emits JSON");

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

    seed_config(&database, "multi-lap").await;
    run_ok(&["simulate", "--scenario", "multi-lap"], &database).await;
    let first = run_ok(&["derive"], &database).await;
    let second = run_ok(&["derive"], &database).await;

    assert_eq!(first, second);
    assert!(first.contains("\"accepted\": 30"));
}

#[tokio::test]
async fn a_process_killed_mid_race_leaves_an_intact_journal() {
    let directory = TempDir::new().expect("temp dir");
    let database = directory.path().join("event.db");

    // Seed with a completed run first. The point of this test is what a kill does to reads
    // that were already acknowledged, and seeding makes that question answerable without
    // depending on how much the *killed* process managed to write — which is a function of
    // the runner's fsync latency and therefore not something to assert on.
    seed_config(&database, "five-k").await;
    run_ok(&["simulate", "--scenario", "five-k"], &database).await;
    let acknowledged = SqliteJournal::open(&database)
        .expect("seed run leaves a readable database")
        .count()
        .expect("count seeded reads");
    assert!(acknowledged > 0, "the seed run wrote nothing");

    // Now a second run, accelerated so the process is genuinely mid-write when it dies. An
    // immediate run would very likely finish before the kill arrives, and a test that
    // silently stops testing anything is worse than no test. A different race, on a
    // different reader, so its reads are distinguishable from the seeded ones.
    seed_config(&database, "multi-lap").await;
    let mut child = Command::new(BINARY)
        .args([
            "simulate",
            "--race",
            "Criterium",
            "--scenario",
            "multi-lap",
            "--speed",
            "300",
        ])
        .arg("--database")
        .arg(&database)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn splitforge simulate");

    // Long enough that the writer is underway on any runner, short enough that the 300x
    // criterium — about five seconds of wall clock — is nowhere near finished.
    tokio::time::sleep(std::time::Duration::from_millis(1_500)).await;
    assert!(
        child.try_wait().expect("poll child").is_none(),
        "the simulation exited on its own before it could be killed"
    );
    child.kill().await.expect("kill the timer mid-race");
    let _ = child.wait().await;

    let recovered: Vec<StoredRawRead> = SqliteJournal::open(&database)
        .expect("a killed process must leave a database that opens")
        .read_all()
        .expect("every surviving row must still decode");

    assert!(
        recovered.len() as u64 >= acknowledged,
        "a kill destroyed reads that had already been acknowledged: {} survived of {}",
        recovered.len(),
        acknowledged
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
    // special-casing anything. Derived against the seeded fixture, so the criterium's
    // partial reads land as unmapped rather than as crossings — which is itself the right
    // behavior: an unrecognized reader withholds interpretation, it does not discard.
    let config = common::reload_config(&database, five_k().config.race.id);
    let derivation = common::derive_from(&config, &recovered);
    assert_eq!(
        derivation.accepted.len() + derivation.rejected.len(),
        recovered.len()
    );
}

#[tokio::test]
async fn a_second_process_can_read_the_journal_while_a_race_is_running() {
    // What an operator does at 6 a.m. to check the timer is alive. It must never fail and
    // never block the writer — that is the entire reason the journal runs in WAL mode.
    //
    // This is not hypothetical: `PRAGMA journal_mode = WAL` takes an exclusive lock and
    // does not honour `busy_timeout`, so issuing it unconditionally on every open made this
    // fail intermittently against a live writer. Storage now reads the mode before setting
    // it, and this test is what holds that fix in place.
    let directory = TempDir::new().expect("temp dir");
    let database = directory.path().join("event.db");

    seed_config(&database, "five-k").await;
    let mut child = Command::new(BINARY)
        .args(["simulate", "--scenario", "five-k", "--speed", "200"])
        .arg("--database")
        .arg(&database)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn splitforge simulate");

    let mut observed = 0_u64;
    let mut successes = 0_u32;
    for attempt in 0..25 {
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        let Ok(journal) = SqliteJournal::open(&database) else {
            // Only tolerated before the writer has created the file at all.
            assert_eq!(
                observed, 0,
                "attempt {attempt}: the journal became unreadable after it had been read \
                 once, while a race was in progress"
            );
            continue;
        };
        let count = journal
            .count()
            .expect("counting a live journal must not fail");
        assert!(
            count >= observed,
            "the visible read count went backwards, from {observed} to {count}"
        );
        observed = count;
        successes += 1;
    }

    child.kill().await.expect("kill the simulation");
    let _ = child.wait().await;

    assert!(successes > 0, "the journal was never readable");
    assert!(
        observed > 0,
        "a race ran for three seconds and a second process saw no reads at all"
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

    let mut store = splitforge_storage::ConfigStore::open(&database).expect("open store");
    splitforge_cli::load_fixture(&mut store, "test", "five-k").expect("load config");
    let config = store.load(fixture.config.race.id).expect("load config");

    let mut journal = SqliteJournal::open(&database).expect("open");
    let first = splitforge_cli::into_journal(
        &config,
        "five-k",
        &mut journal,
        1,
        splitforge_cli::Speed::Immediate,
    )
    .await
    .expect("first run");
    drop(journal);

    let mut journal = SqliteJournal::open(&database).expect("reopen");
    let second = splitforge_cli::into_journal(
        &config,
        "five-k",
        &mut journal,
        2,
        splitforge_cli::Speed::Immediate,
    )
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

/// Writes a fixture's configuration into the database, through the binary.
///
/// Every subprocess test needs configuration in the database before it can simulate or
/// derive anything — which is itself the Milestone 2 change worth noticing: there is no
/// longer a `--fixture` flag that lets a command conjure a roster out of the binary.
async fn seed_config(database: &std::path::Path, slug: &str) {
    run_ok(&["fixture", "load", "--fixture", slug], database).await;
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
