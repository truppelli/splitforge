//! `splitforge-ship`: publishes to RaceDay Connect, as a process of its own
//! ([ADR-0046](../../../../docs/adr/0046-raceday-connect-is-reached-with-ureq-and-rustls-from-a-process-of-its-own.md),
//! [ADR-0047](../../../../docs/adr/0047-the-shipper-reads-the-event-database-and-keeps-its-own.md)).
//!
//! It reads the event database read-only, and keeps everything of its own in `ship.db`: the
//! pairing and its token, and the course each published race needs. The token is stored there
//! and nowhere else, so it is in no backup of the event database and no diagnostic bundle.
//!
//! This is the composition root's package, so it may name `splitforge-sync`; the read path may
//! not (`tests/read_path_boundary.rs`). Sending is the next slice.

// No panicking on any path reachable during an event: a corrupt frame, a missing
// field, or an out-of-range value must become an error the caller can act on, never a
// timer that stops mid-race. Test code is exempt, where panicking on the unexpected is
// the point. See CONTRIBUTING.md, "Code standards".
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use rusqlite::{Connection, OptionalExtension, params};
use splitforge_domain::CheckpointKind;
use splitforge_storage::{ConfigStore, RaceSelection};
use splitforge_sync::raceday::contract::PairRequest;
use splitforge_sync::raceday::course::Course;
use splitforge_sync::raceday::publish;
use splitforge_sync::raceday::transport::Client;

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

    /// Show the pairing and the courses. Never the token.
    Status,
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
            Ok(serde_json::json!({
                "paired": pairing,
                "courses": courses(&state)?,
            }))
        }
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
    match version {
        0 => state.execute_batch(
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
        )?,
        1 => {}
        newer => bail!(
            "{} is schema version {newer}, newer than this build",
            path.display()
        ),
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
