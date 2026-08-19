//! Discontinuities in the device's wall clock.
//!
//! The Pi 3 has no battery-backed clock ([ADR-0002](../../../docs/adr/0002-raspberry-pi-target.md)),
//! so it boots believing whatever it believed last, and then something corrects it — NTP
//! when a network appears, an RTC module, an operator with `date`. That correction is a
//! **step**: the wall clock jumps, while the monotonic clock does not.
//!
//! A step matters because
//! [clock discipline § 4](../../../docs/clock-and-time-discipline.md#4-where-clock-error-actually-matters)
//! puts SplitForge's whole error budget at ±0.1 s across an event day, and because
//! [`race start` records the gun](../../../docs/adr/0015-race-start-records-the-gun.md) from
//! this clock. A one-second jump between the gun and the first finish is ten times the
//! entire budget, applied to every gun time in the race, and it leaves no other trace.
//!
//! Detection needs no hardware and no network — only the two clocks the operating system
//! already provides, compared against each other.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// How far the two clocks must disagree before it counts as a step.
///
/// Not a precision target — a floor for *confidence*. Three things move the two clocks
/// apart without anything being wrong:
///
/// - **NTP slew.** A disciplined clock is corrected gradually, at up to 500 ppm, which over
///   a [`SAMPLE_INTERVAL_MS`] window is a few milliseconds.
/// - **Scheduling.** The two clocks are read one after the other, and a loaded Pi can
///   deschedule the thread in between.
/// - **Timer drift.** The sampling loop does not wake at exactly the interval it asked for.
///
/// 250 ms clears all three with room to spare. It is deliberately *larger* than the ±0.1 s
/// accuracy budget, which means a sub-budget step goes unreported — the alternative is an
/// alarm that fires on healthy devices, and an alarm that fires on healthy devices is an
/// alarm operators learn to ignore.
pub const STEP_THRESHOLD_MS: i64 = 250;

/// How often the two clocks are compared.
///
/// Ten seconds bounds both how long a step can go unnoticed and how precisely
/// [`ClockStep::observed_before`] and [`ClockStep::observed_after`] bracket it. Shorter
/// would narrow the bracket at the cost of waking a battery-powered device more often, for
/// an event where ten seconds of ambiguity about *when* the clock jumped changes nothing
/// about the fact that it did.
pub const SAMPLE_INTERVAL_MS: u64 = 10_000;

/// One observed discontinuity between the wall clock and the monotonic clock.
///
/// Evidence, not configuration: it records what the device observed, and is never edited
/// afterwards ([ADR-0005](../../../docs/adr/0005-raw-read-append-only-journal.md)).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClockStep {
    /// Wall-clock reading at the sample before the step.
    #[serde(with = "time::serde::rfc3339")]
    pub observed_before: OffsetDateTime,
    /// Wall-clock reading at the sample that detected it.
    #[serde(with = "time::serde::rfc3339")]
    pub observed_after: OffsetDateTime,
    /// Monotonic milliseconds actually elapsed between the two samples.
    pub monotonic_ms: u64,
    /// Wall-clock movement minus monotonic movement, in milliseconds.
    ///
    /// Positive means the clock jumped forward, negative means backward. Backward is the
    /// dangerous direction: it can make a later read carry an earlier timestamp than one
    /// recorded before it, which no amount of sequence numbering repairs after the fact.
    pub step_ms: i64,
}

impl ClockStep {
    /// Compares one pair of clock samples, returning a step only when the two disagree by
    /// more than [`STEP_THRESHOLD_MS`].
    ///
    /// `monotonic_ms` is the elapsed monotonic time between the two wall-clock readings.
    #[must_use]
    pub fn detect(
        observed_before: OffsetDateTime,
        observed_after: OffsetDateTime,
        monotonic_ms: u64,
    ) -> Option<Self> {
        let wall_ms = (observed_after - observed_before).whole_milliseconds();

        // Saturating rather than `as`: a wall clock set to a value decades away produces a
        // difference that does not fit an i64 of milliseconds, and a wrapped number would
        // report a tiny step for the largest possible jump.
        let wall_ms = i64::try_from(wall_ms).unwrap_or(i64::MAX);
        let monotonic = i64::try_from(monotonic_ms).unwrap_or(i64::MAX);
        let step_ms = wall_ms.saturating_sub(monotonic);

        (step_ms.saturating_abs() > STEP_THRESHOLD_MS).then_some(Self {
            observed_before,
            observed_after,
            monotonic_ms,
            step_ms,
        })
    }

    /// Whether the clock moved backwards relative to elapsed time.
    #[must_use]
    pub const fn is_backward(&self) -> bool {
        self.step_ms < 0
    }

    /// How far the clocks disagreed, without the direction.
    #[must_use]
    pub const fn magnitude_ms(&self) -> u64 {
        self.step_ms.unsigned_abs()
    }
}

/// A clock step as it exists in the journal, with the fields storage assigns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredClockStep {
    /// Monotonic insertion sequence, independent of any clock — which is the point, given
    /// what this table records.
    pub seq: u64,
    /// When the row was inserted, by the wall clock as it stood after the step.
    #[serde(with = "time::serde::rfc3339")]
    pub recorded_at: OffsetDateTime,
    /// The observation itself.
    #[serde(flatten)]
    pub step: ClockStep,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: i64) -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(1_775_000_000 + seconds).expect("a valid timestamp")
    }

    #[test]
    fn a_healthy_clock_produces_no_step() {
        // Ten seconds of monotonic time, ten seconds of wall time.
        assert_eq!(ClockStep::detect(at(0), at(10), 10_000), None);
    }

    #[test]
    fn ntp_slew_is_not_a_step() {
        // 200 ms of divergence over the sample window: larger than any real slew, still
        // under the threshold, and reporting it would train operators to ignore the alarm.
        let before = at(0);
        let after = before + time::Duration::milliseconds(10_200);
        assert_eq!(ClockStep::detect(before, after, 10_000), None);
    }

    #[test]
    fn a_forward_jump_is_reported_with_its_size() {
        // The classic: no RTC, so the Pi boots at the wrong time and NTP corrects it the
        // moment a network appears.
        let before = at(0);
        let after = before + time::Duration::seconds(3610);
        let step = ClockStep::detect(before, after, 10_000).expect("an hour is a step");

        assert_eq!(step.step_ms, 3_600_000);
        assert!(!step.is_backward());
        assert_eq!(step.magnitude_ms(), 3_600_000);
    }

    #[test]
    fn a_backward_jump_is_reported_as_backward() {
        let before = at(3600);
        let after = before - time::Duration::seconds(30);
        let step = ClockStep::detect(before, after, 10_000).expect("a jump back is a step");

        assert!(step.is_backward());
        assert_eq!(
            step.step_ms, -40_000,
            "30 s back, plus 10 s that did elapse"
        );
        assert_eq!(step.magnitude_ms(), 40_000);
    }

    #[test]
    fn the_threshold_is_exclusive_at_the_boundary() {
        let before = at(0);
        let exactly = before + time::Duration::milliseconds(10_000 + STEP_THRESHOLD_MS);
        assert_eq!(
            ClockStep::detect(before, exactly, 10_000),
            None,
            "at the threshold is not over it"
        );

        let over = before + time::Duration::milliseconds(10_001 + STEP_THRESHOLD_MS);
        assert!(ClockStep::detect(before, over, 10_000).is_some());
    }

    #[test]
    fn an_absurd_jump_does_not_wrap_into_a_small_one() {
        // A clock set to the year 3000 must not be reported as a 12 ms step, which is what
        // an unchecked conversion would produce.
        let before = at(0);
        let after = OffsetDateTime::from_unix_timestamp(32_503_680_000).expect("year 3000");
        let step = ClockStep::detect(before, after, 10_000).expect("a step");

        assert!(
            step.step_ms > 1_000_000_000,
            "a thousand-year jump is not a small number: {}",
            step.step_ms
        );
        assert!(!step.is_backward());
    }

    #[test]
    fn a_step_round_trips_through_json() {
        let step = ClockStep::detect(at(0), at(3610), 10_000).expect("a step");
        let json = serde_json::to_string(&step).expect("serialize");
        let back: ClockStep = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(step, back);
    }
}
