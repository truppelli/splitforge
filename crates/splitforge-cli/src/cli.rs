//! Command-line surface.
//!
//! The Milestone 2 operator vocabulary. Everything an event needs — creating it, importing
//! a roster and chip assignments, declaring readers and what they cover, setting the timing
//! policy, recording the gun, watching reads arrive, exporting, backing up, and checking the
//! database is sound — without ever opening SQLite by hand. That is the milestone's exit
//! criterion, stated as a command list.
//!
//! ## `--race` is usually unnecessary
//!
//! Most commands take an optional `--race`. With one race configured it is noise, so
//! leaving it off selects that race. With several, leaving it off is an error that lists
//! the candidates rather than picking one — operating the wrong race is a mistake nobody
//! notices until the results are wrong.

use std::path::PathBuf;
use std::str::FromStr;

use clap::{Args, Parser, Subcommand, ValueEnum};

/// SplitForge: offline-first race timing.
#[derive(Debug, Parser)]
#[command(name = "splitforge", version, about, long_about = None)]
pub struct Cli {
    /// Path to the event database. Created if it does not exist.
    #[arg(long, value_name = "PATH", global = true, default_value = "event.db")]
    pub database: PathBuf,

    /// How to format the JSON on stdout.
    #[arg(long, value_enum, default_value_t = Format::Pretty, global = true)]
    pub format: Format,

    /// Who is running the command, recorded in the audit trail.
    #[arg(long, value_name = "NAME", global = true, default_value = "operator")]
    pub actor: String,

    /// The command to run.
    #[command(subcommand)]
    pub command: Command,
}

/// JSON output shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum Format {
    /// Indented, for reading.
    #[default]
    Pretty,
    /// One line, for piping and diffing.
    Compact,
}

/// Which race a command applies to.
#[derive(Debug, Clone, Args)]
pub struct RaceRef {
    /// Race name or id. Optional when only one race is configured.
    #[arg(long, value_name = "NAME_OR_ID")]
    pub race: Option<String>,
}

/// The operator vocabulary.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create or migrate an event database.
    Init,

    /// Manage events — a race day, which may hold several races.
    #[command(subcommand)]
    Event(EventCommand),

    /// Manage races.
    #[command(subcommand)]
    Race(RaceCommand),

    /// Manage checkpoints on a course.
    #[command(subcommand)]
    Checkpoint(CheckpointCommand),

    /// Manage the roster.
    #[command(subcommand)]
    Roster(RosterCommand),

    /// Manage chip assignments.
    #[command(subcommand)]
    Chips(ChipsCommand),

    /// Manage readers and what they cover.
    #[command(subcommand)]
    Reader(ReaderCommand),

    /// Inspect or change the timing policy.
    #[command(subcommand)]
    Policy(PolicyCommand),

    /// Load a built-in fixture event into the database.
    #[command(subcommand)]
    Fixture(FixtureCommand),

    /// Run a simulated event, appending every read to the journal.
    Simulate {
        /// Which race the simulated reads belong to.
        #[command(flatten)]
        race: RaceRef,

        /// Which crossing plan to run. Must match a loaded fixture.
        #[arg(long, value_name = "SLUG", default_value = "five-k")]
        scenario: String,

        /// Seed for the simulator's random choices. The same seed replays the same race.
        #[arg(long, default_value_t = 0x5F17_F03E)]
        seed: u64,

        /// `immediate`, or a speed multiplier such as `1` for wall-clock time.
        #[arg(long, default_value = "immediate")]
        speed: Speed,
    },

    /// List raw reads: the evidence, exactly as recorded.
    Reads {
        /// Only reads after this journal sequence number.
        #[arg(long, value_name = "SEQ")]
        since: Option<u64>,

        /// Stop after this many reads.
        #[arg(long, value_name = "N")]
        limit: Option<usize>,

        /// Keep watching for new reads instead of exiting.
        ///
        /// Safe to run against a live race: WAL means this never blocks the writer.
        #[arg(long)]
        follow: bool,

        /// How often to poll when following.
        #[arg(long, value_name = "MS", default_value_t = 500)]
        interval_ms: u64,
    },

    /// List accepted reads: one per crossing, after deduplication.
    Accepted {
        /// Which race supplies the roster, checkpoints, and policy.
        #[command(flatten)]
        race: RaceRef,
    },

    /// Re-derive everything from the journal: crossings, suppressions, and timing events.
    Derive {
        /// Which race supplies the roster, checkpoints, and policy.
        #[command(flatten)]
        race: RaceRef,
    },

    /// Score the race, and publish or inspect result revisions.
    #[command(subcommand)]
    Results(ResultsCommand),

    /// Export crossings or published results.
    #[command(subcommand)]
    Export(ExportCommand),

    /// Write a consistent snapshot of the database. Safe to run mid-race.
    #[command(subcommand)]
    Backup(BackupCommand),

    /// Check that the database and its configuration are sound.
    Doctor {
        /// Which race to check. Omit to check every configured race.
        #[command(flatten)]
        race: RaceRef,
    },

    /// Show the audit trail, newest first.
    Audit {
        /// How many entries to show.
        #[arg(long, value_name = "N", default_value_t = 50)]
        limit: usize,
    },

    /// Summarize the database: schema, configuration, and journal.
    Status {
        /// Which race to summarize. Omit when only one is configured.
        #[command(flatten)]
        race: RaceRef,
    },
}

/// Event management.
#[derive(Debug, Subcommand)]
pub enum EventCommand {
    /// Create an event.
    Create {
        /// Human-readable name, unique within the database.
        #[arg(long, value_name = "NAME")]
        name: String,
    },
    /// List events.
    List,
}

/// Race management.
#[derive(Debug, Subcommand)]
pub enum RaceCommand {
    /// Create a race within an event.
    Create {
        /// Event name or id. Optional when only one event exists.
        #[arg(long, value_name = "NAME_OR_ID")]
        event: Option<String>,

        /// Race name, unique within the event.
        #[arg(long, value_name = "NAME")]
        name: String,

        /// Scheduled start, RFC 3339. Stands in as the gun time until one is recorded.
        #[arg(long, value_name = "RFC3339")]
        start: Option<String>,

        /// Laps a participant is expected to complete.
        #[arg(long, value_name = "N", default_value_t = 1)]
        laps: u16,
    },
    /// List races.
    List,
    /// Record the gun.
    ///
    /// Does not gate ingestion — reads are journalled whether or not this was run. It
    /// records the instant that gun-time scoring will use, and it is append-only.
    Start {
        /// Which race.
        #[command(flatten)]
        race: RaceRef,

        /// The instant to record, RFC 3339. Defaults to now.
        #[arg(long, value_name = "RFC3339")]
        at: Option<String>,

        /// Why, for the audit trail.
        #[arg(long, value_name = "TEXT")]
        note: Option<String>,
    },
    /// Record that the race is closed.
    Stop {
        /// Which race.
        #[command(flatten)]
        race: RaceRef,

        /// The instant to record, RFC 3339. Defaults to now.
        #[arg(long, value_name = "RFC3339")]
        at: Option<String>,

        /// Why, for the audit trail.
        #[arg(long, value_name = "TEXT")]
        note: Option<String>,
    },
}

/// Checkpoint management.
#[derive(Debug, Subcommand)]
pub enum CheckpointCommand {
    /// Add a checkpoint to a race.
    Add {
        /// Which race.
        #[command(flatten)]
        race: RaceRef,

        /// Checkpoint name, unique within the race.
        #[arg(long, value_name = "NAME")]
        name: String,

        /// Role on the course: start, split, finish, or lap.
        #[arg(long, value_name = "KIND")]
        kind: String,

        /// Position in the expected sequence. Defaults to the next free slot.
        #[arg(long, value_name = "N")]
        sequence: Option<u16>,
    },
    /// List a race's checkpoints.
    List {
        /// Which race.
        #[command(flatten)]
        race: RaceRef,
    },
}

/// Roster management.
#[derive(Debug, Subcommand)]
pub enum RosterCommand {
    /// Import participants from CSV with a `bib,name` header.
    ///
    /// Re-importing updates people already entered and keeps their identity, so chip
    /// assignments and already-derived timing events stay valid.
    Import {
        /// Which race.
        #[command(flatten)]
        race: RaceRef,

        /// The CSV file.
        #[arg(value_name = "FILE")]
        file: PathBuf,
    },
    /// List a race's roster.
    List {
        /// Which race.
        #[command(flatten)]
        race: RaceRef,
    },
}

/// Chip assignment management.
#[derive(Debug, Subcommand)]
pub enum ChipsCommand {
    /// Import assignments from CSV with a `bib,chip[,valid_from,valid_until]` header.
    Import {
        /// Which race.
        #[command(flatten)]
        race: RaceRef,

        /// The CSV file.
        #[arg(value_name = "FILE")]
        file: PathBuf,
    },
    /// List a race's chip assignments.
    List {
        /// Which race.
        #[command(flatten)]
        race: RaceRef,
    },
}

/// Reader management.
#[derive(Debug, Subcommand)]
pub enum ReaderCommand {
    /// Declare a reader.
    Add {
        /// Operator-assigned identifier, recorded on every read it produces.
        #[arg(long, value_name = "ID")]
        id: String,

        /// Human-readable description.
        #[arg(long, value_name = "TEXT")]
        label: Option<String>,

        /// Where to reach it. Stored but unused until Milestone 3.
        #[arg(long, value_name = "HOST:PORT")]
        endpoint: Option<String>,

        /// How far to trust its clock: trusted, untrusted, or auto.
        #[arg(long, value_name = "TRUST", default_value = "auto")]
        trust: String,
    },
    /// Map a reader, or one antenna on it, to the checkpoint it covers.
    Map {
        /// Which reader.
        #[arg(long, value_name = "ID")]
        reader: String,

        /// Which antenna. Omit to map every antenna on the reader.
        #[arg(long, value_name = "N")]
        antenna: Option<u16>,

        /// Checkpoint name.
        #[arg(long, value_name = "NAME")]
        checkpoint: String,

        /// Which race the checkpoint belongs to.
        #[command(flatten)]
        race: RaceRef,
    },
    /// List configured readers and their mappings.
    List,
    /// Report each reader's configuration and what the journal has seen from it.
    ///
    /// Connection state is deliberately absent: no reader protocol exists until Milestone
    /// 3, so this reports what it can actually observe rather than a status it would have
    /// to invent.
    Status {
        /// How far back "recent" reaches.
        #[arg(long, value_name = "SECONDS", default_value_t = 60)]
        window_secs: u64,
    },
}

/// Timing policy management.
#[derive(Debug, Subcommand)]
pub enum PolicyCommand {
    /// Change the timing policy.
    Set {
        /// Which race.
        #[command(flatten)]
        race: RaceRef,

        /// Apply to one checkpoint instead of the whole race.
        #[arg(long, value_name = "NAME")]
        checkpoint: Option<String>,

        /// Reads of one chip closer together than this belong to one crossing.
        #[arg(long, value_name = "MS")]
        min_interval_ms: Option<u64>,

        /// A lap faster than this is a re-read, not a superhuman lap. Race-wide only.
        #[arg(long, value_name = "MS")]
        min_lap_ms: Option<u64>,

        /// Which read in a burst wins: `first`, `peak-rssi`, or `first-above-rssi:<DBM>`.
        #[arg(long, value_name = "RULE")]
        selection_rule: Option<String>,

        /// Which clock placement is measured from: `gun` or `chip`. Race-wide only.
        ///
        /// Snapshotted into every revision, so changing it later cannot restate what an
        /// already-published sheet meant.
        #[arg(long, value_name = "MODE")]
        start_mode: Option<String>,
    },
    /// Show the timing policy in force.
    Show {
        /// Which race.
        #[command(flatten)]
        race: RaceRef,
    },
}

/// Fixture loading.
#[derive(Debug, Subcommand)]
pub enum FixtureCommand {
    /// Write a built-in fixture's configuration into the database.
    ///
    /// A development convenience, not the supported setup path — it exists so a simulated
    /// race can be reached in one command instead of eight. The CSV import path is what an
    /// operator uses, and it is exercised on its own.
    Load {
        /// Which fixture.
        #[arg(long, value_name = "SLUG", default_value = "five-k")]
        fixture: String,
    },
    /// List the built-in fixtures.
    List,
}

/// Result scoring and revision commands.
#[derive(Debug, Subcommand)]
pub enum ResultsCommand {
    /// Score the race and show what a revision would contain, without publishing.
    ///
    /// The command to run before the one that cannot be undone.
    Preview {
        /// Which race.
        #[command(flatten)]
        race: RaceRef,
    },

    /// Score the race and publish an immutable revision.
    Publish {
        /// Which race.
        #[command(flatten)]
        race: RaceRef,

        /// `provisional` or `final`.
        #[arg(long, value_name = "STATUS", default_value = "provisional")]
        status: String,

        /// Why this revision exists. Required: a revision nobody can explain is one nobody
        /// can defend.
        #[arg(long, value_name = "TEXT")]
        reason: String,

        /// Publish even when the results are identical to the previous revision.
        #[arg(long)]
        allow_unchanged: bool,
    },

    /// Show a published revision.
    Show {
        /// Which race.
        #[command(flatten)]
        race: RaceRef,

        /// Which revision. Omit for the most recent.
        #[arg(long, value_name = "N")]
        revision: Option<u32>,
    },

    /// List every revision of a race, oldest first.
    List {
        /// Which race.
        #[command(flatten)]
        race: RaceRef,
    },

    /// Compare two revisions, line by line.
    ///
    /// What changed between provisional and final, and for whom.
    Diff {
        /// Which race.
        #[command(flatten)]
        race: RaceRef,

        /// The earlier revision.
        #[arg(long, value_name = "N")]
        from: u32,

        /// The later revision.
        #[arg(long, value_name = "N")]
        to: u32,
    },

    /// Declare how a participant's race ended, overriding what the crossings imply.
    ///
    /// The only way a disqualification enters the system: no arrangement of chip reads
    /// implies "cut the course". Appends — reversing one means declaring again.
    Declare {
        /// Which race.
        #[command(flatten)]
        race: RaceRef,

        /// Whose status.
        #[arg(long, value_name = "BIB")]
        bib: String,

        /// `finished`, `dns`, `dnf`, or `dq`.
        #[arg(long, value_name = "STATUS")]
        status: String,

        /// Why. Required.
        #[arg(long, value_name = "TEXT")]
        reason: String,
    },

    /// Show every status declaration, including superseded ones.
    Declarations {
        /// Which race.
        #[command(flatten)]
        race: RaceRef,
    },
}

/// Export commands.
#[derive(Debug, Subcommand)]
pub enum ExportCommand {
    /// Export derived crossings, joined against the roster.
    ///
    /// A diagnostic view, not a published contract: it answers "why is this runner
    /// missing?". The versioned contract is `export results`.
    Crossings {
        /// Which race.
        #[command(flatten)]
        race: RaceRef,

        /// Output shape.
        ///
        /// Named `shape` rather than `format` because the argument id clap derives from a
        /// field name is global across the tree, and `--format` is already the global JSON
        /// shape. Two arguments sharing an id panic at parse time rather than at build
        /// time, which is a failure worth not shipping.
        #[arg(long = "as", value_enum, default_value_t = ExportFormat::Csv)]
        shape: ExportFormat,

        /// Write to a file instead of stdout.
        #[arg(long, value_name = "PATH")]
        output: Option<PathBuf>,
    },

    /// Export a published result revision.
    Results {
        /// Which race.
        #[command(flatten)]
        race: RaceRef,

        /// Which revision. Omit for the most recent.
        #[arg(long, value_name = "N")]
        revision: Option<u32>,

        /// Output shape. Named `shape` for the reason given above.
        #[arg(long = "as", value_enum, default_value_t = ExportFormat::Csv)]
        shape: ExportFormat,

        /// Write to a file instead of stdout.
        #[arg(long, value_name = "PATH")]
        output: Option<PathBuf>,
    },
}

/// Backup commands.
#[derive(Debug, Subcommand)]
pub enum BackupCommand {
    /// Write a consistent snapshot. Never overwrites.
    Create {
        /// Where to write it.
        #[arg(value_name = "PATH")]
        destination: PathBuf,
    },
}

/// Shape of an export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum ExportFormat {
    /// Comma-separated, with a header row.
    #[default]
    Csv,
    /// JSON array.
    Json,
}

/// How fast a simulated event plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Speed {
    /// As fast as the journal accepts writes.
    #[default]
    Immediate,
    /// In elapsed time, `factor` times faster than the race itself.
    Accelerated {
        /// Speed multiplier. `1` is wall-clock time.
        factor: u32,
    },
}

impl FromStr for Speed {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.eq_ignore_ascii_case("immediate") {
            return Ok(Self::Immediate);
        }
        let factor: u32 = value
            .trim_end_matches('x')
            .parse()
            .map_err(|_| format!("expected `immediate` or a speed multiplier, got {value:?}"))?;
        if factor == 0 {
            return Err("a speed multiplier of 0 would never finish".to_owned());
        }
        Ok(Self::Accelerated { factor })
    }
}

impl From<Speed> for splitforge_simulator::Speed {
    fn from(speed: Speed) -> Self {
        match speed {
            Speed::Immediate => Self::Immediate,
            Speed::Accelerated { factor } => Self::Accelerated { factor },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn the_command_tree_is_well_formed() {
        Cli::command().debug_assert();
    }

    #[test]
    fn speed_accepts_immediate_and_multipliers() {
        assert_eq!("immediate".parse::<Speed>(), Ok(Speed::Immediate));
        assert_eq!("60".parse::<Speed>(), Ok(Speed::Accelerated { factor: 60 }));
        assert_eq!("1x".parse::<Speed>(), Ok(Speed::Accelerated { factor: 1 }));
    }

    #[test]
    fn a_speed_that_would_never_finish_is_refused() {
        assert!("0".parse::<Speed>().is_err());
        assert!("quickly".parse::<Speed>().is_err());
    }

    /// Every command, parsed with its flags actually read back.
    ///
    /// `Cli::command().debug_assert()` does **not** catch an argument id collision between a
    /// global and a subcommand field — clap resolves ids lazily, so the mismatch surfaces as
    /// a downcast panic the first time the value is accessed, at runtime, in front of an
    /// operator. `export crossings --as csv` shipped exactly that bug: the field was named
    /// `format`, which collided with the global `--format`, and every other test passed.
    ///
    /// So this walks the tree and *uses* the parse rather than only building it.
    #[test]
    fn every_command_parses_and_its_arguments_can_be_read_back() {
        const INVOCATIONS: &[&[&str]] = &[
            &["splitforge", "init"],
            &["splitforge", "event", "create", "--name", "Day"],
            &["splitforge", "event", "list"],
            &["splitforge", "race", "create", "--name", "5K"],
            &["splitforge", "race", "list"],
            &["splitforge", "race", "start"],
            &["splitforge", "race", "stop", "--note", "done"],
            &[
                "splitforge",
                "checkpoint",
                "add",
                "--name",
                "finish",
                "--kind",
                "finish",
            ],
            &["splitforge", "checkpoint", "list"],
            &["splitforge", "roster", "import", "roster.csv"],
            &["splitforge", "roster", "list"],
            &["splitforge", "chips", "import", "chips.csv"],
            &["splitforge", "chips", "list"],
            &["splitforge", "reader", "add", "--id", "mat"],
            &[
                "splitforge",
                "reader",
                "map",
                "--reader",
                "mat",
                "--checkpoint",
                "finish",
            ],
            &["splitforge", "reader", "list"],
            &["splitforge", "reader", "status"],
            &["splitforge", "policy", "set", "--min-interval-ms", "3000"],
            &["splitforge", "policy", "show"],
            &["splitforge", "fixture", "load"],
            &["splitforge", "fixture", "list"],
            &["splitforge", "simulate", "--scenario", "five-k"],
            &["splitforge", "reads", "--follow"],
            &["splitforge", "accepted"],
            &["splitforge", "derive"],
            &["splitforge", "export", "crossings", "--as", "csv"],
            &["splitforge", "export", "crossings", "--as", "json"],
            &["splitforge", "export", "results", "--as", "csv"],
            &["splitforge", "export", "results", "--as", "json"],
            &["splitforge", "export", "results", "--revision", "2"],
            &["splitforge", "policy", "set", "--start-mode", "chip"],
            &["splitforge", "results", "preview"],
            &[
                "splitforge",
                "results",
                "publish",
                "--status",
                "provisional",
                "--reason",
                "initial",
            ],
            &[
                "splitforge",
                "results",
                "publish",
                "--status",
                "final",
                "--reason",
                "after appeals",
                "--allow-unchanged",
            ],
            &["splitforge", "results", "show"],
            &["splitforge", "results", "show", "--revision", "1"],
            &["splitforge", "results", "list"],
            &["splitforge", "results", "diff", "--from", "1", "--to", "2"],
            &[
                "splitforge",
                "results",
                "declare",
                "--bib",
                "217",
                "--status",
                "dq",
                "--reason",
                "course cut",
            ],
            &["splitforge", "results", "declarations"],
            &["splitforge", "backup", "create", "snap.db"],
            &["splitforge", "doctor"],
            &["splitforge", "audit"],
            &["splitforge", "status"],
        ];

        for invocation in INVOCATIONS {
            let cli = Cli::try_parse_from(*invocation)
                .unwrap_or_else(|error| panic!("{invocation:?} did not parse: {error}"));
            // Touch the globals and the subcommand together: the downcast happens on access.
            assert!(!cli.actor.is_empty(), "{invocation:?}");
            assert!(!cli.database.as_os_str().is_empty(), "{invocation:?}");
            let rendered = format!("{:?}", cli.command);
            assert!(!rendered.is_empty(), "{invocation:?}");
        }
    }

    #[test]
    fn the_export_shape_and_the_global_format_are_independent() {
        let cli = Cli::try_parse_from([
            "splitforge",
            "--format",
            "compact",
            "export",
            "crossings",
            "--as",
            "json",
        ])
        .expect("parse");
        assert_eq!(cli.format, Format::Compact);
        let Command::Export(ExportCommand::Crossings { shape, .. }) = cli.command else {
            panic!("expected an export command");
        };
        assert_eq!(shape, ExportFormat::Json);
    }

    #[test]
    fn the_two_status_arguments_mean_different_things_and_do_not_collide() {
        // `results publish --status` takes provisional/final; `results declare --status`
        // takes a result status. Distinct subcommands, so distinct argument ids — but the
        // last time two arguments shared an id it panicked at parse time rather than build
        // time, so this is asserted rather than assumed.
        let publish = Cli::try_parse_from([
            "splitforge",
            "results",
            "publish",
            "--status",
            "final",
            "--reason",
            "after appeals",
        ])
        .expect("publish parses");
        let Command::Results(ResultsCommand::Publish { status, reason, .. }) = publish.command
        else {
            panic!("expected a publish command");
        };
        assert_eq!(status, "final");
        assert_eq!(reason, "after appeals");

        let declare = Cli::try_parse_from([
            "splitforge",
            "results",
            "declare",
            "--bib",
            "217",
            "--status",
            "dq",
            "--reason",
            "course cut",
        ])
        .expect("declare parses");
        let Command::Results(ResultsCommand::Declare { bib, status, .. }) = declare.command else {
            panic!("expected a declare command");
        };
        assert_eq!(bib, "217");
        assert_eq!(status, "dq");
    }

    #[test]
    fn publishing_without_a_reason_is_refused_by_the_parser() {
        // Not a runtime check that can be forgotten: a revision nobody can explain should
        // not be reachable from the command line at all.
        assert!(Cli::try_parse_from(["splitforge", "results", "publish"]).is_err());
        assert!(
            Cli::try_parse_from([
                "splitforge",
                "results",
                "declare",
                "--bib",
                "1",
                "--status",
                "dq"
            ])
            .is_err()
        );
    }

    #[test]
    fn the_database_flag_is_accepted_on_either_side_of_the_subcommand() {
        // `global = true` makes both work. An operator who has typed `--database` first for
        // twenty commands should not have it stop working on the twenty-first.
        let before = Cli::try_parse_from(["splitforge", "--database", "a.db", "init"])
            .expect("before the subcommand");
        let after = Cli::try_parse_from(["splitforge", "init", "--database", "a.db"])
            .expect("after the subcommand");
        assert_eq!(before.database, after.database);
    }
}
