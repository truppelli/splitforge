//! Command-line surface.
//!
//! Milestone 1 only. The full operator vocabulary — `event create`, `roster import`,
//! `reader add`, `export results` — arrives in Milestone 2, once there is somewhere to put
//! an event other than a fixture. See `docs/roadmap.md`.

use std::path::PathBuf;
use std::str::FromStr;

use clap::{Parser, Subcommand, ValueEnum};

/// SplitForge: offline-first race timing.
#[derive(Debug, Parser)]
#[command(name = "splitforge", version, about, long_about = None)]
pub struct Cli {
    /// How to format the JSON on stdout.
    #[arg(long, value_enum, default_value_t = Format::Pretty, global = true)]
    pub format: Format,

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

/// The Milestone 1 commands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create or migrate an event database.
    Init {
        /// Path to the event database.
        #[arg(long, value_name = "PATH")]
        database: PathBuf,
    },

    /// Run a simulated event, appending every read to the journal.
    Simulate {
        /// Path to the event database. Created if it does not exist.
        #[arg(long, value_name = "PATH")]
        database: PathBuf,

        /// Which fixture event to run.
        #[arg(long, value_name = "SLUG", default_value = "five-k")]
        fixture: String,

        /// Seed for the simulator's random choices. The same seed replays the same race.
        #[arg(long, default_value_t = 0x5F17_F03E)]
        seed: u64,

        /// `immediate`, or a speed multiplier such as `1` for wall-clock time.
        #[arg(long, default_value = "immediate")]
        speed: Speed,
    },

    /// List raw reads: the evidence, exactly as recorded.
    Reads {
        /// Path to the event database.
        #[arg(long, value_name = "PATH")]
        database: PathBuf,

        /// Only reads after this journal sequence number.
        #[arg(long, value_name = "SEQ")]
        since: Option<u64>,

        /// Stop after this many reads.
        #[arg(long, value_name = "N")]
        limit: Option<usize>,
    },

    /// List accepted reads: one per crossing, after deduplication.
    Accepted {
        /// Path to the event database.
        #[arg(long, value_name = "PATH")]
        database: PathBuf,

        /// Which fixture supplies the roster, checkpoints, and policy.
        #[arg(long, value_name = "SLUG", default_value = "five-k")]
        fixture: String,
    },

    /// Re-derive everything from the journal: crossings, suppressions, and timing events.
    Derive {
        /// Path to the event database.
        #[arg(long, value_name = "PATH")]
        database: PathBuf,

        /// Which fixture supplies the roster, checkpoints, and policy.
        #[arg(long, value_name = "SLUG", default_value = "five-k")]
        fixture: String,
    },

    /// Summarize the journal without deriving anything from it.
    Status {
        /// Path to the event database.
        #[arg(long, value_name = "PATH")]
        database: PathBuf,

        /// Which fixture this database was populated from, for the record.
        #[arg(long, value_name = "SLUG")]
        fixture: Option<String>,
    },
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
}
