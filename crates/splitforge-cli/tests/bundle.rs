//! The diagnostic bundle, and the promise that makes it usable.
//!
//! > The bundle carries no participant names, bib numbers, or chip identifiers, so it can be
//! > attached to a public issue without being read first.
//!
//! That claim is only worth anything if it is checked against the **bytes** of a real
//! bundle, taken from a fully operated event — roster imported, race run, results published,
//! a competitor disqualified by bib with a reason naming them. Checking it by reading the
//! serializer would prove the serializer, not the file, and the file is what gets emailed.
//!
//! So the central test here builds that event through the binary, produces a bundle, and
//! searches the whole file for every name, bib, chip, and operator the event contained. It
//! is deliberately a substring search over the raw text rather than a walk over the parsed
//! JSON: a leak that arrives inside a field nobody thought to check is exactly the leak this
//! is for.

use serde_json::Value;
use tempfile::TempDir;
use tokio::process::Command;

/// The `splitforge` binary, built by Cargo for this test run.
const BINARY: &str = env!("CARGO_BIN_EXE_splitforge");

/// A floor no real machine can satisfy, for reaching the failing-`doctor` path on demand.
const IMPOSSIBLE_FLOOR_MB: &str = "999999999";

/// Distinctive enough that finding it in a bundle cannot be a coincidence.
const OPERATOR: &str = "phillip.truppelli";

/// The chip prefix the `five-k` scenario emits, before the zero-padded bib.
const CHIP_PREFIX: &str = "E2801160600002";

/// The chips the fixture reads, so a hand-built roster lines up with the crossing plan.
fn chip_for(bib: u32) -> String {
    format!("{CHIP_PREFIX}{bib:010}")
}

#[tokio::test]
async fn a_bundle_from_a_fully_operated_event_names_nobody() {
    let event = Event::new();
    event.configure().await;
    event.run_and_publish().await;

    let bundle = event.bundle(&["doctor"]).await;
    let text = bundle.text;

    // Names. The roster is "Runner 101" through "Runner 112".
    assert!(
        !text.contains("Runner"),
        "a participant name reached the bundle"
    );

    // Chips, in every form the database holds them.
    assert!(
        !text.contains(CHIP_PREFIX),
        "a chip identifier reached the bundle"
    );
    for bib in 101_u32..=112 {
        let chip = chip_for(bib);
        assert!(!text.contains(&chip), "chip {chip} reached the bundle");
    }

    // Bibs. Searched as the quoted JSON strings the database stores them as, because a bare
    // `104` is also a plausible count and asserting on that would fail on arithmetic.
    for bib in 101_u32..=112 {
        assert!(
            !text.contains(&format!("\"{bib}\"")),
            "bib {bib} reached the bundle"
        );
    }

    // The operator, who is a person too.
    assert!(
        !text.contains(OPERATOR),
        "the operator's name reached the bundle"
    );

    // And the free text an operator typed, which named a competitor outright.
    assert!(
        !text.contains("cut the course"),
        "an operator's reason reached the bundle"
    );
    assert!(
        !text.contains("gun is the timekeeper"),
        "an operator's note reached the bundle"
    );
    assert!(
        !text.contains("disqualified after review"),
        "a publication reason reached the bundle"
    );
}

#[tokio::test]
async fn a_bundle_still_says_enough_to_debug_the_device() {
    // The other half of the promise. A file that leaks nothing because it contains nothing
    // is not a diagnostic, so this asserts on what a maintainer would actually need.
    let event = Event::new();
    event.configure().await;
    event.run_and_publish().await;

    let json = event.bundle(&["doctor"]).await.json;

    assert_eq!(json["format"], "splitforge.diagnostic-bundle");
    assert_eq!(json["version"], 1);
    assert!(json["generated_at"].as_str().is_some());
    assert!(json["tool_version"].as_str().is_some());

    assert_eq!(
        json["device"]["schema_version"],
        splitforge_storage::SCHEMA_VERSION,
        "the bundle reports the schema the build actually migrated to"
    );
    assert_eq!(json["device"]["min_free_mb"], 256);
    assert_eq!(json["device"]["above_floor"], true);

    let journal = &json["journal"];
    assert_eq!(journal["raw_reads"], 638);
    assert_eq!(journal["first_seq"], 1);
    assert_eq!(journal["last_seq"], 638);
    assert_eq!(journal["sequence_gaps"], 0);
    assert_eq!(
        journal["distinct_chips"], 13,
        "twelve entrants plus a stray"
    );
    assert_eq!(journal["sidecar_present"], true);
    assert_eq!(journal["sidecar"]["records"], 638);
    assert_eq!(journal["sidecar"]["missing_from_database"], 0);
    assert!(
        journal.get("max_write_latency_ms").is_none(),
        "write latency cannot be measured from a simulated run — the simulator stamps \
         `received_at` from the fixture's race day — and a field that reports the fixture's \
         age as storage latency is worse than no field: {journal}"
    );

    // Reads per antenna, which is how a dead mat becomes visible without any read leaving
    // the device.
    let by_source = journal["by_source"].as_object().expect("by_source");
    assert!(
        by_source.keys().any(|key| key.starts_with("mat/")),
        "got: {by_source:?}"
    );

    let race = &json["races"][0];
    assert_eq!(race["race"], "5K");
    assert_eq!(race["event"], "Spring Series");
    assert_eq!(race["participants"], 12);
    assert_eq!(race["assignments"], 12);
    assert_eq!(race["participants_without_a_chip"], 0);
    assert_eq!(race["start_mode"], "gun");
    assert!(race["gun_time"].as_str().is_some());
    assert_eq!(
        race["checkpoints"].as_array().expect("checkpoints").len(),
        2
    );
    assert_eq!(race["readers"][0]["id"], "mat");
    assert!(
        race["readers"][0]["covers"]
            .as_array()
            .expect("covers")
            .iter()
            .any(|line| line.as_str().is_some_and(|line| line.contains("finish"))),
        "the antenna map is the most common misconfiguration and has to survive: {race}"
    );

    // Two revisions, with the digests that distinguish them. Enough to answer "did the
    // published numbers change" without any of the numbers being about a person.
    let revisions = race["revisions"].as_array().expect("revisions");
    assert_eq!(revisions.len(), 2);
    assert_eq!(revisions[0]["number"], 1);
    assert_eq!(revisions[0]["status"], "provisional");
    assert_eq!(revisions[1]["status"], "final");
    assert_ne!(
        revisions[0]["digest"], revisions[1]["digest"],
        "the correction has to be visible: {revisions:?}"
    );
    assert_eq!(revisions[0]["entries"], 12);

    // The audit trail is present as a sequence of actions, which is what reconstructs what
    // the operator did.
    let audit = json["audit"].as_array().expect("audit");
    assert!(
        audit.iter().any(|entry| entry["action"] == "race.start"),
        "got: {audit:?}"
    );
    assert!(
        audit
            .iter()
            .any(|entry| entry["action"] == "results.publish"),
        "got: {audit:?}"
    );
}

#[tokio::test]
async fn the_chip_that_nobody_owns_is_still_traceable_through_the_bundle() {
    // The diagnosis the tokens exist for. The `five-k` scenario emits one crossing by a chip
    // that is on no roster — a mat that works, reads that land, nothing that scores — and
    // the bundle has to make that visible without publishing the chip.
    let event = Event::new();
    event.configure().await;
    event.run_and_publish().await;

    let json = event.bundle(&["doctor"]).await.json;
    let race = &json["races"][0];

    assert_eq!(race["unassigned_chips_read_total"], 1);
    let tokens = race["unassigned_chips_read"].as_array().expect("tokens");
    assert_eq!(tokens.len(), 1);

    let token = tokens[0].as_str().expect("a token");
    assert!(token.starts_with("h:"), "got: {token}");
    assert!(
        !token.contains(CHIP_PREFIX),
        "the token is standing in for the chip, not carrying it: {token}"
    );
}

#[tokio::test]
async fn two_bundles_from_the_same_device_share_no_tokens() {
    // The salt is per-bundle and discarded, so tokens cannot be followed from one report to
    // the next — or matched against a roster somebody holds. Within a bundle they still
    // correlate, which is the trade the design makes.
    let event = Event::new();
    event.configure().await;
    event.run_and_publish().await;

    let first = event.bundle(&["doctor"]).await.json;
    let second = event.bundle(&["doctor"]).await.json;

    let token_of = |json: &Value| {
        json["races"][0]["unassigned_chips_read"][0]
            .as_str()
            .expect("a token")
            .to_owned()
    };
    assert_ne!(
        token_of(&first),
        token_of(&second),
        "a stable token would be a chip identifier with extra steps"
    );

    // And the operator hashed in the audit trail moves with it, for the same reason.
    let actor_of = |json: &Value| {
        json["audit"][0]["actor"]
            .as_str()
            .expect("actor")
            .to_owned()
    };
    assert_ne!(actor_of(&first), actor_of(&second));

    // Within one bundle, though, one operator ran every command and hashes to one token.
    let actors: Vec<&str> = first["audit"]
        .as_array()
        .expect("audit")
        .iter()
        .filter_map(|entry| entry["actor"].as_str())
        .collect();
    assert!(actors.len() > 1, "the event had several audited steps");
    assert!(
        actors.windows(2).all(|pair| pair[0] == pair[1]),
        "one operator must hash to one token within a bundle: {actors:?}"
    );
}

#[tokio::test]
async fn a_finding_that_can_name_a_participant_travels_without_its_message() {
    // `config.chips` lists the entrants who have no chip, by bib. It is the finding the
    // allowlist exists for: the check name reaches the bundle so a maintainer knows what
    // fired, and the message stays on the device.
    let event = Event::new();
    event.configure_without_chips().await;

    let bundle = event.bundle(&["doctor"]).await;

    let on_device = event.json(&["doctor"]).await;
    let device_finding = find(&on_device["findings"], "config.chips");
    assert!(
        device_finding["detail"]
            .as_str()
            .expect("detail")
            .contains("101"),
        "the device's own diagnostic still names bibs, which is why it is useful: \
         {device_finding}"
    );

    let bundled = find(&bundle.json["doctor"]["findings"], "config.chips");
    assert_eq!(bundled["severity"], "warning");
    assert_eq!(
        bundled["detail"],
        Value::Null,
        "the message names entrants and must not travel"
    );
    assert!(
        bundled["detail_withheld"].as_str().is_some(),
        "silence without explanation reads as a bug: {bundled}"
    );

    // A finding that carries only counts keeps its message, so withholding is a decision
    // about one check rather than a blanket refusal to say anything.
    let roster = find(&bundle.json["doctor"]["findings"], "config.roster");
    assert!(
        roster.is_null() || roster["detail"].as_str().is_some(),
        "an allowlisted check must keep its detail: {roster}"
    );

    assert!(
        !bundle.text.contains("Runner"),
        "a name reached the bundle through a finding"
    );
    for bib in 101_u32..=112 {
        assert!(
            !bundle.text.contains(&format!("\"{bib}\"")),
            "bib {bib} reached the bundle through a finding"
        );
    }
}

#[tokio::test]
async fn a_bundle_is_written_even_when_doctor_fails() {
    // The case a bundle exists for. Tying it to a clean run would withhold the diagnostic in
    // every situation somebody wants one.
    let event = Event::new();
    event.configure().await;
    event
        .run_ok(&["device", "set", "--min-free-mb", IMPOSSIBLE_FLOOR_MB])
        .await;

    let path = event.path("bundle.json");
    let (_out, stderr, ok) = event.run(&["doctor", "--bundle", &path]).await;
    assert!(!ok, "an unstartable device must still fail its own check");
    assert!(
        stderr.contains("bundle.json"),
        "the operator has to be told where it went: {stderr}"
    );

    let json: Value = serde_json::from_str(&std::fs::read_to_string(&path).expect("read bundle"))
        .expect("the bundle is JSON");

    assert_eq!(json["doctor"]["errors"], 1);
    let finding = find(&json["doctor"]["findings"], "storage.free_space");
    assert_eq!(finding["severity"], "error");
    assert!(
        finding["detail"]
            .as_str()
            .expect("detail")
            .contains("below the"),
        "the reason the device is unhappy is the point of the file: {finding}"
    );
    assert_eq!(json["device"]["above_floor"], false);
}

#[tokio::test]
async fn a_bundle_names_the_database_file_but_never_its_path() {
    // A path on a Pi is `/var/lib/splitforge/event.db` and says nothing. The same path on a
    // laptop runs through a home directory and names the operator — and the temporary
    // directory this test runs in does exactly that on every platform CI builds for.
    let event = Event::new();
    event.configure().await;

    let bundle = event.bundle(&["doctor"]).await;
    assert_eq!(bundle.json["device"]["database"], "event.db");

    let directory = event.directory.path().display().to_string();
    assert!(
        !bundle.text.contains(&directory),
        "the bundle leaked the directory it was taken from: {directory}"
    );

    // Both separators, since a Windows path serialized into JSON arrives escaped.
    let escaped = directory.replace('\\', "\\\\");
    assert!(!bundle.text.contains(&escaped), "escaped path leaked");
}

#[tokio::test]
async fn doctor_without_the_flag_writes_nothing() {
    let event = Event::new();
    event.configure().await;

    event.run_ok(&["doctor"]).await;
    assert!(
        std::fs::read_dir(event.directory.path())
            .expect("read dir")
            .filter_map(Result::ok)
            .all(|entry| entry.file_name() != "bundle.json"),
        "a bundle must be opt-in; it is a file somebody is going to send somewhere"
    );
}

// ---- harness --------------------------------------------------------------------

/// A bundle as both bytes and parsed JSON. The text is what the privacy assertions search.
struct Captured {
    text: String,
    json: Value,
}

struct Event {
    directory: TempDir,
    database: String,
}

impl Event {
    fn new() -> Self {
        let directory = TempDir::new().expect("temp dir");
        let database = directory.path().join("event.db").display().to_string();
        Self {
            directory,
            database,
        }
    }

    fn path(&self, name: &str) -> String {
        self.directory.path().join(name).display().to_string()
    }

    fn write(&self, name: &str, contents: &str) -> String {
        let path = self.directory.path().join(name);
        std::fs::write(&path, contents).expect("write file");
        path.display().to_string()
    }

    async fn run(&self, args: &[&str]) -> (String, String, bool) {
        let output = Command::new(BINARY)
            .args(["--database", &self.database, "--format", "compact"])
            .args(["--actor", OPERATOR])
            .args(args)
            .output()
            .await
            .expect("run splitforge");
        (
            String::from_utf8(output.stdout).expect("stdout is UTF-8"),
            String::from_utf8(output.stderr).expect("stderr is UTF-8"),
            output.status.success(),
        )
    }

    async fn run_ok(&self, args: &[&str]) -> String {
        let (out, stderr, ok) = self.run(args).await;
        assert!(ok, "`splitforge {}` failed: {stderr}", args.join(" "));
        out
    }

    async fn json(&self, args: &[&str]) -> Value {
        serde_json::from_str(&self.run_ok(args).await).expect("command emits JSON")
    }

    /// Takes a bundle alongside `args`, tolerating a non-zero exit from `doctor` itself.
    async fn bundle(&self, args: &[&str]) -> Captured {
        let path = self.path("bundle.json");
        let mut with_flag: Vec<&str> = args.to_vec();
        with_flag.extend_from_slice(&["--bundle", &path]);
        let (_out, stderr, _ok) = self.run(&with_flag).await;

        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("no bundle at {path} ({error}): {stderr}"));
        let json = serde_json::from_str(&text).expect("the bundle is JSON");
        Captured { text, json }
    }

    /// Configures the 5K through the CLI alone, roster and chips included.
    async fn configure(&self) {
        self.configure_without_chips().await;
        let mut chips = String::from("bib,chip\n");
        for bib in 101_u32..=112 {
            chips.push_str(&format!("{bib},{}\n", chip_for(bib)));
        }
        let chips = self.write("assignments.csv", &chips);
        self.run_ok(&["chips", "import", &chips]).await;
    }

    /// The same event with no chip assignments, so `config.chips` fires with bibs in it.
    async fn configure_without_chips(&self) {
        self.run_ok(&["init"]).await;
        self.run_ok(&["event", "create", "--name", "Spring Series"])
            .await;
        self.run_ok(&[
            "race",
            "create",
            "--name",
            "5K",
            "--start",
            "2026-04-11T08:00:00Z",
        ])
        .await;
        self.run_ok(&["checkpoint", "add", "--name", "start", "--kind", "start"])
            .await;
        self.run_ok(&["checkpoint", "add", "--name", "finish", "--kind", "finish"])
            .await;

        let mut roster = String::from("bib,name\n");
        for bib in 101_u32..=112 {
            roster.push_str(&format!("{bib},Runner {bib}\n"));
        }
        let roster = self.write("participants.csv", &roster);
        self.run_ok(&["roster", "import", &roster]).await;

        self.run_ok(&["reader", "add", "--id", "mat", "--label", "Start/finish"])
            .await;
        self.run_ok(&[
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
        self.run_ok(&[
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
    }

    /// Runs the race and takes it all the way to a corrected final revision.
    ///
    /// The DQ matters here: it is the step that puts a bib and an operator's prose about a
    /// named competitor into the database, which is what the bundle has to not repeat.
    async fn run_and_publish(&self) {
        self.run_ok(&["race", "start", "--note", "the gun is the timekeeper today"])
            .await;
        self.run_ok(&["simulate", "--scenario", "five-k"]).await;
        self.run_ok(&["race", "stop"]).await;
        self.run_ok(&["derive"]).await;
        self.run_ok(&[
            "results",
            "publish",
            "--status",
            "provisional",
            "--reason",
            "provisional results",
        ])
        .await;
        self.run_ok(&[
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
        self.run_ok(&[
            "results",
            "publish",
            "--status",
            "final",
            "--reason",
            "bib 104 disqualified after review",
        ])
        .await;
    }
}

/// The first finding with this check name, or `Null` when there is none.
fn find(findings: &Value, check: &str) -> Value {
    findings
        .as_array()
        .expect("findings")
        .iter()
        .find(|finding| finding["check"] == check)
        .cloned()
        .unwrap_or(Value::Null)
}
