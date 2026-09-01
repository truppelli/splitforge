//! The service as systemd runs it: a real binary, a real database, a real signal.
//!
//! The thing under test is the **executable**, so every claim below is made against a
//! spawned `splitforge-edge` process talking over a real socket, never against an in-process
//! call. A claim tested through a library call is a claim about a path the deployment does
//! not take.
//!
//! The fixture *setup* is library calls, and deliberately so. It used to shell out to the
//! `splitforge` binary, which reached across a crate boundary by path — and that binary is
//! only on disk when Cargo happens to be building the whole workspace, so
//! `cargo test -p splitforge-edge` failed on a missing file rather than on a defect. Setup
//! is not the claim; the service is.
//!
//! Unix only. The service serves a Unix socket (ADR-0021) and refuses to run elsewhere.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use splitforge_cli::Speed;
use splitforge_domain::{ClockStep, RawReadJournal as _};
use splitforge_storage::{ConfigStore, SqliteJournal};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::UnixStream;
use tokio::process::{Child, Command};

/// The service binary, built by Cargo for this test run.
const SERVICE: &str = env!("CARGO_BIN_EXE_splitforge-edge");

/// The scenario every test here runs.
const FIXTURE: &str = "five-k";

/// `splitforge simulate`'s default seed, so the read counts below are the ones an operator
/// would see running the same scenario by hand.
const SEED: u64 = 0x5F17_F03E;

/// Reads the 5K fixture into a database the service can open.
fn configure(database: &Path) {
    let mut store = ConfigStore::open(database).expect("open the configuration");
    splitforge_cli::load_fixture(&mut store, "test", FIXTURE).expect("load the fixture");
}

/// Runs the 5K crossing plan into the journal, from this process rather than the service's.
async fn simulate(database: &Path) {
    let store = ConfigStore::open(database).expect("open the configuration");
    let race = match store.resolve_race(None).expect("resolve the race") {
        splitforge_storage::RaceSelection::One(race) => race,
        other => panic!("expected exactly one race, got {other:?}"),
    };
    let config = store.load(race.id).expect("load the race configuration");

    let (mut journal, _recovery) =
        SqliteJournal::open_recovering(database).expect("open the journal");
    let report =
        splitforge_cli::into_journal(&config, FIXTURE, &mut journal, SEED, Speed::Immediate)
            .await
            .expect("simulate");
    assert_eq!(report.reads_persisted, 638, "the scenario is deterministic");
}

struct Service {
    _directory: tempfile::TempDir,
    socket: PathBuf,
    child: Child,
}

impl Service {
    /// Configures a 5K, then starts the service against it with no reader.
    async fn start() -> Self {
        Self::start_with(None).await
    }

    /// Starts the service with the 5K running through its own read path.
    async fn start_simulating() -> Self {
        Self::start_with(Some(FIXTURE)).await
    }

    async fn start_with(scenario: Option<&str>) -> Self {
        let directory = tempfile::tempdir().expect("tempdir");
        let database = directory.path().join("event.db");
        let socket = directory.path().join("api.sock");
        configure(&database);

        let child = spawn_with(&database, &socket, scenario);
        wait_for(&socket).await;

        Self {
            _directory: directory,
            socket,
            child,
        }
    }

    fn database(&self) -> PathBuf {
        self.socket.with_file_name("event.db")
    }

    async fn get(&self, target: &str) -> (String, serde_json::Value) {
        let response = request(&self.socket, target).await;

        let (head, body) = response
            .split_once("\r\n\r\n")
            .unwrap_or_else(|| panic!("no body in: {response}"));
        let status = head.lines().next().expect("status line").to_owned();
        let json = serde_json::from_str(body).unwrap_or_else(|error| {
            panic!("body was not JSON ({error}): {body}");
        });
        (status, json)
    }

    /// Polls `/health` until `ready` accepts the body, or gives up after ten seconds.
    ///
    /// Background tasks land after the socket does — the service binds and starts answering
    /// while its first time-source sample is still running in a subprocess. Polling is the
    /// difference between asserting that something happened and racing it.
    async fn poll_until(
        &self,
        ready: impl Fn(&serde_json::Value) -> bool,
    ) -> Option<serde_json::Value> {
        for _ in 0..200 {
            let (_status, health) = self.get("/health").await;
            if ready(&health) {
                return Some(health);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        None
    }

    /// Sends SIGTERM, which is what systemd sends on `stop` and `restart`.
    ///
    /// Takes the whole handle, so the temporary directory goes with it — which is what a
    /// test that is finished with the service wants, and what a test that still needs to
    /// look at the files must not do. That one calls [`Self::stop`].
    async fn terminate(self) -> std::process::ExitStatus {
        self.stop().await.0
    }

    /// Stops the service and hands back its directory, still alive.
    ///
    /// A `TempDir` deletes its contents when it drops, so inspecting a database *after* the
    /// service has exited means keeping the handle for as long as the files are wanted.
    async fn stop(self) -> (std::process::ExitStatus, tempfile::TempDir) {
        let Self {
            _directory,
            socket: _,
            mut child,
        } = self;

        let killed = sigterm(child.id().expect("a pid")).await;
        assert!(killed, "kill -TERM failed");

        let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
            .await
            .expect("the service did not exit within 10s of SIGTERM")
            .expect("wait");

        (status, _directory)
    }
}

/// Starts the service binary against one database and one socket path.
fn spawn(database: &Path, socket: &Path) -> Child {
    spawn_with(database, socket, None)
}

/// Starts the service, optionally running a scenario through its own read path.
///
/// The seed is passed explicitly rather than left to the binary's default, so that the read
/// counts asserted below are the scenario's and not a default's — if the default ever moves,
/// these tests should keep passing and `splitforge simulate` should be the thing that
/// changes.
fn spawn_with(database: &Path, socket: &Path, scenario: Option<&str>) -> Child {
    let mut command = Command::new(SERVICE);
    command
        .args(["--database", database.to_str().expect("utf-8")])
        .args(["--socket", socket.to_str().expect("utf-8")]);
    if let Some(scenario) = scenario {
        command
            .args(["--simulate", scenario])
            .args(["--simulate-seed", &SEED.to_string()]);
    }
    command.spawn().expect("start splitforge-edge")
}

/// Waits for the service to bind, which is also the check that it started at all.
async fn wait_for(socket: &Path) {
    for _ in 0..400 {
        if socket.exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!(
        "the service never created its socket at {}",
        socket.display()
    );
}

/// One hand-written HTTP/1.1 request over the socket, and the whole response.
async fn request(socket: &Path, target: &str) -> String {
    let mut stream = UnixStream::connect(socket).await.expect("connect");
    stream
        .write_all(
            format!("GET {target} HTTP/1.1\r\nHost: splitforge\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await
        .expect("write");

    let mut response = String::new();
    stream.read_to_string(&mut response).await.expect("read");
    response
}

/// Sends SIGTERM to one process, via the shell's builtin.
///
/// Shelled out rather than reached through a crate: `Child::kill` sends SIGKILL, which is
/// precisely the signal that would *not* exercise the graceful path, and pulling in a libc
/// wrapper to send one signal in one test is not worth a dependency in a workspace that
/// denies `unsafe_code`.
///
/// Through `sh -c` rather than `kill` directly, because `kill` is a shell builtin and the
/// slim images CI builds in do not all ship the `procps` binary of that name.
async fn sigterm(pid: u32) -> bool {
    Command::new("sh")
        .args(["-c", &format!("kill -TERM {pid}")])
        .status()
        .await
        .expect("run sh")
        .success()
}

#[tokio::test]
async fn the_service_reports_the_database_it_was_pointed_at() {
    let service = Service::start().await;
    let (status, health) = service.get("/health").await;

    assert!(status.contains("200 OK"), "got: {status}");
    assert_eq!(health["status"], "ok");
    assert_eq!(health["database"], "event.db", "the name, never the path");
    assert_eq!(health["schema_version"], splitforge_storage::SCHEMA_VERSION);
    assert_eq!(
        health["raw_reads"], 0,
        "the fixture configures, it does not run"
    );
    assert_eq!(health["min_free_mb"], 256, "the documented default floor");
    assert_eq!(health["above_floor"], true);
    assert!(health["free_mb"].as_u64().is_some(), "got: {health}");

    service.terminate().await;
}

#[tokio::test]
async fn the_service_measures_its_time_source_rather_than_assuming_one() {
    // Every read this service writes carries a `DeviceClockState`, and that is permanent
    // evidence. This asserts the measurement happened at all — the four tokens are the whole
    // vocabulary, and which one arrives depends on what the machine running the test has
    // installed, so the token is checked for membership rather than for a value.
    let service = Service::start().await;

    let health = service
        .poll_until(|health| !health["clock_source"].is_null())
        .await
        .expect("the service never sampled its time source");

    let source = &health["clock_source"];
    let measurement = source["measurement"].as_str().expect("a token");
    assert!(
        matches!(
            measurement,
            "measured" | "no_daemon_tool" | "daemon_unreachable" | "unreadable"
        ),
        "unknown measurement token {measurement:?}: {health}"
    );

    // The distinction the field exists for. "Nobody could ask" must never arrive looking
    // like "we asked, and the clock is bad" — only a real measurement carries a state.
    if measurement == "measured" {
        assert!(source["state"].is_string(), "got: {health}");
    } else {
        assert!(
            source["state"].is_null(),
            "an unmeasured clock has no state to report: {health}"
        );
    }

    service.terminate().await;
}

#[tokio::test]
async fn a_service_with_no_reader_reports_one_that_cannot_be_read_as_working() {
    // The distinction the whole `ReaderHealth` shape exists for, asserted against the real
    // binary rather than a constructed struct: a device waiting for hardware must not look
    // like a device whose reader is fine.
    let service = Service::start().await;
    let (status, health) = service.get("/health").await;

    assert!(status.contains("200 OK"), "got: {status}");
    assert_eq!(health["status"], "ok", "no reader is not a fault: {health}");
    assert_eq!(health["reader"]["kind"], "none");
    assert!(
        health["reader"]["state"].is_null(),
        "nothing whose state could be described: {health}"
    );
    assert_eq!(health["reader"]["reads_received"], 0);
    assert_eq!(health["reader"]["reads_persisted"], 0);

    service.terminate().await;
}

#[tokio::test]
async fn the_service_makes_the_reads_it_takes_off_its_reader_durable() {
    // The milestone. Every read below was taken off a `ReaderProvider` by the service, in
    // the service's own process, and written by the service — not simulated into the file by
    // a test helper first, which is what every other read in this file has been.
    let service = Service::start_simulating().await;

    let health = service
        .poll_until(|health| health["reader"]["reads_persisted"] == 638)
        .await
        .expect("the service never persisted the whole scenario");

    assert_eq!(
        health["reader"]["kind"], "simulated",
        "reported, not hidden"
    );
    assert_eq!(health["reader"]["reads_received"], 638);
    assert_eq!(health["reader"]["reads_persisted"], 638);

    // The independent check, and the one that would catch a counter that moved without a
    // write behind it: `raw_reads` is `SELECT COUNT(*)` against the database on every
    // request, so it can only agree if the rows are actually there.
    assert_eq!(
        health["raw_reads"], 638,
        "the counter and the journal must agree: {health}"
    );
    assert_eq!(health["status"], "ok", "got: {health}");

    // The reader ran out of script, which is the ordinary end of a run and carries no fault.
    let health = service
        .poll_until(|health| health["reader"]["state"] == "stopped")
        .await
        .expect("the reader never reported stopping");
    assert_eq!(health["status"], "ok", "a finished script is not a fault");

    service.terminate().await;
}

#[tokio::test]
async fn every_read_the_service_wrote_is_in_the_journal_after_it_exits() {
    // Clause 2 of M3a's exit criterion, as far as a simulator can carry it: the journal does
    // not disagree with what arrived. Checked from a different process, against the file, on
    // a database the service has closed.
    let service = Service::start_simulating().await;
    service
        .poll_until(|health| health["reader"]["reads_persisted"] == 638)
        .await
        .expect("the service never persisted the whole scenario");

    let database = service.database();
    // `stop` rather than `terminate`: the directory has to outlive the service, because the
    // whole claim below is about what it left on disk.
    let (_status, _directory) = service.stop().await;

    let journal = SqliteJournal::open(&database).expect("reopen the journal");
    let reads = journal.read_all().expect("read the journal");
    assert_eq!(reads.len(), 638);

    // Contiguous from 1. A gap here would mean a row was written and lost, or that two
    // writers raced; neither may happen on a path that appends under one lock.
    for (index, read) in reads.iter().enumerate() {
        let expected = u64::try_from(index).expect("fits") + 1;
        assert_eq!(read.seq, expected, "journal sequence is not contiguous");
    }

    // Every read carries the state the device measured, not one the read path invented.
    // Whatever chrony said on this machine, it must be the same answer for every row.
    let states: std::collections::BTreeSet<_> = reads
        .iter()
        .map(|read| read.read.device_clock_state)
        .collect();
    assert_eq!(states.len(), 1, "one run, one clock state: {states:?}");

    // Both directions, which is the reconciliation `doctor` performs: the sidecar holding a
    // read the database lost is the recoverable case, and the database holding one the
    // sidecar never saw would mean the write-ahead ordering was not honored.
    let sidecar = journal.survey().expect("survey the sidecar");
    assert_eq!(sidecar.missing_from_database, 0, "got: {sidecar:?}");
    assert_eq!(sidecar.missing_from_sidecar, 0, "got: {sidecar:?}");
    assert_eq!(sidecar.corrupt_lines, 0, "got: {sidecar:?}");
}

#[tokio::test]
async fn health_counts_the_reads_the_journal_actually_holds() {
    // The number has to come from the database rather than from a counter the service keeps,
    // because the service did not write these reads — another process did, on its own
    // connection, while the service was already running and holding the file open.
    let service = Service::start().await;

    let (_status, before) = service.get("/health").await;
    assert_eq!(before["raw_reads"], 0);

    simulate(&service.database()).await;

    let (status, after) = service.get("/health").await;
    assert!(status.contains("200 OK"), "got: {status}");
    assert_eq!(
        after["raw_reads"], 638,
        "health must read the journal, not a cached count: {after}"
    );

    service.terminate().await;
}

#[tokio::test]
async fn a_floor_it_cannot_meet_makes_the_service_report_degraded() {
    let service = Service::start().await;

    let mut store = ConfigStore::open(service.database()).expect("open the configuration");
    store
        .set_min_free_bytes(999_999_999 * 1024 * 1024)
        .expect("raise the floor");
    drop(store);

    let (status, health) = service.get("/health").await;

    assert!(
        status.contains("503"),
        "a monitor keys off the status code: {status}"
    );
    assert_eq!(health["status"], "degraded");
    assert_eq!(health["above_floor"], false);
    assert!(
        health["degraded_by"][0]
            .as_str()
            .expect("a reason")
            .contains("below the"),
        "the reason has to name the problem: {health}"
    );

    service.terminate().await;
}

#[tokio::test]
async fn a_clock_that_jumped_makes_the_service_report_degraded() {
    // The service writes these itself, from its own monitor loop. What this covers is the
    // other direction: that a step already in the database reaches health, which is what a
    // monitor polling the socket after a restart actually sees. The loop's own arithmetic
    // is `splitforge-domain`'s tests, and the loop end to end is verified against a
    // deliberately stepped clock in docs/clock-and-time-discipline.md.
    let service = Service::start().await;

    let (status, before) = service.get("/health").await;
    assert!(status.contains("200 OK"), "got: {status}");
    assert_eq!(before["clock_steps"], 0);

    let mut journal = SqliteJournal::open(service.database()).expect("open journal");
    let at = time::OffsetDateTime::now_utc();
    journal
        .record_clock_step(&ClockStep {
            observed_before: at,
            observed_after: at + time::Duration::seconds(-30),
            monotonic_ms: 10_000,
            step_ms: -40_000,
        })
        .expect("record a step");
    drop(journal);

    let (status, health) = service.get("/health").await;

    assert!(
        status.contains("503"),
        "a clock that moved is something a monitor must be able to see: {status}"
    );
    assert_eq!(health["status"], "degraded");
    assert_eq!(health["clock_steps"], 1);

    let reason = health["degraded_by"][0].as_str().expect("a reason");
    assert!(reason.contains("jumped 1 time(s)"), "got: {reason}");
    assert!(
        reason.contains("-40000 ms"),
        "the sign is the dangerous part: {reason}"
    );

    service.terminate().await;
}

#[tokio::test]
async fn sigterm_stops_the_service_cleanly_and_removes_the_socket() {
    // systemd sends SIGTERM on both `stop` and `restart`. A service that ignored it would
    // be killed after the timeout and leave its socket behind for the next start to clear.
    let service = Service::start().await;
    let socket = service.socket.clone();
    assert!(socket.exists());

    let status = service.terminate().await;

    assert!(status.success(), "a SIGTERM shutdown is not a failure");
    assert!(
        !socket.exists(),
        "a clean stop should take the socket with it"
    );
}

#[tokio::test]
async fn the_service_replays_a_sidecar_the_database_is_missing() {
    // The service is the writer's entry point, so it reconciles against the write-ahead
    // sidecar on start (ADR-0018). A service coming up after a crash is exactly when that
    // matters, and it is the one thing this binary does that the health endpoint does not.
    let directory = tempfile::tempdir().expect("tempdir");
    let database = directory.path().join("event.db");
    let socket = directory.path().join("api.sock");

    configure(&database);
    simulate(&database).await;

    // The disaster: the database goes, the sidecar stays.
    for suffix in ["", "-wal", "-shm"] {
        let victim = database.with_file_name(format!("event.db{suffix}"));
        if victim.exists() {
            std::fs::remove_file(&victim).expect("delete");
        }
    }
    assert!(database.with_file_name("event.db.reads.jsonl").exists());

    let mut child = spawn(&database, &socket);
    wait_for(&socket).await;

    let response = request(&socket, "/health").await;
    let body = response.split_once("\r\n\r\n").expect("a body").1;
    let health: serde_json::Value = serde_json::from_str(body).expect("JSON");

    assert_eq!(
        health["raw_reads"], 638,
        "starting the service must replay the sidecar into a database that lost them: {health}"
    );

    assert!(sigterm(child.id().expect("a pid")).await);
    let _ = tokio::time::timeout(Duration::from_secs(10), child.wait()).await;
}
