//! `splitforge-ship`: publishes to RaceDay Connect, as a process of its own
//! ([ADR-0046](../../../../docs/adr/0046-raceday-connect-is-reached-with-ureq-and-rustls-from-a-process-of-its-own.md),
//! [ADR-0047](../../../../docs/adr/0047-the-shipper-reads-the-event-database-and-keeps-its-own.md)).
//!
//! It reads the event database read-only, and keeps everything of its own in `ship.db`: the
//! pairing and its token, and the course each published race needs. The token is stored there
//! and nowhere else, so it is in no backup of the event database and no diagnostic bundle.
//!
//! `run` is the shipper itself. Every interval it asks whether anything changed: the event
//! database's `data_version`, or `ship.db`'s when a `pair` or `course` command has committed.
//! When something has, it derives each published race, and sends the manifest if it changed,
//! the crossings of every runner whose crossings changed, and every result revision not yet
//! delivered. What was delivered is recorded in `ship.db`, so a restart resends nothing that
//! arrived. A retry waits for `Retry-After` or the backoff, an unpaired box stops until it is
//! paired again, and a refusal is recorded and set aside, never resent (ADR-0039).
//!
//! This is the composition root's package, so it may name `splitforge-sync`; the read path may
//! not (`tests/read_path_boundary.rs`).

// No panicking on any path reachable during an event: a corrupt frame, a missing
// field, or an out-of-range value must become an error the caller can act on, never a
// timer that stops mid-race. Test code is exempt, where panicking on the unexpected is
// the point. See CONTRIBUTING.md, "Code standards".
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use rusqlite::{Connection, OptionalExtension, params};
use splitforge_domain::{CheckpointKind, RaceConfig, RawReadJournal as _, TimingEvent};
use splitforge_engine::{DerivationInput, derive};
use splitforge_storage::{ConfigStore, RaceSelection, ResultStore, SqliteJournal};
use splitforge_sync::raceday::contract::{
    Crossing, MAX_CROSSINGS_PER_BATCH, PairRequest, RunnerRef,
};
use splitforge_sync::raceday::course::Course;
use splitforge_sync::raceday::delivery::{Outcome, backoff};
use splitforge_sync::raceday::publish;
use splitforge_sync::raceday::transport::{Client, Sent};

#[derive(Debug, Parser)]
#[command(name = "splitforge-ship", version)]
struct Args {
    /// The shipper's own database: the pairing, the courses, and what was sent.
    #[arg(
        long,
        value_name = "PATH",
        default_value = "/var/lib/splitforge-ship/ship.db"
    )]
    state: PathBuf,

    /// The event database, which the shipper only reads.
    #[arg(
        long,
        value_name = "PATH",
        default_value = "/var/lib/splitforge/event.db"
    )]
    database: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Trade the code a race director read out for this box's token.
    Pair {
        /// RaceDay Connect's address, such as `https://raceday.example`.
        #[arg(long, value_name = "URL")]
        url: String,
        /// The pairing code, such as `K7QM-4XPD`.
        #[arg(long, value_name = "CODE")]
        code: String,
        /// What the race page calls this box.
        #[arg(long, value_name = "NAME")]
        device_name: Option<String>,
    },

    /// Record what RaceDay Connect needs to know about a race's course: checked against the
    /// race, and against every other course, before it is kept.
    Course {
        /// The SplitForge race.
        #[arg(long, value_name = "NAME")]
        race: String,
        /// The distance key RaceDay Connect files it under. Chosen once: a new key is a new
        /// distance, with its results starting again at revision 1.
        #[arg(long, value_name = "KEY")]
        key: String,
        /// The certified distance in metres, all laps included.
        #[arg(long, value_name = "METRES")]
        meters: f64,
        /// Where an intermediate split sits along one lap. Repeat for each split.
        #[arg(long = "split", value_name = "CHECKPOINT=METRES", value_parser = parse_split)]
        splits: Vec<(String, f64)>,
    },

    /// Show the pairing, the courses and how delivery is going. Never the token.
    Status,

    /// Send what changed, every interval, until stopped.
    Run {
        /// Seconds between looks at the event database (ADR-0047).
        #[arg(long, value_name = "SECONDS", default_value_t = 10)]
        interval: u64,
        /// One pass, then exit, printing what it did.
        #[arg(long)]
        once: bool,
    },
}

fn main() -> ExitCode {
    match run(Args::parse()) {
        Ok(report) => match serde_json::to_string_pretty(&report) {
            Ok(text) => {
                println!("{text}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("splitforge-ship: {error}");
                ExitCode::FAILURE
            }
        },
        Err(error) => {
            eprintln!("splitforge-ship: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Args) -> Result<serde_json::Value> {
    let state = open_state(&args.state)?;
    match args.command {
        Command::Pair {
            url,
            code,
            device_name,
        } => {
            let paired = Client::new(&url)?.pair(&PairRequest { code, device_name })?;
            state.execute(
                "INSERT OR REPLACE INTO pairing (id, url, race_id, race_name, slug, token)
                 VALUES (1, ?1, ?2, ?3, ?4, ?5)",
                params![
                    url,
                    paired.race_id,
                    paired.race_name,
                    paired.slug,
                    paired.token
                ],
            )?;
            // A new pairing may be another RaceDay Connect race, which holds none of what was
            // sent. Everything is sent again, which RaceDay Connect takes as replacements.
            state.execute_batch(
                "DELETE FROM sent_manifest; DELETE FROM sent_crossings; DELETE FROM sent_revisions;
                 UPDATE delivery SET unpaired = 0, last_error = NULL;",
            )?;
            Ok(serde_json::json!({
                "paired": {
                    "url": url,
                    "race_id": paired.race_id,
                    "race_name": paired.race_name,
                    "slug": paired.slug,
                }
            }))
        }

        Command::Course {
            race,
            key,
            meters,
            splits,
        } => {
            let store = ConfigStore::open_read_only(&args.database).with_context(|| {
                format!("opening the event database {}", args.database.display())
            })?;
            let race = match store.resolve_race(Some(&race))? {
                RaceSelection::One(race) => race,
                RaceSelection::None => bail!("no race is named {race:?}"),
                RaceSelection::Ambiguous(races) => bail!(
                    "{race:?} matches {} races: {}",
                    races.len(),
                    races
                        .iter()
                        .map(|race| race.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            };
            let config = store.load(race.id)?;

            // A split the race does not have would be ignored silently, and the operator's
            // typo would surface later as a split with no position.
            for (name, _) in &splits {
                let known = config.checkpoints.iter().any(|checkpoint| {
                    checkpoint.kind == CheckpointKind::Split && checkpoint.name == *name
                });
                if !known {
                    bail!(
                        "race {:?} has no split checkpoint named {name:?}",
                        race.name
                    );
                }
            }
            let course = Course {
                race: race.id,
                key,
                meters,
                splits: splits.into_iter().collect::<BTreeMap<_, _>>(),
            };

            // Checked with every other published race, so two cannot share a key.
            let mut courses = courses(&state)?;
            courses.retain(|other| other.race != course.race);
            courses.push(course.clone());
            let configs = courses
                .iter()
                .map(|course| store.load(course.race))
                .collect::<Result<Vec<_>, _>>()?;
            let pairs: Vec<_> = configs.iter().zip(courses.iter()).collect();
            publish::manifest(&pairs)?;

            state.execute(
                "INSERT OR REPLACE INTO courses (race_id, course_json) VALUES (?1, ?2)",
                params![course.race.to_string(), serde_json::to_string(&course)?],
            )?;
            Ok(serde_json::json!({ "course": course, "race": race.name }))
        }

        Command::Status => {
            let pairing = state
                .query_row(
                    "SELECT url, race_id, race_name, slug FROM pairing WHERE id = 1",
                    [],
                    |row| {
                        Ok(serde_json::json!({
                            "url": row.get::<_, String>(0)?,
                            "race_id": row.get::<_, String>(1)?,
                            "race_name": row.get::<_, String>(2)?,
                            "slug": row.get::<_, String>(3)?,
                        }))
                    },
                )
                .optional()?;
            let delivery = state.query_row(
                "SELECT unpaired, last_error, last_attempt_us, last_delivered_us FROM delivery",
                [],
                |row| {
                    Ok(serde_json::json!({
                        "unpaired": row.get::<_, i64>(0)? != 0,
                        "last_error": row.get::<_, Option<String>>(1)?,
                        "last_attempt": row.get::<_, Option<i64>>(2)?.and_then(rfc3339),
                        "last_delivered": row.get::<_, Option<i64>>(3)?.and_then(rfc3339),
                    }))
                },
            )?;
            let runners: i64 =
                state.query_row("SELECT COUNT(*) FROM sent_crossings", [], |row| row.get(0))?;
            let revisions: i64 = state.query_row(
                "SELECT COUNT(*) FROM sent_revisions WHERE outcome = 'delivered'",
                [],
                |row| row.get(0),
            )?;
            Ok(serde_json::json!({
                "paired": pairing,
                "courses": courses(&state)?,
                "delivery": delivery,
                "runners_sent": runners,
                "revisions_delivered": revisions,
            }))
        }

        Command::Run { interval, once } => watch(
            &state,
            &args.database,
            Duration::from_secs(interval.max(1)),
            once,
        ),
    }
}

/// `ship.db`, created with its schema on first use and readable by this user alone: it holds
/// the token.
fn open_state(path: &Path) -> Result<Connection> {
    let state = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("restricting {} to its owner", path.display()))?;
    }
    let version: i64 = state.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version > 2 {
        bail!(
            "{} is schema version {version}, newer than this build",
            path.display()
        );
    }
    if version == 0 {
        state.execute_batch(
            "BEGIN;
             CREATE TABLE pairing (
                 id        INTEGER PRIMARY KEY CHECK (id = 1),
                 url       TEXT NOT NULL,
                 race_id   TEXT NOT NULL,
                 race_name TEXT NOT NULL,
                 slug      TEXT NOT NULL,
                 token     TEXT NOT NULL
             ) STRICT;
             CREATE TABLE courses (
                 race_id     TEXT PRIMARY KEY,
                 course_json TEXT NOT NULL
             ) STRICT;
             PRAGMA user_version = 1;
             COMMIT;",
        )?;
    }
    if version <= 1 {
        state.execute_batch(
            "BEGIN;
             -- What RaceDay Connect holds, as far as the shipper knows (ADR-0047).
             CREATE TABLE sent_manifest (
                 id            INTEGER PRIMARY KEY CHECK (id = 1),
                 manifest_json TEXT NOT NULL,
                 delivered     INTEGER NOT NULL
             ) STRICT;
             CREATE TABLE sent_crossings (
                 runner_json    TEXT PRIMARY KEY,
                 crossings_json TEXT NOT NULL
             ) STRICT;
             CREATE TABLE sent_revisions (
                 race_id TEXT NOT NULL,
                 number  INTEGER NOT NULL,
                 outcome TEXT NOT NULL CHECK (outcome IN ('delivered', 'refused')),
                 PRIMARY KEY (race_id, number)
             ) STRICT;
             CREATE TABLE delivery (
                 id                INTEGER PRIMARY KEY CHECK (id = 1),
                 unpaired          INTEGER NOT NULL DEFAULT 0,
                 last_error        TEXT,
                 last_attempt_us   INTEGER,
                 last_delivered_us INTEGER
             ) STRICT;
             INSERT INTO delivery (id) VALUES (1);
             PRAGMA user_version = 2;
             COMMIT;",
        )?;
    }
    Ok(state)
}

fn courses(state: &Connection) -> Result<Vec<Course>> {
    let mut statement = state.prepare("SELECT course_json FROM courses ORDER BY race_id")?;
    let mut rows = statement.query([])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(serde_json::from_str(&row.get::<_, String>(0)?)?);
    }
    Ok(out)
}

fn parse_split(text: &str) -> Result<(String, f64), String> {
    let (name, meters) = text
        .rsplit_once('=')
        .ok_or_else(|| format!("{text:?} is not CHECKPOINT=METRES"))?;
    let meters = meters
        .trim()
        .parse::<f64>()
        .map_err(|_| format!("{meters:?} is not a number of metres"))?;
    Ok((name.trim().to_owned(), meters))
}

/// The event database, opened read-only, kept open between passes so `data_version` compares
/// against this connection's last look.
struct Event {
    journal: SqliteJournal,
    config: ConfigStore,
    results: ResultStore,
}

impl Event {
    fn open(path: &Path) -> Result<Self> {
        let context = || format!("opening the event database {}", path.display());
        Ok(Self {
            journal: SqliteJournal::open_read_only(path).with_context(context)?,
            config: ConfigStore::open_read_only(path).with_context(context)?,
            results: ResultStore::open_read_only(path).with_context(context)?,
        })
    }
}

/// How a pass ended.
enum Pass {
    /// Everything owed was sent, or set aside.
    Done,
    /// RaceDay Connect asked for a retry, or did not answer. Wait, then try again.
    Retry(Option<Duration>),
    /// Nothing more can be sent until something changes: the box is unpaired, or the
    /// manifest was refused and the course has to be corrected.
    Stopped,
}

/// Watches the event database and sends what changed, until stopped or for one pass.
fn watch(
    state: &Connection,
    database: &Path,
    interval: Duration,
    once: bool,
) -> Result<serde_json::Value> {
    let mut event: Option<Event> = None;
    let mut seen: Option<(i64, i64)> = None;
    let mut attempts = 0_u32;
    let mut retry_at: Option<Instant> = None;

    loop {
        let mut wait = interval;
        if event.is_none() {
            match Event::open(database) {
                Ok(opened) => event = Some(opened),
                Err(error) => {
                    // No writer has it open (ADR-0047 § 7), or it is not migrated yet.
                    note(state, &format!("waiting for the event database: {error:#}"))?;
                    if once {
                        return Ok(serde_json::json!({ "pass": "waiting" }));
                    }
                    std::thread::sleep(wait);
                    continue;
                }
            }
        }
        let Some(open) = event.as_ref() else {
            continue;
        };

        let versions = (
            open.journal.data_version()?,
            state.query_row("PRAGMA data_version", [], |row| row.get::<_, i64>(0))?,
        );
        let waiting = retry_at.is_some_and(|at| Instant::now() < at);
        let due = retry_at.is_some() && !waiting;
        if once || (!waiting && (seen != Some(versions) || due)) {
            seen = Some(versions);
            let outcome = match paired(state)? {
                None => {
                    note(state, "not paired; run `splitforge-ship pair`")?;
                    Ok(Pass::Stopped)
                }
                Some(pairing) => pass(state, open, &pairing),
            };
            let label = match outcome {
                Ok(Pass::Done) => {
                    attempts = 0;
                    retry_at = None;
                    "done"
                }
                Ok(Pass::Retry(after)) => {
                    attempts = attempts.saturating_add(1);
                    let after = after.unwrap_or_else(|| backoff(attempts));
                    retry_at = Some(Instant::now() + after);
                    wait = wait.min(after);
                    "retry"
                }
                Ok(Pass::Stopped) => {
                    retry_at = None;
                    "stopped"
                }
                Err(error) => {
                    note(state, &format!("{error:#}"))?;
                    // Opened again next time, in case the database went away under it.
                    event = None;
                    "error"
                }
            };
            if once {
                return Ok(serde_json::json!({ "pass": label }));
            }
        } else if let Some(at) = retry_at {
            wait = wait.min(at.saturating_duration_since(Instant::now()));
        }
        std::thread::sleep(wait.max(Duration::from_millis(100)));
    }
}

/// Where to send, and as whom.
struct Pairing {
    url: String,
    race_id: String,
    token: String,
}

/// The pairing, unless there is none or RaceDay Connect has said it is no longer paired.
fn paired(state: &Connection) -> Result<Option<Pairing>> {
    Ok(state
        .query_row(
            "SELECT url, race_id, token FROM pairing, delivery
             WHERE pairing.id = 1 AND delivery.unpaired = 0",
            [],
            |row| {
                Ok(Pairing {
                    url: row.get(0)?,
                    race_id: row.get(1)?,
                    token: row.get(2)?,
                })
            },
        )
        .optional()?)
}

/// One pass: derive every published race, and send what RaceDay Connect does not hold.
fn pass(state: &Connection, event: &Event, pairing: &Pairing) -> Result<Pass> {
    let courses = courses(state)?;
    if courses.is_empty() {
        note(state, "no course recorded; run `splitforge-ship course`")?;
        return Ok(Pass::Stopped);
    }
    let client = Client::new(&pairing.url)?;
    let (token, race) = (pairing.token.as_str(), pairing.race_id.as_str());

    // One derivation per race per pass, at about 0.5 KB a read (ADR-0047).
    let reads = event.journal.read_all()?;
    let mut derived: Vec<(RaceConfig, &Course, Vec<TimingEvent>)> = Vec::new();
    for course in &courses {
        let config = event.config.load(course.race)?;
        let chips = config
            .chips()
            .context("the race's chip assignments overlap")?;
        let manual: Vec<_> = event
            .journal
            .manual_entries(course.race)?
            .into_iter()
            .map(|stored| stored.entry)
            .collect();
        let derivation = derive(&DerivationInput {
            reads: &reads,
            policy: &config.policy,
            chips: &chips,
            antennas: &config.antennas,
            manual: &manual,
            gun_time: config.gun_time(),
        });
        derived.push((config, course, derivation.timing_events));
    }
    drop(reads);
    let mut problem: Option<String> = None;

    // The course. Sent again when it changes, which includes the gun (ADR-0039).
    let pairs: Vec<(&RaceConfig, &Course)> = derived
        .iter()
        .map(|(config, course, _)| (config, *course))
        .collect();
    let manifest = publish::manifest(&pairs)?;
    let json = serde_json::to_string(&manifest)?;
    let last: Option<(String, bool)> = state
        .query_row(
            "SELECT manifest_json, delivered FROM sent_manifest WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get::<_, i64>(1)? != 0)),
        )
        .optional()?;
    match last {
        Some((sent, true)) if sent == json => {}
        Some((sent, false)) if sent == json => return Ok(Pass::Stopped),
        _ => {
            let sent = client.put_manifest(token, race, &manifest);
            match sent.outcome {
                Outcome::Delivered => record_manifest(state, &json, true)?,
                Outcome::Refused => {
                    // Nothing else is sent against a course RaceDay Connect will not hold.
                    record_manifest(state, &json, false)?;
                    note(state, &format!("the course was refused: {}", sent.detail))?;
                    return Ok(Pass::Stopped);
                }
                Outcome::Retry { .. } | Outcome::Unpaired => return settle(state, &sent),
            }
        }
    }

    // Crossings, each changed runner restated whole (ADR-0039).
    let mut now: BTreeMap<RunnerRef, Vec<Crossing>> = BTreeMap::new();
    for (config, course, events) in &derived {
        now.extend(publish::crossings(config, course, events)?.runners);
    }
    let changed = publish::changed(&sent_crossings(state)?, &now);
    for batch in publish::batches(&now, &changed, MAX_CROSSINGS_PER_BATCH) {
        let sent = client.post_crossings(token, race, &batch);
        match sent.outcome {
            Outcome::Delivered | Outcome::Refused => {
                if sent.outcome == Outcome::Refused {
                    problem = Some(format!("a batch of crossings was refused: {}", sent.detail));
                }
                // A refused batch is set aside with the delivered ones: the same bytes would be
                // refused again. Each runner is sent afresh once their crossings change.
                for runner in &batch.replaces {
                    record_runner(state, runner, now.get(runner))?;
                }
            }
            Outcome::Retry { .. } | Outcome::Unpaired => return settle(state, &sent),
        }
    }

    // Result revisions not yet delivered, oldest first.
    for (config, course, events) in &derived {
        let race_id = course.race.to_string();
        for summary in event.results.revisions(course.race)? {
            let done = state
                .query_row(
                    "SELECT 1 FROM sent_revisions WHERE race_id = ?1 AND number = ?2",
                    params![race_id, summary.number],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            if done {
                continue;
            }
            let Some(revision) = event.results.revision(course.race, summary.number)? else {
                continue;
            };
            let outcome = match publish::revision(config, course, &revision, events) {
                Ok(document) => {
                    let sent = client.post_results(token, race, &document);
                    match sent.outcome {
                        Outcome::Delivered => "delivered",
                        Outcome::Refused => {
                            problem = Some(format!(
                                "revision {} was refused: {}",
                                summary.number, sent.detail
                            ));
                            "refused"
                        }
                        Outcome::Retry { .. } | Outcome::Unpaired => {
                            return settle(state, &sent);
                        }
                    }
                }
                Err(error) => {
                    problem = Some(format!(
                        "revision {} cannot be published: {error}",
                        summary.number
                    ));
                    "refused"
                }
            };
            state.execute(
                "INSERT INTO sent_revisions (race_id, number, outcome) VALUES (?1, ?2, ?3)",
                params![race_id, summary.number, outcome],
            )?;
        }
    }

    state.execute(
        "UPDATE delivery SET last_error = ?1, last_attempt_us = ?2, last_delivered_us = ?2",
        params![problem, now_us()],
    )?;
    if let Some(problem) = problem {
        eprintln!("splitforge-ship: {problem}");
    }
    Ok(Pass::Done)
}

/// What an answer that ends the pass means: wait and retry, or stop until paired again.
fn settle(state: &Connection, sent: &Sent) -> Result<Pass> {
    if let Outcome::Retry { after } = sent.outcome {
        note(state, &format!("will retry: {}", sent.detail))?;
        return Ok(Pass::Retry(after));
    }
    state.execute("UPDATE delivery SET unpaired = 1", [])?;
    note(
        state,
        &format!(
            "RaceDay Connect says this box is not paired; run `splitforge-ship pair`: {}",
            sent.detail
        ),
    )?;
    Ok(Pass::Stopped)
}

/// Records what went wrong, for `status` and the journal.
fn note(state: &Connection, problem: &str) -> Result<()> {
    eprintln!("splitforge-ship: {problem}");
    state.execute(
        "UPDATE delivery SET last_error = ?1, last_attempt_us = ?2",
        params![problem, now_us()],
    )?;
    Ok(())
}

fn record_manifest(state: &Connection, json: &str, delivered: bool) -> Result<()> {
    state.execute(
        "INSERT OR REPLACE INTO sent_manifest (id, manifest_json, delivered) VALUES (1, ?1, ?2)",
        params![json, i64::from(delivered)],
    )?;
    Ok(())
}

fn sent_crossings(state: &Connection) -> Result<BTreeMap<RunnerRef, Vec<Crossing>>> {
    let mut statement = state.prepare("SELECT runner_json, crossings_json FROM sent_crossings")?;
    let mut rows = statement.query([])?;
    let mut out = BTreeMap::new();
    while let Some(row) = rows.next()? {
        out.insert(
            serde_json::from_str(&row.get::<_, String>(0)?)?,
            serde_json::from_str(&row.get::<_, String>(1)?)?,
        );
    }
    Ok(out)
}

/// What RaceDay Connect now holds for `runner`: `crossings`, or nothing.
fn record_runner(
    state: &Connection,
    runner: &RunnerRef,
    crossings: Option<&Vec<Crossing>>,
) -> Result<()> {
    let key = serde_json::to_string(runner)?;
    match crossings {
        Some(crossings) => state.execute(
            "INSERT OR REPLACE INTO sent_crossings (runner_json, crossings_json) VALUES (?1, ?2)",
            params![key, serde_json::to_string(crossings)?],
        )?,
        None => state.execute("DELETE FROM sent_crossings WHERE runner_json = ?1", [key])?,
    };
    Ok(())
}

fn now_us() -> i64 {
    i64::try_from(time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000)
        .unwrap_or(i64::MAX)
}

fn rfc3339(micros: i64) -> Option<String> {
    time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(micros) * 1_000)
        .ok()
        .and_then(|at| {
            at.format(&time::format_description::well_known::Rfc3339)
                .ok()
        })
}
