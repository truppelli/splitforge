//! Milestone 6's exit criterion: *an event times identically, and produces byte-identical
//! exports, with integrations enabled and disabled.*
//!
//! ADR-0047 argues this holds by construction: the shipper opens the event database read-only,
//! so it cannot change what the exports are made from. These tests observe it instead of
//! trusting the argument, with the real `splitforge-ship` binary delivering to a loopback
//! RaceDay Connect.
//!
//! Two comparisons, because an event's exports carry identifiers minted at ingest and the time
//! of publication, which differ between any two events however they are timed:
//!
//! - **Across two events**, one timed with the shipper running throughout and one without: the
//!   results CSV, which carries neither, and the read counts.
//! - **Within one event**, before and after the shipper has delivered everything: every export,
//!   results and crossings, CSV and JSON.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use clap::Parser as _;
use splitforge_domain::RawReadJournal as _;
use splitforge_storage::{ConfigStore, SqliteJournal};
use tempfile::TempDir;

const SHIP: &str = env!("CARGO_BIN_EXE_splitforge-ship");

/// `splitforge simulate`'s default seed, so both events receive the same reads.
const SEED: u64 = 0x5F17_F03E;

/// The path of every request the fake RaceDay Connect received, in order.
type Requests = Arc<Mutex<Vec<String>>>;

/// A RaceDay Connect on loopback that pairs as race `r1`, accepts everything, and records the
/// path of each request.
fn raceday() -> (String, Requests) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("address"));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut reader = BufReader::new(stream.try_clone().expect("clone"));
            let mut line = String::new();
            if reader.read_line(&mut line).is_err() {
                continue;
            }
            let path = line
                .split_whitespace()
                .nth(1)
                .unwrap_or_default()
                .to_owned();
            let mut length = 0;
            loop {
                let mut header = String::new();
                reader.read_line(&mut header).expect("header");
                let header = header.trim_end();
                if header.is_empty() {
                    break;
                }
                if let Some(value) = header.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().expect("length");
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).expect("body");
            let reply = if path.ends_with("/pair") {
                r#"{"raceId":"r1","raceName":"Spring 5K","slug":"spring-5k","token":"rdc_t"}"#
            } else {
                r#"{"accepted":1}"#
            };
            log.lock().expect("log").push(path);
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                reply.len()
            );
        }
    });
    (base, seen)
}

/// One event: the five-k fixture, with a writer held open on it as the timer's service holds
/// one. Without a writer, the shipper's read-only open has no `-shm` to use (ADR-0047).
struct Event {
    _directory: TempDir,
    database: PathBuf,
    state: PathBuf,
    _writer: ConfigStore,
}

impl Event {
    fn new() -> Self {
        let directory = TempDir::new().expect("tempdir");
        let database = directory.path().join("event.db");
        let state = directory.path().join("ship.db");
        let mut writer = ConfigStore::open(&database).expect("open the event database");
        splitforge_cli::load_fixture(&mut writer, "test", "five-k").expect("load the fixture");
        Self {
            _directory: directory,
            database,
            state,
            _writer: writer,
        }
    }

    fn ship(&self) -> Command {
        let mut command = Command::new(SHIP);
        command
            .arg("--state")
            .arg(&self.state)
            .arg("--database")
            .arg(&self.database);
        command
    }

    fn ship_ok(&self, args: &[&str]) -> serde_json::Value {
        let output = self
            .ship()
            .args(args)
            .output()
            .expect("run splitforge-ship");
        assert!(
            output.status.success(),
            "splitforge-ship {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).expect("json")
    }

    /// Pairs the shipper with `url` and gives the 5K a course, which is everything it needs
    /// before it sends.
    fn enable(&self, url: &str) {
        let _ = self.ship_ok(&["pair", "--url", url, "--code", "K7QM-4XPD"]);
        let _ = self.ship_ok(&["course", "--race", "5K", "--key", "5k", "--meters", "5000"]);
    }

    /// Times the scripted 5K into the journal, as the timer's read path would.
    fn simulate(&self) -> u64 {
        let store = ConfigStore::open(&self.database).expect("open");
        let race = match store.resolve_race(None).expect("resolve") {
            splitforge_storage::RaceSelection::One(race) => race,
            other => panic!("one race, not {other:?}"),
        };
        let config = store.load(race.id).expect("load");
        let (mut journal, _) =
            SqliteJournal::open_recovering(&self.database, "test").expect("journal");
        let report = runtime()
            .block_on(splitforge_cli::into_journal(
                &config,
                "five-k",
                &mut journal,
                SEED,
                splitforge_cli::Speed::Immediate,
            ))
            .expect("simulate");
        assert_eq!(report.reads_persisted, 638, "the scenario is deterministic");
        journal.count().expect("count")
    }

    /// Runs one `splitforge` command against this event, as an operator would.
    fn splitforge(&self, args: &[&str]) {
        let mut argv = vec![
            "splitforge",
            "--database",
            self.database.to_str().expect("utf-8"),
            "--format",
            "compact",
        ];
        argv.extend_from_slice(args);
        runtime()
            .block_on(splitforge_cli::run(splitforge_cli::Cli::parse_from(argv)))
            .unwrap_or_else(|error| panic!("splitforge {args:?}: {error:#}"));
    }

    fn publish(&self) {
        self.splitforge(&[
            "results",
            "publish",
            "--status",
            "provisional",
            "--reason",
            "the test publishes",
        ]);
    }

    /// Every export an operator can take, by name, as bytes.
    fn exports(&self) -> Vec<(&'static str, Vec<u8>)> {
        [
            ("results.csv", "results", "csv"),
            ("results.json", "results", "json"),
            ("crossings.csv", "crossings", "csv"),
            ("crossings.json", "crossings", "json"),
        ]
        .into_iter()
        .map(|(name, what, shape)| {
            let path = self.database.with_file_name(name);
            self.splitforge(&[
                "export",
                what,
                "--as",
                shape,
                "--output",
                path.to_str().expect("utf-8"),
            ]);
            (name, std::fs::read(&path).expect("read the export"))
        })
        .collect()
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Runtime::new().expect("runtime")
}

/// Waits for a request to `suffix` to have reached RaceDay Connect.
fn delivered(seen: &Mutex<Vec<String>>, suffix: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if seen
            .lock()
            .expect("log")
            .iter()
            .any(|path| path.ends_with(suffix))
        {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

/// Times one event, with or without the shipper running throughout, and returns its read count
/// and its results CSV.
fn time_an_event(with_shipper: bool) -> (u64, Vec<u8>, Option<Requests>) {
    let event = Event::new();
    let (running, seen) = if with_shipper {
        let (url, seen) = raceday();
        event.enable(&url);
        let running = event
            .ship()
            .args(["run", "--interval", "1"])
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("run splitforge-ship");
        (Some(running), Some(seen))
    } else {
        (None, None)
    };

    let reads = event.simulate();
    event.publish();

    if let (Some(mut running), Some(seen)) = (running, seen.as_ref()) {
        let results_arrived = delivered(seen, "/races/r1/results");
        let _ = running.kill();
        let _ = running.wait();
        assert!(results_arrived, "the shipper delivered: {:?}", seen.lock());
    }

    let csv = event
        .exports()
        .into_iter()
        .find(|(name, _)| *name == "results.csv")
        .expect("a results csv")
        .1;
    (reads, csv, seen)
}

#[test]
fn an_event_times_the_same_with_the_shipper_running_as_without_it() {
    let (reads_with, csv_with, seen) = time_an_event(true);
    let (reads_without, csv_without, _) = time_an_event(false);

    let seen = seen.expect("the shipper ran");
    let paths = seen.lock().expect("log");
    for sent in [
        "/races/r1/manifest",
        "/races/r1/crossings",
        "/races/r1/results",
    ] {
        assert!(
            paths.iter().any(|path| path.ends_with(sent)),
            "the integration was really enabled, and sent {sent}: {paths:?}"
        );
    }

    assert_eq!(reads_with, reads_without, "the same reads were journaled");
    assert_eq!(
        String::from_utf8_lossy(&csv_with).lines().count(),
        13,
        "a header and the 5K's twelve entries, so the comparison below compares something"
    );
    assert!(
        csv_with == csv_without,
        "the results differ with the shipper running:\n--- with\n{}\n--- without\n{}",
        String::from_utf8_lossy(&csv_with),
        String::from_utf8_lossy(&csv_without)
    );
}

#[test]
fn delivering_everything_changes_no_export_of_the_event() {
    let event = Event::new();
    let _ = event.simulate();
    event.publish();
    let before = event.exports();
    for (name, bytes) in &before {
        assert!(bytes.len() > 100, "{name} holds the event: {bytes:?}");
    }

    let (url, seen) = raceday();
    event.enable(&url);
    assert_eq!(event.ship_ok(&["run", "--once"])["pass"], "done");
    assert!(delivered(&seen, "/races/r1/results"), "{:?}", seen.lock());
    // A second pass, which finds nothing new to send.
    assert_eq!(event.ship_ok(&["run", "--once"])["pass"], "done");

    let after = event.exports();
    for ((name, before), (_, after)) in before.iter().zip(&after) {
        assert!(
            before == after,
            "{name} changed after the shipper delivered:\n--- before\n{}\n--- after\n{}",
            String::from_utf8_lossy(before),
            String::from_utf8_lossy(after)
        );
    }
}
