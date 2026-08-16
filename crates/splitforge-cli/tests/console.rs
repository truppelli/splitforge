//! The Milestone 2 exit criterion, as an executable claim.
//!
//! > A simulated race can be configured and operated end to end without ever touching the
//! > database directly.
//!
//! Everything here goes through the **binary**. No test reaches for `ConfigStore`, no test
//! loads a fixture, and no test opens SQLite. If any of them did, the milestone's claim
//! would be tested against a path an operator does not have.
//!
//! The roster and chip assignments come from CSV files written in the test, because that is
//! what an operator actually has: a spreadsheet exported from a registration system.

use std::path::Path;

use serde_json::Value;
use tempfile::TempDir;
use tokio::process::Command;

/// The `splitforge` binary, built by Cargo for this test run.
const BINARY: &str = env!("CARGO_BIN_EXE_splitforge");

/// The fixture chips, so a hand-built roster lines up with the `five-k` crossing plan.
///
/// The scenario emits chip `E2801160600002` + a zero-padded bib. Matching that here is what
/// lets a race configured entirely through the CLI be driven by the simulator.
fn chip_for(bib: u32) -> String {
    format!("E2801160600002{bib:010}")
}

struct Console {
    _directory: TempDir,
    database: String,
    directory: std::path::PathBuf,
}

impl Console {
    fn new() -> Self {
        let directory = TempDir::new().expect("temp dir");
        let path = directory.path().to_path_buf();
        Self {
            database: path.join("event.db").display().to_string(),
            directory: path,
            _directory: directory,
        }
    }

    /// Runs the binary, failing loudly with stderr if it did not succeed.
    async fn run(&self, args: &[&str]) -> String {
        let output = Command::new(BINARY)
            .args(["--database", &self.database, "--format", "compact"])
            .args(args)
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

    /// Runs the binary expecting failure, returning stderr.
    async fn expect_failure(&self, args: &[&str]) -> String {
        let output = Command::new(BINARY)
            .args(["--database", &self.database, "--format", "compact"])
            .args(args)
            .output()
            .await
            .expect("run splitforge");
        assert!(
            !output.status.success(),
            "`splitforge {}` was expected to fail but succeeded: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout)
        );
        String::from_utf8(output.stderr).expect("stderr is UTF-8")
    }

    async fn json(&self, args: &[&str]) -> Value {
        serde_json::from_str(&self.run(args).await).expect("command emits JSON")
    }

    fn write(&self, name: &str, contents: &str) -> String {
        let path = self.directory.join(name);
        std::fs::write(&path, contents).expect("write file");
        path.display().to_string()
    }

    fn path(&self, name: &str) -> String {
        self.directory.join(name).display().to_string()
    }

    /// Configures a complete 5K through the CLI alone.
    async fn configure_a_five_k(&self) {
        self.run(&["init"]).await;
        self.run(&["event", "create", "--name", "Spring Series"])
            .await;
        self.run(&[
            "race",
            "create",
            "--name",
            "5K",
            "--start",
            "2026-04-11T08:00:00Z",
        ])
        .await;
        self.run(&["checkpoint", "add", "--name", "start", "--kind", "start"])
            .await;
        self.run(&["checkpoint", "add", "--name", "finish", "--kind", "finish"])
            .await;

        let mut roster = String::from("bib,name\n");
        let mut chips = String::from("bib,chip\n");
        for bib in 101_u32..=112 {
            roster.push_str(&format!("{bib},Runner {bib}\n"));
            chips.push_str(&format!("{bib},{}\n", chip_for(bib)));
        }
        let roster = self.write("participants.csv", &roster);
        let chips = self.write("assignments.csv", &chips);

        self.run(&["roster", "import", &roster]).await;
        self.run(&["chips", "import", &chips]).await;

        self.run(&["reader", "add", "--id", "mat", "--label", "Start/finish"])
            .await;
        self.run(&[
            "reader",
            "map",
            "--reader",
            "mat",
            "--antenna",
            "1",
            "--checkpoint",
            "start",
        ])
        .await;
        self.run(&[
            "reader",
            "map",
            "--reader",
            "mat",
            "--antenna",
            "2",
            "--checkpoint",
            "finish",
        ])
        .await;
        self.run(&[
            "policy",
            "set",
            "--checkpoint",
            "finish",
            "--selection-rule",
            "first-above-rssi:-62",
        ])
        .await;
    }
}

#[tokio::test]
async fn a_race_configured_entirely_through_the_cli_times_a_simulated_event() {
    // This is the milestone, start to finish.
    let console = Console::new();
    console.configure_a_five_k().await;

    // Nothing is wrong with the configuration before a single read arrives.
    let doctor = console.json(&["doctor"]).await;
    assert_eq!(doctor["errors"], 0, "doctor: {doctor}");
    assert_eq!(doctor["warnings"], 0, "doctor: {doctor}");

    console
        .run(&[
            "--actor",
            "phillip",
            "race",
            "start",
            "--at",
            "2026-04-11T08:00:00Z",
        ])
        .await;

    let simulated = console.json(&["simulate", "--scenario", "five-k"]).await;
    assert_eq!(simulated["reads_received"], simulated["reads_scripted"]);
    assert_eq!(simulated["reads_persisted"], simulated["reads_received"]);
    assert_eq!(simulated["race"], "5K");

    console.run(&["--actor", "phillip", "race", "stop"]).await;

    // The whole point: derivation now reads its roster, checkpoints, and policy from the
    // database that was built by the commands above. There is no fixture in this path.
    let derived = console.json(&["derive"]).await;
    assert_eq!(derived["race"], "5K");
    assert_eq!(
        derived["accepted"], 24,
        "24 crossings: 12 starts, 12 finishes"
    );
    assert_eq!(
        derived["timing_events"], 23,
        "every crossing but the unrostered one is credited"
    );
    assert_eq!(derived["unassigned_crossings"], 1);
    assert_eq!(derived["raw_reads"], simulated["reads_persisted"]);

    let status = console.json(&["status"]).await;
    assert_eq!(status["participants"], 12);
    assert_eq!(status["checkpoints"], 2);
    assert_eq!(status["chip_assignments"], 12);
    assert_eq!(status["antenna_mappings"], 2);
    assert_eq!(status["gun_time"], "2026-04-11T08:00:00Z");
    assert_eq!(status["running"], false, "the race was stopped");
}

#[tokio::test]
async fn the_audit_trail_explains_how_the_event_was_configured() {
    // docs/timing-model.md § 8: given a finished event, the database alone has to explain
    // what changed. Every configuration command must therefore leave a trace.
    let console = Console::new();
    console.configure_a_five_k().await;
    console.run(&["--actor", "phillip", "race", "start"]).await;

    let trail = console.json(&["audit", "--limit", "100"]).await;
    let actions: Vec<&str> = trail
        .as_array()
        .expect("an array")
        .iter()
        .filter_map(|entry| entry["action"].as_str())
        .collect();

    for expected in [
        "event.create",
        "race.create",
        "checkpoint.add",
        "roster.import",
        "chips.import",
        "reader.add",
        "reader.map",
        "policy.set",
        "race.start",
    ] {
        assert!(
            actions.contains(&expected),
            "{expected} left no audit trail; actions were {actions:?}"
        );
    }

    // The actor is recorded, not assumed.
    let start = trail
        .as_array()
        .expect("an array")
        .iter()
        .find(|entry| entry["action"] == "race.start")
        .expect("a recorded start");
    assert_eq!(start["actor"], "phillip");
    assert_eq!(start["subject"], "5K");
}

#[tokio::test]
async fn a_recorded_gun_time_never_gates_ingestion() {
    // The evidence principle, as a test. A read is journalled because a reader sent it and
    // the disk accepted it — an operator who forgets `race start` must not lose evidence.
    let console = Console::new();
    console.configure_a_five_k().await;

    let simulated = console.json(&["simulate", "--scenario", "five-k"]).await;
    assert!(
        simulated["reads_persisted"].as_u64().expect("a count") > 0,
        "reads were dropped because the race had not been started"
    );

    // And a start recorded *after* the reads still produces a usable gun time.
    console
        .run(&["race", "start", "--at", "2026-04-11T08:00:00Z"])
        .await;
    assert_eq!(
        console.json(&["status"]).await["gun_time"],
        "2026-04-11T08:00:00Z"
    );
}

#[tokio::test]
async fn a_restarted_race_supersedes_the_first_gun_without_erasing_it() {
    let console = Console::new();
    console.configure_a_five_k().await;

    console
        .run(&["race", "start", "--at", "2026-04-11T08:00:00Z"])
        .await;
    console
        .run(&[
            "race",
            "start",
            "--at",
            "2026-04-11T08:12:00Z",
            "--note",
            "false start, field recalled",
        ])
        .await;

    assert_eq!(
        console.json(&["status"]).await["gun_time"],
        "2026-04-11T08:12:00Z",
        "the most recent start is the one that counts"
    );

    let trail = console.json(&["audit", "--limit", "100"]).await;
    let starts = trail
        .as_array()
        .expect("an array")
        .iter()
        .filter(|entry| entry["action"] == "race.start")
        .count();
    assert_eq!(starts, 2, "the superseded gun time must still be on record");
}

#[tokio::test]
async fn re_importing_a_corrected_roster_does_not_invalidate_the_chips() {
    let console = Console::new();
    console.configure_a_five_k().await;
    console.run(&["simulate", "--scenario", "five-k"]).await;

    let before = console.json(&["derive"]).await;

    // A name was misspelled at registration. Re-import with the correction.
    let mut roster = String::from("bib,name\n");
    for bib in 101_u32..=112 {
        if bib == 104 {
            roster.push_str("104,Ada Lovelace\n");
        } else {
            roster.push_str(&format!("{bib},Runner {bib}\n"));
        }
    }
    let path = console.write("corrected.csv", &roster);
    let summary = console.json(&["roster", "import", &path]).await;

    assert_eq!(summary["updated"], 1);
    assert_eq!(summary["inserted"], 0);
    assert_eq!(summary["unchanged"], 11);

    let after = console.json(&["derive"]).await;
    assert_eq!(
        after["timing_events"], before["timing_events"],
        "a name correction must not cost anybody their result"
    );
    assert_eq!(after["accepted"], before["accepted"]);

    let renamed = after["accepted_reads"]
        .as_array()
        .expect("crossings")
        .iter()
        .find(|crossing| crossing["bib"] == "104")
        .expect("bib 104 still has a crossing");
    assert_eq!(renamed["name"], "Ada Lovelace");
}

#[tokio::test]
async fn a_chip_assigned_to_nobody_is_refused_at_import() {
    // Assigning a chip to a bib that is not on the roster would produce a crossing credited
    // to nobody, discovered at the finish line. It is refused at configuration time, when
    // there is still somebody to ask.
    let console = Console::new();
    console.configure_a_five_k().await;

    let path = console.write("stray.csv", "bib,chip\n999,E28011606000020000000999\n");
    let stderr = console.expect_failure(&["chips", "import", &path]).await;

    assert!(
        stderr.contains("not on the roster"),
        "the error must name the problem: {stderr}"
    );
}

#[tokio::test]
async fn an_unconfigured_database_refuses_to_guess() {
    let console = Console::new();
    console.run(&["init"]).await;

    let stderr = console.expect_failure(&["derive"]).await;
    assert!(
        stderr.contains("no races are configured"),
        "the error must say what to do next: {stderr}"
    );

    // `doctor` is the diagnostic, so it reports rather than refuses.
    let doctor = console.json(&["doctor"]).await;
    assert_eq!(doctor["errors"], 0);
    assert!(doctor["warnings"].as_u64().expect("a count") > 0);
}

#[tokio::test]
async fn two_races_make_an_unqualified_command_ambiguous_rather_than_arbitrary() {
    // Operating the wrong race is a mistake nobody notices until the results are wrong.
    let console = Console::new();
    console.configure_a_five_k().await;
    console
        .run(&[
            "race",
            "create",
            "--name",
            "10K",
            "--event",
            "Spring Series",
        ])
        .await;

    let stderr = console.expect_failure(&["derive"]).await;
    assert!(
        stderr.contains("several races") && stderr.contains("5K") && stderr.contains("10K"),
        "the error must list the candidates: {stderr}"
    );

    // Naming one resolves it.
    console
        .run(&["simulate", "--race", "5K", "--scenario", "five-k"])
        .await;
    assert_eq!(
        console.json(&["derive", "--race", "5K"]).await["accepted"],
        24
    );
}

#[tokio::test]
async fn a_second_race_in_the_same_database_does_not_borrow_the_first_ones_antennas() {
    // The scoped antenna map. An unscoped one would resolve a 5K read to a 5K checkpoint
    // while deriving the 10K, inventing a crossing at a checkpoint that race does not have.
    let console = Console::new();
    console.configure_a_five_k().await;
    console
        .run(&[
            "race",
            "create",
            "--name",
            "10K",
            "--event",
            "Spring Series",
        ])
        .await;
    console
        .run(&[
            "checkpoint",
            "add",
            "--race",
            "10K",
            "--name",
            "finish",
            "--kind",
            "finish",
        ])
        .await;
    console
        .run(&["simulate", "--race", "5K", "--scenario", "five-k"])
        .await;

    let ten_k = console.json(&["derive", "--race", "10K"]).await;
    assert_eq!(
        ten_k["accepted"], 0,
        "the 10K has no antenna mapped, so it must interpret nothing"
    );
    assert_eq!(
        ten_k["rejections_by_reason"]["unmapped_reader"], ten_k["raw_reads"],
        "every read must be withheld as unmapped, not silently dropped"
    );

    // And the 5K is unaffected.
    assert_eq!(
        console.json(&["derive", "--race", "5K"]).await["accepted"],
        24
    );
}

#[tokio::test]
async fn doctor_names_the_configuration_mistake_that_produces_no_results() {
    // The single most common setup failure: the mat works, reads land, nothing scores,
    // because the reader was never mapped to a checkpoint.
    let console = Console::new();
    console.run(&["init"]).await;
    console.run(&["event", "create", "--name", "Day"]).await;
    console.run(&["race", "create", "--name", "5K"]).await;
    console
        .run(&["checkpoint", "add", "--name", "finish", "--kind", "finish"])
        .await;

    let output = Command::new(BINARY)
        .args([
            "--database",
            &console.database,
            "--format",
            "compact",
            "doctor",
        ])
        .output()
        .await
        .expect("run doctor");

    assert!(
        !output.status.success(),
        "doctor must exit non-zero when it finds errors, so it can gate a race start"
    );
    let report: Value =
        serde_json::from_slice(&output.stdout).expect("doctor emits JSON even when it fails");
    let findings = report["findings"].as_array().expect("findings");

    assert!(
        findings.iter().any(|finding| {
            finding["check"] == "config.readers" && finding["severity"] == "error"
        }),
        "doctor missed the unmapped reader: {report}"
    );
    assert!(
        findings
            .iter()
            .any(|finding| finding["check"] == "config.roster"),
        "doctor missed the empty roster: {report}"
    );
}

#[tokio::test]
async fn a_backup_taken_after_a_race_holds_every_read() {
    let console = Console::new();
    console.configure_a_five_k().await;
    let simulated = console.json(&["simulate", "--scenario", "five-k"]).await;

    let snapshot = console.path("snapshot.db");
    let backup = console.json(&["backup", "create", &snapshot]).await;

    assert_eq!(backup["raw_reads"], simulated["reads_persisted"]);
    assert!(backup["bytes"].as_u64().expect("a size") > 0);
    assert!(Path::new(&snapshot).exists());

    // The snapshot is a working database: deriving from it reaches the same conclusion.
    let from_snapshot = Command::new(BINARY)
        .args(["--database", &snapshot, "--format", "compact", "derive"])
        .output()
        .await
        .expect("derive from the snapshot");
    assert!(from_snapshot.status.success());
    let derived: Value = serde_json::from_slice(&from_snapshot.stdout).expect("JSON");
    assert_eq!(derived["accepted"], 24);

    // And a second backup to the same path refuses rather than overwriting.
    let stderr = console
        .expect_failure(&["backup", "create", &snapshot])
        .await;
    assert!(stderr.contains("already exists"), "{stderr}");
}

#[tokio::test]
async fn crossings_export_as_csv_with_a_row_per_crossing() {
    let console = Console::new();
    console.configure_a_five_k().await;
    console.run(&["simulate", "--scenario", "five-k"]).await;

    let destination = console.path("crossings.csv");
    console
        .run(&[
            "export",
            "crossings",
            "--as",
            "csv",
            "--output",
            &destination,
        ])
        .await;

    let csv = std::fs::read_to_string(&destination).expect("read export");
    let lines: Vec<&str> = csv.lines().collect();

    assert_eq!(lines.len(), 25, "a header plus 24 crossings");
    assert!(
        lines[0].starts_with("checkpoint,at,bib,name,chip,lap"),
        "header was: {}",
        lines[0]
    );
    // No placement column. Scoring is Milestone 4, and a `place` column produced before the
    // rules exist is a number somebody publishes and nobody can defend.
    assert!(!lines[0].contains("place"), "header was: {}", lines[0]);
    assert!(
        lines.iter().skip(1).any(|line| line.contains("Runner 104")),
        "the roster was not joined onto the crossings"
    );

    // JSON is the same data in the other shape.
    let json = console.json(&["export", "crossings", "--as", "json"]).await;
    assert_eq!(json.as_array().expect("an array").len(), 24);
}

#[tokio::test]
async fn reader_status_reports_what_it_observed_and_claims_nothing_else() {
    let console = Console::new();
    console.configure_a_five_k().await;
    console.run(&["simulate", "--scenario", "five-k"]).await;

    let status = console.json(&["reader", "status"]).await;
    let mat = &status.as_array().expect("an array")[0];

    assert_eq!(mat["id"], "mat");
    assert!(mat["reads"].as_u64().expect("a count") > 0);
    assert_eq!(mat["covers"].as_array().expect("coverage").len(), 2);

    // No reader protocol exists until Milestone 3, so there must be nothing here that
    // asserts a connection. See docs/hardware-support.md.
    assert!(mat.get("connected").is_none(), "{mat}");
    assert_eq!(mat["endpoint"], Value::Null);
}

#[tokio::test]
async fn reads_can_be_paged_and_followed_without_blocking_the_writer() {
    let console = Console::new();
    console.configure_a_five_k().await;
    console.run(&["simulate", "--scenario", "five-k"]).await;

    let first_page = console.json(&["reads", "--limit", "1"]).await;
    assert_eq!(first_page["seq"], 1);

    // `--follow` against a finished race still terminates once it hits the limit, which is
    // what makes it testable at all.
    let followed = console
        .run(&["reads", "--follow", "--limit", "3", "--interval-ms", "50"])
        .await;
    assert_eq!(
        followed.lines().count(),
        3,
        "follow must emit one JSON object per line"
    );
    for line in followed.lines() {
        serde_json::from_str::<Value>(line).expect("each line is a complete JSON object");
    }
}
