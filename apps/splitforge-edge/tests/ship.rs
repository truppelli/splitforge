//! `splitforge-ship`'s pairing, courses and status, run as the binary (ADR-0046, ADR-0047).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use splitforge_storage::ConfigStore;
use tempfile::TempDir;

const SHIP: &str = env!("CARGO_BIN_EXE_splitforge-ship");

struct Shipper {
    _directory: TempDir,
    state: PathBuf,
    database: PathBuf,
}

impl Shipper {
    fn new() -> Self {
        let directory = TempDir::new().expect("tempdir");
        let state = directory.path().join("ship.db");
        let database = directory.path().join("event.db");
        Self {
            _directory: directory,
            state,
            database,
        }
    }

    fn ship(&self, args: &[&str]) -> Output {
        Command::new(SHIP)
            .arg("--state")
            .arg(&self.state)
            .arg("--database")
            .arg(&self.database)
            .args(args)
            .output()
            .expect("run splitforge-ship")
    }

    fn json(&self, args: &[&str]) -> serde_json::Value {
        let output = self.ship(args);
        assert!(
            output.status.success(),
            "splitforge-ship {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).expect("json")
    }

    /// The five-k fixture, and a writer held open on it as the timer's service holds one.
    /// Without a writer SQLite has removed `-shm`, and a read-only open needs it (ADR-0047).
    fn event(&self) -> ConfigStore {
        let mut store = ConfigStore::open(&self.database).expect("open the event database");
        splitforge_cli::load_fixture(&mut store, "test", "five-k").expect("load the fixture");
        store
    }
}

/// A loopback server answering one request with `response`, returning its address and the
/// request body it received.
fn serve(response: String) -> (String, std::sync::mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("address"));
    let (sender, received) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        let mut reader = BufReader::new(stream.try_clone().expect("clone"));
        let mut length = 0;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).expect("line");
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                length = value.trim().parse().expect("length");
            }
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body).expect("body");
        let _ = sender.send(String::from_utf8(body).expect("utf-8"));
        stream.write_all(response.as_bytes()).expect("respond");
    });
    (base, received)
}

fn answer(status: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

const PAIRED: &str =
    r#"{"raceId":"r1","raceName":"Spring 5K","slug":"spring-5k","token":"rdc_secret_token"}"#;

#[test]
fn pairing_keeps_the_token_and_status_never_shows_it() {
    let shipper = Shipper::new();
    let (url, received) = serve(answer("200 OK", PAIRED));

    let paired = shipper.json(&["pair", "--url", &url, "--code", "K7QM-4XPD"]);
    assert_eq!(paired["paired"]["race_name"], "Spring 5K");
    assert_eq!(
        received.recv().expect("a request"),
        r#"{"code":"K7QM-4XPD"}"#
    );

    let status = shipper.json(&["status"]);
    assert_eq!(status["paired"]["race_id"], "r1");
    assert_eq!(status["paired"]["url"], url);
    let printed = status.to_string() + &paired.to_string();
    assert!(!printed.contains("rdc_secret_token"), "{printed}");
    assert_eq!(
        stored_token(&shipper.state).as_deref(),
        Some("rdc_secret_token")
    );
}

#[cfg(unix)]
#[test]
fn the_shippers_database_is_readable_by_its_owner_alone() {
    use std::os::unix::fs::PermissionsExt as _;
    let shipper = Shipper::new();
    let _ = shipper.json(&["status"]);
    let mode = std::fs::metadata(&shipper.state)
        .expect("stat")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600, "it holds the token: {mode:o}");
}

#[test]
fn a_refused_pairing_code_is_an_error_and_keeps_nothing() {
    let shipper = Shipper::new();
    let (url, _received) = serve(answer("404 Not Found", "no such code"));

    let output = shipper.ship(&["pair", "--url", &url, "--code", "WRONG"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("404") && stderr.contains("no such code"),
        "{stderr}"
    );
    assert!(shipper.json(&["status"])["paired"].is_null());
}

#[test]
fn a_token_is_never_sent_unencrypted_across_a_network() {
    let shipper = Shipper::new();
    let output = shipper.ship(&["pair", "--url", "http://raceday.example", "--code", "X"]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("https://"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn a_course_is_checked_against_its_race_before_it_is_kept() {
    let shipper = Shipper::new();
    let _writer = shipper.event();

    for (args, refusal) in [
        (
            vec!["course", "--race", "5K", "--key", "5k", "--meters", "0"],
            "more than 0 metres",
        ),
        (
            vec![
                "course",
                "--race",
                "5K",
                "--key",
                "5k",
                "--meters",
                "5000",
                "--split",
                "halfway=2500",
            ],
            "no split checkpoint named \"halfway\"",
        ),
        (
            vec![
                "course", "--race", "10K", "--key", "10k", "--meters", "10000",
            ],
            "no race is named",
        ),
    ] {
        let output = shipper.ship(&args);
        assert!(!output.status.success(), "{args:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(refusal), "{args:?}: {stderr}");
    }
    assert_eq!(shipper.json(&["status"])["courses"], serde_json::json!([]));

    let kept = shipper.json(&["course", "--race", "5K", "--key", "5k", "--meters", "5000"]);
    assert_eq!(kept["race"], "5K");
    let courses = shipper.json(&["status"])["courses"].clone();
    assert_eq!(courses[0]["key"], "5k");
    assert_eq!(courses[0]["meters"], 5000.0);

    // Recorded again with another distance, it replaces the first.
    let _ = shipper.json(&["course", "--race", "5K", "--key", "5k", "--meters", "5010"]);
    let courses = shipper.json(&["status"])["courses"].clone();
    assert_eq!(courses.as_array().map(Vec::len), Some(1));
    assert_eq!(courses[0]["meters"], 5010.0);
}

fn stored_token(state: &Path) -> Option<String> {
    rusqlite::Connection::open(state)
        .expect("open ship.db")
        .query_row("SELECT token FROM pairing WHERE id = 1", [], |row| {
            row.get(0)
        })
        .ok()
}

// ---- run --------------------------------------------------------------------------------

/// One request the fake RaceDay Connect received.
#[derive(Debug, Clone)]
struct Request {
    method: String,
    path: String,
    body: String,
}

/// A fake RaceDay Connect answering every request with `answer(method, path)`, for as long as
/// the test runs, and recording each one.
fn raceday(
    answer: impl Fn(&str, &str) -> String + Send + 'static,
) -> (String, std::sync::Arc<std::sync::Mutex<Vec<Request>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("address"));
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = std::sync::Arc::clone(&seen);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut reader = BufReader::new(stream.try_clone().expect("clone"));
            let mut line = String::new();
            if reader.read_line(&mut line).is_err() {
                continue;
            }
            let mut parts = line.split_whitespace();
            let method = parts.next().unwrap_or_default().to_owned();
            let path = parts.next().unwrap_or_default().to_owned();
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
            let response = answer(&method, &path);
            log.lock().expect("log").push(Request {
                method,
                path,
                body: String::from_utf8(body).expect("utf-8"),
            });
            let _ = stream.write_all(response.as_bytes());
        }
    });
    (base, seen)
}

/// A RaceDay Connect that pairs as race `r1` and takes everything.
fn accepting(method: &str, path: &str) -> String {
    let _ = method;
    if path.ends_with("/pair") {
        answer("200 OK", PAIRED)
    } else {
        answer("200 OK", r#"{"accepted":1}"#)
    }
}

/// A paired shipper with the five-k course recorded, over an event database holding the
/// simulated 5K. Returns the writer too: it has to stay open (ADR-0047).
fn ready(url: &str) -> (Shipper, splitforge_storage::SqliteJournal) {
    let shipper = Shipper::new();
    let store = shipper.event();
    let journal = simulate(&shipper.database, &store);
    let _ = shipper.json(&["pair", "--url", url, "--code", "K7QM-4XPD"]);
    let _ = shipper.json(&["course", "--race", "5K", "--key", "5k", "--meters", "5000"]);
    (shipper, journal)
}

fn simulate(database: &Path, store: &ConfigStore) -> splitforge_storage::SqliteJournal {
    let race = match store.resolve_race(None).expect("resolve") {
        splitforge_storage::RaceSelection::One(race) => race,
        other => panic!("one race, not {other:?}"),
    };
    let config = store.load(race.id).expect("load");
    let (mut journal, _) =
        splitforge_storage::SqliteJournal::open_recovering(database, "test").expect("journal");
    tokio::runtime::Runtime::new()
        .expect("runtime")
        .block_on(splitforge_cli::into_journal(
            &config,
            "five-k",
            &mut journal,
            0x5F17_F03E,
            splitforge_cli::Speed::Immediate,
        ))
        .expect("simulate");
    journal
}

fn publish_results(database: &Path) {
    use clap::Parser as _;
    let cli = splitforge_cli::Cli::parse_from([
        "splitforge",
        "--database",
        database.to_str().expect("utf-8"),
        "--format",
        "compact",
        "results",
        "publish",
        "--reason",
        "the test publishes",
    ]);
    tokio::runtime::Runtime::new()
        .expect("runtime")
        .block_on(splitforge_cli::run(cli))
        .expect("publish");
}

fn sent_to(requests: &[Request], suffix: &str) -> Vec<Request> {
    requests
        .iter()
        .filter(|request| request.path.ends_with(suffix))
        .cloned()
        .collect()
}

#[test]
fn a_pass_sends_the_course_and_every_runner_and_the_next_sends_nothing_new() {
    let (url, seen) = raceday(accepting);
    let (shipper, _writer) = ready(&url);

    assert_eq!(shipper.json(&["run", "--once"])["pass"], "done");
    let first = seen.lock().expect("log").clone();
    let manifests = sent_to(&first, "/races/r1/manifest");
    assert_eq!(manifests.len(), 1);
    assert_eq!(manifests[0].method, "PUT");
    let crossings = sent_to(&first, "/races/r1/crossings");
    assert!(!crossings.is_empty(), "{first:?}");
    let body: serde_json::Value = serde_json::from_str(&crossings[0].body).expect("json");
    assert!(
        !body["replaces"].as_array().expect("replaces").is_empty(),
        "{body}"
    );
    assert!(
        sent_to(&first, "/results").is_empty(),
        "nothing is published yet"
    );

    let status = shipper.json(&["status"]);
    assert!(
        status["runners_sent"].as_i64().expect("a count") > 0,
        "{status}"
    );
    assert!(status["delivery"]["last_error"].is_null(), "{status}");
    assert!(status["delivery"]["last_delivered"].is_string(), "{status}");

    // Nothing changed, so nothing is sent: what arrived is recorded in ship.db.
    assert_eq!(shipper.json(&["run", "--once"])["pass"], "done");
    assert_eq!(seen.lock().expect("log").len(), first.len());
}

#[test]
fn a_published_revision_is_sent_once() {
    let (url, seen) = raceday(accepting);
    let (shipper, _writer) = ready(&url);
    assert_eq!(shipper.json(&["run", "--once"])["pass"], "done");

    publish_results(&shipper.database);
    assert_eq!(shipper.json(&["run", "--once"])["pass"], "done");
    let results = sent_to(&seen.lock().expect("log"), "/races/r1/results");
    assert_eq!(results.len(), 1);
    assert_eq!(shipper.json(&["status"])["revisions_delivered"], 1);

    assert_eq!(shipper.json(&["run", "--once"])["pass"], "done");
    assert_eq!(
        sent_to(&seen.lock().expect("log"), "/races/r1/results").len(),
        1
    );
}

#[test]
fn a_busy_raceday_connect_is_retried_and_nothing_is_recorded_as_sent() {
    let (url, _seen) = raceday(|method, path| {
        if path.ends_with("/crossings") {
            answer("503 Service Unavailable", "busy")
        } else {
            accepting(method, path)
        }
    });
    let (shipper, _writer) = ready(&url);

    assert_eq!(shipper.json(&["run", "--once"])["pass"], "retry");
    let status = shipper.json(&["status"]);
    assert_eq!(status["runners_sent"], 0, "{status}");
    let error = status["delivery"]["last_error"].as_str().expect("an error");
    assert!(
        error.contains("will retry") && error.contains("503"),
        "{error}"
    );
}

#[test]
fn an_unpaired_box_stops_until_it_is_paired_again() {
    let (url, seen) = raceday(|method, path| {
        if path.ends_with("/manifest") {
            answer("401 Unauthorized", "")
        } else {
            accepting(method, path)
        }
    });
    let (shipper, _writer) = ready(&url);

    assert_eq!(shipper.json(&["run", "--once"])["pass"], "stopped");
    assert_eq!(shipper.json(&["status"])["delivery"]["unpaired"], true);
    let sent = seen.lock().expect("log").len();
    assert_eq!(shipper.json(&["run", "--once"])["pass"], "stopped");
    assert_eq!(
        seen.lock().expect("log").len(),
        sent,
        "nothing is sent while unpaired"
    );

    let _ = shipper.json(&["pair", "--url", &url, "--code", "K7QM-4XPD"]);
    assert_eq!(shipper.json(&["status"])["delivery"]["unpaired"], false);
}

#[test]
fn a_refused_course_is_not_resent_until_it_changes() {
    let (url, seen) = raceday(|method, path| {
        if path.ends_with("/manifest") {
            answer("422 Unprocessable Entity", "no such distance")
        } else {
            accepting(method, path)
        }
    });
    let (shipper, _writer) = ready(&url);

    assert_eq!(shipper.json(&["run", "--once"])["pass"], "stopped");
    let status = shipper.json(&["status"]);
    let error = status["delivery"]["last_error"].as_str().expect("an error");
    assert!(error.contains("the course was refused"), "{error}");
    assert!(sent_to(&seen.lock().expect("log"), "/crossings").is_empty());

    assert_eq!(shipper.json(&["run", "--once"])["pass"], "stopped");
    assert_eq!(
        sent_to(&seen.lock().expect("log"), "/manifest").len(),
        1,
        "not resent"
    );

    let _ = shipper.json(&["course", "--race", "5K", "--key", "5k", "--meters", "5010"]);
    assert_eq!(shipper.json(&["run", "--once"])["pass"], "stopped");
    assert_eq!(
        sent_to(&seen.lock().expect("log"), "/manifest").len(),
        2,
        "a new course is sent"
    );
}

#[test]
fn a_shipper_waits_for_an_event_database_it_cannot_open() {
    let (url, _seen) = raceday(accepting);
    let shipper = Shipper::new();
    let _ = shipper.json(&["pair", "--url", &url, "--code", "K7QM-4XPD"]);

    assert_eq!(shipper.json(&["run", "--once"])["pass"], "waiting");
    let status = shipper.json(&["status"]);
    let error = status["delivery"]["last_error"].as_str().expect("an error");
    assert!(error.contains("waiting for the event database"), "{error}");
}

#[test]
fn the_running_shipper_retries_after_a_busy_answer_and_notices_new_results() {
    // The loop itself, not one pass: a 503 with Retry-After, then a revision published while it
    // runs, which it has to notice through data_version.
    let busy_once = std::sync::atomic::AtomicBool::new(true);
    let (url, seen) = raceday(move |method, path| {
        if path.ends_with("/crossings")
            && busy_once.swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            "HTTP/1.1 503 Service Unavailable\r\nRetry-After: 1\r\nContent-Length: 4\r\n\
             Connection: close\r\n\r\nbusy"
                .to_owned()
        } else {
            accepting(method, path)
        }
    });
    let (shipper, _writer) = ready(&url);
    let mut running = Command::new(SHIP)
        .arg("--state")
        .arg(&shipper.state)
        .arg("--database")
        .arg(&shipper.database)
        .args(["run", "--interval", "1"])
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("run splitforge-ship");

    let arrived = |suffix: &str, wanted: usize| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while std::time::Instant::now() < deadline {
            let count = seen
                .lock()
                .expect("log")
                .iter()
                .filter(|request| request.path.ends_with(suffix) && request.method == "POST")
                .count();
            if count >= wanted {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        false
    };
    let crossings_delivered = arrived("/crossings", 2);
    publish_results(&shipper.database);
    let results_delivered = arrived("/results", 1);
    let _ = running.kill();
    let _ = running.wait();

    assert!(
        crossings_delivered,
        "the refused batch was retried: {:?}",
        seen.lock().expect("log")
    );
    assert!(results_delivered, "the new revision was noticed and sent");
}
