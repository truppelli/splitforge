//! The Milestone 4 exit criterion, as an executable claim.
//!
//! > A test event imports a roster, records reads, publishes a provisional revision,
//! > applies a DQ correction, and retains **both** revisions with a complete audit trail
//! > explaining the difference.
//!
//! Everything here goes through the **binary**. No test reaches for `ResultStore`, calls
//! `score`, or opens SQLite — because the claim is about what an organizer can do and later
//! defend, and a claim tested against a path they do not have is not the claim.
//!
//! The interesting assertion is not that revision 2 is correct. It is that revision 1 is
//! still exactly what it was after revision 2 exists.

use serde_json::Value;
use tempfile::TempDir;
use tokio::process::Command;

/// The `splitforge` binary, built by Cargo for this test run.
const BINARY: &str = env!("CARGO_BIN_EXE_splitforge");

/// The fixture chips, so a hand-built roster lines up with the `five-k` crossing plan.
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
        let out = self.run(args).await;
        serde_json::from_str(&out).unwrap_or_else(|error| {
            panic!("`splitforge {}` emitted {out:?}: {error}", args.join(" "))
        })
    }

    fn write(&self, name: &str, contents: &str) -> String {
        let path = self.directory.join(name);
        std::fs::write(&path, contents).expect("write file");
        path.display().to_string()
    }

    fn path(&self, name: &str) -> String {
        self.directory.join(name).display().to_string()
    }

    /// Configures a 5K through the CLI alone, then times it.
    async fn time_a_five_k(&self) {
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

        self.run(&["race", "start", "--at", "2026-04-11T08:00:00Z"])
            .await;
        self.run(&["simulate", "--scenario", "five-k"]).await;
        self.run(&["race", "stop"]).await;
    }
}

/// Finds one participant's line in a revision or preview payload.
fn line<'a>(payload: &'a Value, bib: &str) -> &'a Value {
    payload["entries"]
        .as_array()
        .expect("entries is an array")
        .iter()
        .find(|entry| entry["bib"] == bib)
        .unwrap_or_else(|| panic!("bib {bib} is missing from {payload}"))
}

/// The bib of whoever the fixture leaves without a finish crossing.
fn the_dnf(preview: &Value) -> String {
    preview["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .find(|entry| entry["status"] == "dnf")
        .and_then(|entry| entry["bib"].as_str())
        .expect("the five-k fixture includes someone who does not finish")
        .to_owned()
}

// ---------------------------------------------------------------------------------
// The exit criterion
// ---------------------------------------------------------------------------------

#[tokio::test]
async fn a_dq_correction_supersedes_the_results_without_erasing_what_was_published() {
    let console = Console::new();
    console.time_a_five_k().await;

    // Provisional results.
    let published = console
        .json(&[
            "results",
            "publish",
            "--status",
            "provisional",
            "--reason",
            "provisional results",
        ])
        .await;
    assert_eq!(published["revision"], 1);
    assert_eq!(published["status"], "provisional");

    let first = console.json(&["results", "show", "--revision", "1"]).await;
    let winner = line(&first, "104");
    assert_eq!(winner["place"], 1, "104 crosses first in the fixture");
    assert_eq!(winner["status"], "finished");
    let runner_up_bib = first["entries"][1]["bib"]
        .as_str()
        .expect("a second place exists")
        .to_owned();
    assert_eq!(first["entries"][1]["place"], 2);

    // The referee disqualifies the winner.
    console
        .run(&[
            "results",
            "declare",
            "--bib",
            "104",
            "--status",
            "dq",
            "--reason",
            "cut the course at the turnaround",
        ])
        .await;

    let corrected = console
        .json(&[
            "results",
            "publish",
            "--status",
            "final",
            "--reason",
            "bib 104 disqualified after review",
        ])
        .await;
    assert_eq!(corrected["revision"], 2);
    assert_eq!(corrected["changed"], true);

    // Revision 2 reflects the correction.
    let second = console.json(&["results", "show", "--revision", "2"]).await;
    let disqualified = line(&second, "104");
    assert_eq!(disqualified["status"], "dq");
    assert_eq!(disqualified["status_source"], "declared");
    assert_eq!(
        disqualified["status_reason"], "cut the course at the turnaround",
        "the reason travels with the result, not just the audit log"
    );
    assert!(
        disqualified["place"].is_null(),
        "a disqualified runner takes no place"
    );
    assert!(
        disqualified["gun_time"].is_string(),
        "the clock did not lie about when they crossed"
    );
    assert_eq!(
        line(&second, &runner_up_bib)["place"],
        1,
        "everyone behind the disqualified runner moves up"
    );

    // And revision 1 is exactly what it was. This is the claim.
    let first_again = console.json(&["results", "show", "--revision", "1"]).await;
    assert_eq!(
        first_again, first,
        "revision 1 was published; superseding it is honest, rewriting it is not"
    );
    assert_eq!(line(&first_again, "104")["status"], "finished");
    assert_eq!(line(&first_again, "104")["place"], 1);
}

#[tokio::test]
async fn the_diff_between_two_revisions_explains_what_changed_and_for_whom() {
    let console = Console::new();
    console.time_a_five_k().await;
    console
        .run(&[
            "results",
            "publish",
            "--status",
            "provisional",
            "--reason",
            "provisional",
        ])
        .await;
    console
        .run(&[
            "results",
            "declare",
            "--bib",
            "104",
            "--status",
            "dq",
            "--reason",
            "course cut",
        ])
        .await;
    console
        .run(&[
            "results",
            "publish",
            "--status",
            "final",
            "--reason",
            "after review",
        ])
        .await;

    let diff = console
        .json(&["results", "diff", "--from", "1", "--to", "2"])
        .await;

    assert_eq!(diff["from"], 1);
    assert_eq!(diff["to"], 2);
    assert_eq!(diff["from_reason"], "provisional");
    assert_eq!(diff["to_reason"], "after review");

    let changes = diff["changes"].as_array().expect("changes");
    let disqualified = changes
        .iter()
        .find(|change| change["bib"] == "104")
        .expect("the DQ is in the diff");
    assert_eq!(disqualified["change"], "status");
    assert_eq!(disqualified["status_from"], "finished");
    assert_eq!(disqualified["status_to"], "dq");
    assert_eq!(disqualified["reason"], "course cut");

    // Everyone who moved up a place is also a change; the ones who did not are counted.
    assert!(
        diff["changed"].as_u64().unwrap_or(0) > 1,
        "the DQ moved other people's places too: {diff}"
    );
    assert!(
        diff["unchanged"].as_u64().unwrap_or(0) > 0,
        "the runners behind an unplaced entry did not all change: {diff}"
    );
}

#[tokio::test]
async fn the_audit_trail_explains_the_difference_between_the_revisions() {
    let console = Console::new();
    console.time_a_five_k().await;
    console
        .run(&[
            "results",
            "publish",
            "--status",
            "provisional",
            "--reason",
            "provisional results",
        ])
        .await;
    console
        .run(&[
            "results",
            "declare",
            "--bib",
            "104",
            "--status",
            "dq",
            "--reason",
            "cut the course",
        ])
        .await;
    console
        .run(&[
            "results",
            "publish",
            "--status",
            "final",
            "--reason",
            "bib 104 disqualified",
        ])
        .await;

    let audit = console.json(&["audit"]).await;
    let entries = audit.as_array().expect("audit is an array");
    let actions: Vec<&str> = entries
        .iter()
        .filter_map(|entry| entry["action"].as_str())
        .collect();

    assert_eq!(
        actions
            .iter()
            .filter(|action| **action == "results.publish")
            .count(),
        2,
        "both publications are recorded: {actions:?}"
    );
    assert!(
        actions.contains(&"results.declare"),
        "the declaration that caused the change is recorded: {actions:?}"
    );

    // The test docs/timing-model.md § 8 sets: can the database alone explain what changed
    // between the provisional and the final results?
    let declaration = entries
        .iter()
        .find(|entry| entry["action"] == "results.declare")
        .expect("the declaration row");
    let detail = &declaration["detail"];
    assert_eq!(detail["bib"], "104");
    assert_eq!(detail["status"], "dq");
    assert_eq!(detail["reason"], "cut the course");

    let publications: Vec<&Value> = entries
        .iter()
        .filter(|entry| entry["action"] == "results.publish")
        .collect();
    let digests: Vec<&Value> = publications
        .iter()
        .map(|entry| &entry["detail"]["digest"])
        .collect();
    assert_ne!(
        digests[0], digests[1],
        "the two publications recorded different result digests"
    );
}

#[tokio::test]
async fn every_declaration_is_kept_including_the_ones_that_were_reversed() {
    let console = Console::new();
    console.time_a_five_k().await;

    console
        .run(&[
            "results",
            "declare",
            "--bib",
            "104",
            "--status",
            "dq",
            "--reason",
            "reported for cutting the course",
        ])
        .await;
    console
        .run(&[
            "results",
            "declare",
            "--bib",
            "104",
            "--status",
            "finished",
            "--reason",
            "appeal upheld: marshal was at the wrong corner",
        ])
        .await;

    let declarations = console.json(&["results", "declarations"]).await;
    let rows = declarations.as_array().expect("declarations");
    assert_eq!(rows.len(), 2, "reversing a DQ appends; it does not erase");
    assert_eq!(rows[0]["status"], "dq");
    assert_eq!(rows[0]["in_force"], false);
    assert_eq!(rows[1]["status"], "finished");
    assert_eq!(rows[1]["in_force"], true);

    // And scoring uses the newest.
    let preview = console.json(&["results", "preview"]).await;
    assert_eq!(line(&preview, "104")["status"], "finished");
    assert_eq!(line(&preview, "104")["place"], 1);
}

// ---------------------------------------------------------------------------------
// Scoring
// ---------------------------------------------------------------------------------

#[tokio::test]
async fn a_runner_who_started_but_never_finished_is_a_dnf_not_a_missing_row() {
    let console = Console::new();
    console.time_a_five_k().await;

    let preview = console.json(&["results", "preview"]).await;
    let dnf = the_dnf(&preview);

    assert_eq!(
        preview["entries"].as_array().map(Vec::len),
        Some(12),
        "the whole roster is scored, not just the finishers"
    );
    assert_eq!(preview["dnf"], 1);
    assert_eq!(preview["dns"], 0);
    assert!(
        line(&preview, &dnf)["place"].is_null(),
        "a DNF takes no place"
    );
    assert!(
        line(&preview, &dnf)["scoring_time_ms"].is_null(),
        "and has no time"
    );
}

#[tokio::test]
async fn a_registered_runner_who_never_appeared_is_a_dns() {
    let console = Console::new();
    console.time_a_five_k().await;

    // Somebody registers on the day and never starts. No chip, no crossings.
    let roster = console.write("late.csv", "bib,name\n199,Late Entry\n");
    console.run(&["roster", "import", &roster]).await;

    let preview = console.json(&["results", "preview"]).await;
    assert_eq!(preview["dns"], 1);
    assert_eq!(line(&preview, "199")["status"], "dns");
    assert_eq!(line(&preview, "199")["status_source"], "derived");
}

#[tokio::test]
async fn chip_time_and_gun_time_differ_and_the_start_mode_decides_placement() {
    let console = Console::new();
    console.time_a_five_k().await;

    let by_gun = console.json(&["results", "preview"]).await;
    assert_eq!(by_gun["start_mode"], "gun");
    let gun_winner = &by_gun["entries"][0];
    assert_eq!(
        gun_winner["scoring_time"], gun_winner["gun_time"],
        "gun mode places on gun time"
    );

    console
        .run(&["policy", "set", "--start-mode", "chip"])
        .await;

    let by_chip = console.json(&["results", "preview"]).await;
    assert_eq!(by_chip["start_mode"], "chip");
    let chip_winner = &by_chip["entries"][0];
    assert_eq!(
        chip_winner["scoring_time"], chip_winner["chip_time"],
        "chip mode places on chip time"
    );

    // The difference between the two times is exactly how long after the gun that runner
    // crossed the start mat — which is the entire reason chip time exists.
    let gun_at = time::OffsetDateTime::parse(
        by_chip["gun_time"].as_str().expect("a recorded gun"),
        &time::format_description::well_known::Rfc3339,
    )
    .expect("the gun time is RFC 3339");

    let mut crossed_after_the_gun = 0_u32;
    let mut crossed_before_the_gun = 0_u32;
    for entry in by_chip["entries"].as_array().expect("entries") {
        let (Some(chip), Some(start_at)) =
            (entry["chip_time_ms"].as_i64(), entry["start_at"].as_str())
        else {
            continue;
        };
        let gun = entry["gun_time_ms"]
            .as_i64()
            .expect("a finisher has a gun time");
        let start_at =
            time::OffsetDateTime::parse(start_at, &time::format_description::well_known::Rfc3339)
                .expect("start crossing is RFC 3339");
        let offset_ms = i64::try_from((start_at - gun_at).whole_milliseconds()).expect("in range");

        assert_eq!(
            gun - chip,
            offset_ms,
            "chip time is gun time less the wait on the start line: {entry}"
        );
        if offset_ms > 0 {
            crossed_after_the_gun += 1;
        } else {
            // Someone stepping over the mat a fraction before the gun gets a chip time
            // *longer* than their gun time. That is correct, and it is the case a
            // `chip <= gun` assertion would have quietly enshrined as a bug.
            crossed_before_the_gun += 1;
        }
    }
    assert!(
        crossed_after_the_gun > 0,
        "most of the field starts behind the line"
    );
    assert!(
        crossed_before_the_gun > 0,
        "the fixture includes a runner over the mat before the gun, and the arithmetic \
         still has to hold for them"
    );

    // The start mode is race configuration, and `policy show` reports it.
    let policy = console.json(&["policy", "show"]).await;
    assert_eq!(policy["start_mode"], "chip");
}

#[tokio::test]
async fn the_start_mode_is_snapshotted_so_changing_it_cannot_restate_a_published_sheet() {
    let console = Console::new();
    console.time_a_five_k().await;
    console
        .run(&[
            "results",
            "publish",
            "--status",
            "provisional",
            "--reason",
            "scored from the gun",
        ])
        .await;

    console
        .run(&["policy", "set", "--start-mode", "chip"])
        .await;

    let published = console.json(&["results", "show", "--revision", "1"]).await;
    assert_eq!(
        published["start_mode"], "gun",
        "revision 1 was scored from the gun and still says so"
    );
}

// ---------------------------------------------------------------------------------
// Publishing
// ---------------------------------------------------------------------------------

#[tokio::test]
async fn republishing_identical_results_is_refused_unless_asked_for() {
    let console = Console::new();
    console.time_a_five_k().await;
    console
        .run(&[
            "results",
            "publish",
            "--status",
            "provisional",
            "--reason",
            "first",
        ])
        .await;

    let error = console
        .expect_failure(&[
            "results",
            "publish",
            "--status",
            "provisional",
            "--reason",
            "again",
        ])
        .await;
    assert!(
        error.contains("identical to revision 1"),
        "a revision that changes nothing is usually a mistake: {error}"
    );
    assert!(
        error.contains("--allow-unchanged"),
        "and it says how: {error}"
    );

    // Promoting provisional results to final without any scoring change is legitimate.
    let promoted = console
        .json(&[
            "results",
            "publish",
            "--status",
            "final",
            "--reason",
            "no appeals received",
            "--allow-unchanged",
        ])
        .await;
    assert_eq!(promoted["revision"], 2);
    assert_eq!(promoted["status"], "final");
    assert_eq!(promoted["changed"], false);
}

#[tokio::test]
async fn preview_writes_nothing() {
    let console = Console::new();
    console.time_a_five_k().await;

    console.json(&["results", "preview"]).await;
    console.json(&["results", "preview"]).await;

    let listed = console.json(&["results", "list"]).await;
    assert_eq!(
        listed.as_array().map(Vec::len),
        Some(0),
        "the rehearsal for the irreversible command must not be irreversible"
    );
}

#[tokio::test]
async fn preview_and_publish_agree_because_they_score_the_same_way() {
    let console = Console::new();
    console.time_a_five_k().await;

    let preview = console.json(&["results", "preview"]).await;
    console
        .run(&[
            "results",
            "publish",
            "--status",
            "provisional",
            "--reason",
            "as previewed",
        ])
        .await;
    let published = console.json(&["results", "show"]).await;

    assert_eq!(
        preview["entries"], published["entries"],
        "what the rehearsal showed is what was written"
    );
}

#[tokio::test]
async fn revisions_are_listed_with_who_published_them_and_why() {
    let console = Console::new();
    console.time_a_five_k().await;
    console
        .run(&[
            "--actor",
            "alice",
            "results",
            "publish",
            "--status",
            "provisional",
            "--reason",
            "provisional",
        ])
        .await;
    console
        .run(&[
            "results",
            "declare",
            "--bib",
            "104",
            "--status",
            "dq",
            "--reason",
            "course cut",
        ])
        .await;
    console
        .run(&[
            "--actor",
            "bob",
            "results",
            "publish",
            "--status",
            "final",
            "--reason",
            "after review",
        ])
        .await;

    let listed = console.json(&["results", "list"]).await;
    let rows = listed.as_array().expect("revisions");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["revision"], 1);
    assert_eq!(rows[0]["actor"], "alice");
    assert_eq!(rows[0]["status"], "provisional");
    assert_eq!(rows[1]["revision"], 2);
    assert_eq!(rows[1]["actor"], "bob");
    assert_eq!(rows[1]["status"], "final");
    assert_ne!(rows[0]["digest"], rows[1]["digest"]);
}

#[tokio::test]
async fn declaring_a_status_for_a_bib_nobody_has_is_refused() {
    let console = Console::new();
    console.time_a_five_k().await;

    let error = console
        .expect_failure(&[
            "results", "declare", "--bib", "999", "--status", "dq", "--reason", "typo",
        ])
        .await;
    assert!(
        error.contains("999"),
        "the error names the bib that was not found: {error}"
    );
}

#[tokio::test]
async fn an_unrecognized_status_is_refused_rather_than_guessed() {
    let console = Console::new();
    console.time_a_five_k().await;

    // "dsq" is a real abbreviation elsewhere in the sport. Guessing would disqualify
    // somebody.
    let error = console
        .expect_failure(&[
            "results",
            "declare",
            "--bib",
            "104",
            "--status",
            "dsq",
            "--reason",
            "course cut",
        ])
        .await;
    assert!(
        error.contains("finished, dns, dnf, dq"),
        "the error lists what is accepted: {error}"
    );
}

#[tokio::test]
async fn results_cannot_be_shown_before_they_are_published() {
    let console = Console::new();
    console.time_a_five_k().await;

    let error = console.expect_failure(&["results", "show"]).await;
    assert!(
        error.contains("no published results"),
        "and it says what to run: {error}"
    );
    assert!(error.contains("results publish"), "{error}");
}

#[tokio::test]
async fn a_course_with_no_finish_line_says_so_rather_than_scoring_nobody() {
    let console = Console::new();
    console.run(&["init"]).await;
    console
        .run(&["fixture", "load", "--fixture", "multi-lap"])
        .await;

    // The criterium fixture is a lap course: no start line, no finish line. Milestone 4
    // scores one of each, and an empty results sheet would be a wrong answer that looks
    // like a right one.
    let error = console
        .expect_failure(&["results", "preview", "--race", "Criterium"])
        .await;
    assert!(
        error.contains("no finish checkpoint"),
        "the error explains why nothing could be scored: {error}"
    );
}

#[tokio::test]
async fn doctor_catches_a_course_that_cannot_be_scored_before_the_gun_rather_than_after() {
    let console = Console::new();
    console.run(&["init"]).await;
    console.run(&["event", "create", "--name", "Day"]).await;
    console
        .run(&[
            "race",
            "create",
            "--name",
            "5K",
            "--start",
            "2026-04-11T08:00:00Z",
        ])
        .await;
    console
        .run(&["checkpoint", "add", "--name", "finish", "--kind", "finish"])
        .await;

    // Chip timing needs a start line. A checkpoint cannot be added retroactively to a mat
    // that was never there, so finding this out at `results publish` is finding out too
    // late.
    console
        .run(&["policy", "set", "--start-mode", "chip"])
        .await;

    let output = Command::new(BINARY)
        .args(["--database", &console.database, "--format", "compact"])
        .arg("doctor")
        .output()
        .await
        .expect("run doctor");
    assert!(
        !output.status.success(),
        "doctor exits non-zero when it finds something that produces wrong results"
    );

    let report: Value =
        serde_json::from_slice(&output.stdout).expect("doctor still reports on stdout");
    let findings = report["findings"].as_array().expect("findings");
    let scoring = findings
        .iter()
        .find(|finding| finding["check"] == "scoring.start")
        .unwrap_or_else(|| panic!("expected a scoring.start finding in {report}"));
    assert_eq!(scoring["severity"], "error");
    assert!(
        scoring["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("--start-mode gun")),
        "the finding says how to fix it: {scoring}"
    );
}

// ---------------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------------

#[tokio::test]
async fn results_export_as_csv_with_a_row_per_participant() {
    let console = Console::new();
    console.time_a_five_k().await;
    console
        .run(&[
            "results",
            "publish",
            "--status",
            "final",
            "--reason",
            "final results",
        ])
        .await;

    let path = console.path("results.csv");
    console
        .run(&["export", "results", "--as", "csv", "--output", &path])
        .await;
    let csv = std::fs::read_to_string(&path).expect("read export");
    let lines: Vec<&str> = csv.lines().collect();

    assert_eq!(lines.len(), 13, "a header and twelve runners");
    assert!(lines[0].starts_with("place,bib,name,status"));
    assert!(
        lines[1].starts_with("1,104,Runner 104,finished"),
        "the winner is the first row: {}",
        lines[1]
    );
    assert!(
        csv.contains("0:17:32"),
        "times are formatted for a human, not left as milliseconds: {csv}"
    );
}

#[tokio::test]
async fn results_export_as_json_carries_the_policy_that_produced_them() {
    let console = Console::new();
    console.time_a_five_k().await;
    console
        .run(&[
            "results",
            "publish",
            "--status",
            "final",
            "--reason",
            "final results",
        ])
        .await;

    let path = console.path("results.json");
    console
        .run(&["export", "results", "--as", "json", "--output", &path])
        .await;
    let body = std::fs::read_to_string(&path).expect("read export");
    let parsed: Value = serde_json::from_str(&body).expect("valid json");

    assert_eq!(parsed["format"], "splitforge.results");
    assert_eq!(parsed["version"], 1);
    assert_eq!(parsed["revision"]["number"], 1);
    assert_eq!(parsed["revision"]["reason"], "final results");
    assert_eq!(parsed["revision"]["policy"]["start_mode"], "gun");
    assert!(
        parsed["revision"]["policy"]["timing"]["min_interval_ms"].is_number(),
        "the dedup window that produced these crossings travels with the file"
    );
    assert_eq!(
        parsed["entries"].as_array().map(Vec::len),
        Some(12),
        "every registered runner, not just the finishers"
    );
}

#[tokio::test]
async fn an_older_revision_can_still_be_exported_after_it_is_superseded() {
    let console = Console::new();
    console.time_a_five_k().await;
    console
        .run(&[
            "results",
            "publish",
            "--status",
            "provisional",
            "--reason",
            "provisional",
        ])
        .await;
    console
        .run(&[
            "results",
            "declare",
            "--bib",
            "104",
            "--status",
            "dq",
            "--reason",
            "course cut",
        ])
        .await;
    console
        .run(&[
            "results",
            "publish",
            "--status",
            "final",
            "--reason",
            "after review",
        ])
        .await;

    // Somebody screenshotted the provisional sheet and is asking about it a week later.
    let path = console.path("provisional.csv");
    console
        .run(&[
            "export",
            "results",
            "--revision",
            "1",
            "--as",
            "csv",
            "--output",
            &path,
        ])
        .await;
    let csv = std::fs::read_to_string(&path).expect("read export");
    assert!(
        csv.contains("1,104,Runner 104,finished"),
        "revision 1 exports exactly as it was published: {csv}"
    );

    let latest = console.path("final.csv");
    console
        .run(&["export", "results", "--as", "csv", "--output", &latest])
        .await;
    let final_csv = std::fs::read_to_string(&latest).expect("read export");
    assert!(
        final_csv.contains(",104,Runner 104,dq,declared,course cut"),
        "and the default export is the newest revision: {final_csv}"
    );
}

#[tokio::test]
async fn a_backup_taken_after_publication_holds_the_revisions() {
    let console = Console::new();
    console.time_a_five_k().await;
    console
        .run(&[
            "results",
            "publish",
            "--status",
            "final",
            "--reason",
            "final results",
        ])
        .await;

    let snapshot = console.path("snapshot.db");
    console.run(&["backup", "create", &snapshot]).await;

    let output = Command::new(BINARY)
        .args(["--database", &snapshot, "--format", "compact"])
        .args(["results", "show", "--revision", "1"])
        .output()
        .await
        .expect("read the backup");
    assert!(
        output.status.success(),
        "the snapshot holds the published results: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let restored: Value =
        serde_json::from_slice(&output.stdout).expect("the backup returns a revision");
    assert_eq!(restored["revision"], 1);
    assert_eq!(line(&restored, "104")["place"], 1);
}
