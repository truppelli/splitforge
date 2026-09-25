//! What the operator says about the course, because SplitForge does not otherwise know it.
//!
//! SplitForge times a race without knowing how long it is: a checkpoint has a role and a
//! sequence, not a position, and scoring never needs one. The public race page does — pace,
//! the progress rail, and the order of mats on the live board are all measured in metres.
//! So publishing needs a little geometry the timer never asked for, and it is the one
//! thing the operator types for RaceDay Connect alone.
//!
//! The start and the finish are placed without asking: the start at zero, the finish (and
//! a lap mat) at one lap's length. Only an intermediate split needs a number.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use splitforge_domain::RaceId;

/// One SplitForge race, as a RaceDay Connect distance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Course {
    /// The SplitForge race.
    pub race: RaceId,
    /// The distance key RaceDay Connect files it under.
    ///
    /// Chosen once and never changed: revisions are numbered per key, so a new key is a
    /// new distance with its revision history starting again at 1.
    pub key: String,
    /// Certified distance in metres, all laps included.
    pub meters: f64,
    /// Where each intermediate split sits along one lap, in metres, by checkpoint name.
    #[serde(default)]
    pub splits: BTreeMap<String, f64>,
}
