//! Timing policy: the configurable rules that turn reads into crossings.
//!
//! Policy is data, not code paths. It is snapshotted into each result revision so that
//! "which dedup window produced this result?" stays answerable after someone edits the
//! configuration — see `docs/timing-model.md`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::ids::CheckpointId;

/// Which read within a burst becomes the crossing.
///
/// The right answer depends on physics, so it is configurable per checkpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "rule", rename_all = "snake_case")]
pub enum SelectionRule {
    /// Earliest read in the burst. The default, and the conventional choice for a finish
    /// line: credit the first credible detection.
    First,
    /// Earliest read at or above an RSSI floor.
    ///
    /// For antennas that reach beyond the mat, where [`SelectionRule::First`] would credit
    /// a runner who is still approaching. Needs on-site calibration.
    FirstAboveRssi {
        /// Minimum acceptable signal strength in dBm.
        floor_dbm: i16,
    },
    /// Strongest read in the burst, for when closest physical approach matters more than
    /// earliest detection.
    PeakRssi,
}

/// Per-checkpoint overrides. Any field left `None` falls back to the race-wide policy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointPolicy {
    /// Overrides [`TimingPolicy::min_interval_ms`].
    pub min_interval_ms: Option<u64>,
    /// Overrides [`TimingPolicy::selection_rule`].
    pub selection_rule: Option<SelectionRule>,
}

/// The complete set of rules used to derive crossings from reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimingPolicy {
    /// Reads of the same chip at the same checkpoint closer together than this belong to
    /// one crossing.
    pub min_interval_ms: u64,
    /// Which read in a burst becomes the crossing.
    pub selection_rule: SelectionRule,
    /// A lap credited faster than this is treated as a re-read, not a superhuman lap.
    pub min_lap_ms: Option<u64>,
    /// Per-checkpoint overrides.
    pub per_checkpoint: BTreeMap<CheckpointId, CheckpointPolicy>,
}

/// Default dedup window: 3 s.
///
/// A runner takes roughly a second to clear a mat, and a genuine second crossing of the
/// same checkpoint within three seconds is not physically plausible on any course this
/// milestone supports. Deliberately conservative — merging two real crossings loses a
/// result, while splitting one crossing produces a visible duplicate that an operator
/// can spot.
pub const DEFAULT_MIN_INTERVAL_MS: u64 = 3_000;

impl Default for TimingPolicy {
    fn default() -> Self {
        Self {
            min_interval_ms: DEFAULT_MIN_INTERVAL_MS,
            selection_rule: SelectionRule::First,
            min_lap_ms: None,
            per_checkpoint: BTreeMap::new(),
        }
    }
}

impl TimingPolicy {
    /// The dedup window in force at a checkpoint.
    #[must_use]
    pub fn min_interval_ms_for(&self, checkpoint: CheckpointId) -> u64 {
        self.per_checkpoint
            .get(&checkpoint)
            .and_then(|policy| policy.min_interval_ms)
            .unwrap_or(self.min_interval_ms)
    }

    /// The selection rule in force at a checkpoint.
    #[must_use]
    pub fn selection_rule_for(&self, checkpoint: CheckpointId) -> SelectionRule {
        self.per_checkpoint
            .get(&checkpoint)
            .and_then(|policy| policy.selection_rule)
            .unwrap_or(self.selection_rule)
    }

    /// Sets a per-checkpoint override, returning `self` for chaining.
    #[must_use]
    pub fn with_checkpoint(mut self, checkpoint: CheckpointId, policy: CheckpointPolicy) -> Self {
        self.per_checkpoint.insert(checkpoint, policy);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkpoint_overrides_beat_the_race_wide_default() {
        let checkpoint = CheckpointId::new();
        let other = CheckpointId::new();
        let policy = TimingPolicy::default().with_checkpoint(
            checkpoint,
            CheckpointPolicy {
                min_interval_ms: Some(9_000),
                selection_rule: Some(SelectionRule::PeakRssi),
            },
        );

        assert_eq!(policy.min_interval_ms_for(checkpoint), 9_000);
        assert_eq!(
            policy.selection_rule_for(checkpoint),
            SelectionRule::PeakRssi
        );
        assert_eq!(
            policy.min_interval_ms_for(other),
            DEFAULT_MIN_INTERVAL_MS,
            "an unconfigured checkpoint uses the race-wide default"
        );
        assert_eq!(policy.selection_rule_for(other), SelectionRule::First);
    }

    #[test]
    fn a_partial_override_only_replaces_the_field_it_sets() {
        let checkpoint = CheckpointId::new();
        let policy = TimingPolicy::default().with_checkpoint(
            checkpoint,
            CheckpointPolicy {
                min_interval_ms: Some(500),
                selection_rule: None,
            },
        );
        assert_eq!(policy.min_interval_ms_for(checkpoint), 500);
        assert_eq!(policy.selection_rule_for(checkpoint), SelectionRule::First);
    }

    #[test]
    fn policy_round_trips_through_json_so_revisions_can_snapshot_it() {
        let policy = TimingPolicy::default().with_checkpoint(
            CheckpointId::new(),
            CheckpointPolicy {
                min_interval_ms: Some(1_500),
                selection_rule: Some(SelectionRule::FirstAboveRssi { floor_dbm: -60 }),
            },
        );
        let json = serde_json::to_string(&policy).expect("serialize");
        let back: TimingPolicy = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(policy, back);
    }
}
