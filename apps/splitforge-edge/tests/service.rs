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
use splitforge_domain::ClockStep;
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
    /// Configures a 5K, then starts the service against it.
    async fn start() -> Self {
        let directory = tempfile::tempdir().expect("tempdir");
        let database = directory.path().join("event.db");
        let socket = directory.path().join("api.sock");
        configure(&database);

        let child = spawn(&database, &socket);
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

    /// Sends SIGTERM, which is what systemd sends on `stop` and `restart`.
    async fn terminate(mut self) -> std::process::ExitStatus {
        let killed = sigterm(self.child.id().expect("a pid")).await;
        assert!(killed, "kill -TERM failed");

        tokio::time::timeout(Duration::from_secs(10), self.child.wait())
            .await
            .expect("the service did not exit within 10s of SIGTERM")
            .expect("wait")
    }
}

/// Starts the service binary against one database and one socket path.
fn spawn(database: &Path, socket: &Path) -> Child {
    Command::new(SERVICE)
        .args(["--database", database.to_str().expect("utf-8")])
        .args(["--socket", socket.to_str().expect("utf-8")])
        .spawn()
        .expect("start splitforge-edge")
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
