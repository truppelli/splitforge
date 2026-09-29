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
