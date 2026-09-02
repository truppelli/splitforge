//! Stretches where a reader was not producing, and how that was noticed.
//!
//! A timer that records nothing and a timer that records nothing *while a reader is
//! unplugged* look identical in the journal — an absence of reads. The difference is the
//! whole point of this module:
//! [ADR-0025](../../../docs/adr/0025-m3a-proves-durability-above-the-transport.md) makes
//! "every disconnection is detected and recorded as a bounded gap in the evidence" a clause
//! of Milestone 3a's exit criterion, because the ThingMagic module cannot announce its own
//! failure — user guide § 8.8.2 says it cannot *"detect a broken communications interface
//! connection and stop streaming the tag results"*, and § 5.1.4.1 says the interface has no
//! flow control. Nothing tells SplitForge the reader is gone, so SplitForge has to notice.
//!
//! # Why a gap is two rows
//!
//! [ADR-0026](../../../docs/adr/0026-a-reader-gap-is-two-rows.md). A gap is the only evidence
//! in this project whose end arrives later than its beginning and is unknown when the
//! beginning is recorded — and evidence is append-only at the database
//! ([ADR-0011](../../../docs/adr/0011-append-only-enforced-by-triggers.md)), so the end
//! cannot be written into the row that opened it.
//!
//! So the table holds one row per [`GapEdge`], and [`ReaderGap`] is the pair, derived rather
//! than stored. An opened edge with no closed edge **is** the open gap: no flag, no in-memory
//! state, and no shutdown hook, which is what makes an open gap survive the power cut that
//! is the single most likely cause of one.
//!
//! # Confirmed and suspected are different facts
//!
//! A device node that vanishes is unambiguous. A stream that has gone quiet is not — a
//! checkpoint with nobody crossing it is also quiet, and the tail of a 10K is quiet for a
//! long time. The ambiguity survives into the evidence rather than being resolved by a
//! confident guess, which is the same distinction the project already draws between
//! *"not measured"* and *"measured, and the clock is bad"* in the clock-source report, and
//! between a device that never had a reader and one whose reader is merely stopped.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::ids::ReaderId;

/// How long a stream may be quiet before it is presumed gone, in milliseconds.
///
/// **This number has no measurement behind it and does not pretend to.**
/// [Q14](../../../docs/open-questions.md#q14-reader-silence-threshold) asks what it should be
/// and is open: answering it needs to know whether the module emits anything at all into an
/// empty field, which needs either a current SDK header or a module on a bench.
///
/// Two minutes is chosen to be wrong in the safe direction. Too short manufactures gaps in
/// evidence that is perfectly intact, and a false gap on a quiet finish line is a report an
/// operator learns to ignore; too long lets a module that died at the gun go unnoticed. A
/// gap is a warning and blocks nothing, so the cost of being late is bounded, while the cost
/// of crying wolf is that the whole signal stops being read.
pub const DEFAULT_SILENCE_THRESHOLD_MS: u64 = 120_000;

/// How a gap came to be noticed.
///
/// Recorded on the opening edge and never revised. ADR-0025: a silence-derived gap *"is
/// recorded as suspected, never as confirmed"* — enforced here by there being no value that
/// could say otherwise and no path that rewrites it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GapDetection {
    /// The transport said so: the device node went away, a read failed, or the port could
    /// not be reopened. Unambiguous, and the disconnection an observer actually induces.
    Confirmed,
    /// The stream went quiet for longer than the configured interval.
    ///
    /// **Ambiguous by construction.** This is indistinguishable from a checkpoint nobody is
    /// crossing, and it is reported as a suspicion for exactly that reason.
    Suspected,
}

impl GapDetection {
    /// The token stored in the database and printed in reports.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Confirmed => "confirmed",
            Self::Suspected => "suspected",
        }
    }

    /// Parses a stored token.
    ///
    /// Returns `None` rather than guessing: a value this does not recognize came from a
    /// newer schema or a corrupted row, and both deserve to be noticed.
    #[must_use]
    pub fn parse(token: &str) -> Option<Self> {
        match token {
            "confirmed" => Some(Self::Confirmed),
            "suspected" => Some(Self::Suspected),
            _ => None,
        }
    }

    /// Whether the transport itself reported the failure.
    #[must_use]
    pub const fn is_confirmed(self) -> bool {
        matches!(self, Self::Confirmed)
    }
}

/// Which end of a gap an event records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GapEdge {
    /// The reader stopped producing.
    Opened,
    /// It started again.
    Closed,
}

impl GapEdge {
    /// The token stored in the database.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Opened => "opened",
            Self::Closed => "closed",
        }
    }

    /// Parses a stored token, returning `None` for anything unrecognized.
    #[must_use]
    pub fn parse(token: &str) -> Option<Self> {
        match token {
            "opened" => Some(Self::Opened),
            "closed" => Some(Self::Closed),
            _ => None,
        }
    }
}

/// What the silence watchdog should do on one tick.
///
/// A pure decision, separated from the I/O that carries it out, because the interesting cases
/// are all about *when* rather than about databases: a disabled threshold, a race that is not
/// running, and the boundary itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SilenceVerdict {
    /// Do nothing. Either the check is off, or no race is running, so silence means nothing.
    Idle,
    /// Open a [`GapDetection::Suspected`] gap, or leave the one already open alone.
    Open,
    /// Close whatever gap is open: reads are arriving, so the reader is back.
    Close,
}

/// Decides what a silence watchdog tick should do.
///
/// `threshold_ms` of zero **disables the check**, which is the one value that means something
/// other than a duration. An operator who has decided a false gap is worse than a late one
/// needs a way to say so, and saying it explicitly beats setting a week and hoping.
///
/// A race that is not running yields [`SilenceVerdict::Idle`] rather than
/// [`SilenceVerdict::Close`]: a device on a bench overnight is silent for twelve hours and
/// that is not a fault, and stopping a race is not evidence that a reader came back.
#[must_use]
pub const fn assess_silence(
    threshold_ms: u64,
    race_running: bool,
    silent_for_ms: u64,
) -> SilenceVerdict {
    if threshold_ms == 0 || !race_running {
        return SilenceVerdict::Idle;
    }
    if silent_for_ms >= threshold_ms {
        SilenceVerdict::Open
    } else {
        SilenceVerdict::Close
    }
}

/// A gap, assembled from the edges that bound it.
///
/// **Derived, never stored.** Nothing writes one of these; storage pairs the rows and hands
/// this back, so a caller cannot forget the join and see edges where it meant to see gaps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReaderGap {
    /// The `seq` of the opening edge, and this gap's identity.
    pub opened_seq: u64,
    /// The `seq` of the closing edge, or `None` while the gap is still open.
    pub closed_seq: Option<u64>,
    /// Which reader was not producing.
    pub reader_id: ReaderId,
    /// How it was noticed. Never revised — see [`GapDetection`].
    pub detection: GapDetection,
    /// Wall clock when the reader stopped producing.
    #[serde(with = "time::serde::rfc3339")]
    pub started_at: OffsetDateTime,
    /// Wall clock when it resumed, or `None` while open.
    #[serde(with = "time::serde::rfc3339::option")]
    pub ended_at: Option<OffsetDateTime>,
    /// Monotonic reading at the opening edge, on the clock of the process that recorded it.
    pub started_monotonic_ms: u64,
    /// Monotonic reading at the closing edge, or `None` while open.
    ///
    /// Not comparable with [`Self::started_monotonic_ms`] unless one process recorded both —
    /// see [`Self::duration_ms`].
    pub ended_monotonic_ms: Option<u64>,
    /// Whatever the opening edge could say about the cause.
    pub detail: Option<String>,
}

impl ReaderGap {
    /// Whether this gap has no end yet.
    ///
    /// An open gap is a reportable state rather than an omission (ADR-0025): it is what a
    /// device says about itself when the reader is unplugged *right now*, and it is what
    /// survives a power cut, because the opening row was on disk before the power went.
    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.closed_seq.is_none()
    }

    /// How long the gap lasted, in milliseconds, or `None` while it is open.
    ///
    /// **From the wall clock, which is the only pair of readings guaranteed to be
    /// comparable.** The monotonic readings beside them are absolute values taken from
    /// whichever process recorded each edge, and a gap that opens before a restart and closes
    /// after one has two of them from different origins — on Linux, different boots. A
    /// subtraction across that pair would produce a confident number that means nothing.
    ///
    /// The cost is named rather than hidden: a wall clock that steps during a gap distorts
    /// this. That is disclosed rather than undetectable — `clock_steps` is in the same
    /// journal, and a step overlapping a gap is exactly the correlation somebody disputing a
    /// result should be looking for.
    #[must_use]
    pub fn duration_ms(&self) -> Option<u64> {
        let ended = self.ended_at?;
        let elapsed = (ended - self.started_at).whole_milliseconds();
        Some(u64::try_from(elapsed).unwrap_or(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: i64) -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(1_775_000_000 + seconds).expect("a valid timestamp")
    }

    fn gap(closed: Option<(u64, u64)>) -> ReaderGap {
        ReaderGap {
            opened_seq: 1,
            closed_seq: closed.map(|(seq, _)| seq),
            reader_id: ReaderId::new("mat"),
            detection: GapDetection::Confirmed,
            started_at: at(0),
            ended_at: closed.map(|_| at(30)),
            started_monotonic_ms: 1_000,
            ended_monotonic_ms: closed.map(|(_, monotonic)| monotonic),
            detail: None,
        }
    }

    #[test]
    fn an_unpaired_gap_is_open_and_has_no_duration() {
        let open = gap(None);
        assert!(open.is_open());
        assert_eq!(open.duration_ms(), None);
    }

    #[test]
    fn a_paired_gap_is_closed_and_measures_between_its_two_wall_clock_readings() {
        let closed = gap(Some((2, 31_000)));
        assert!(!closed.is_open());
        assert_eq!(closed.duration_ms(), Some(30_000));
    }

    #[test]
    fn a_gap_that_appears_to_end_before_it_started_reports_zero_not_an_epoch() {
        // Reachable: the wall clock steps backwards mid-gap, which is the direction clock
        // discipline calls the dangerous one. A duration that underflowed to 584 million
        // years would be reported as fact, and this table exists to be trusted when somebody
        // disputes a result.
        let mut backwards = gap(Some((2, 31_000)));
        backwards.ended_at = Some(at(-30));
        assert_eq!(backwards.duration_ms(), Some(0));
    }

    #[test]
    fn detection_tokens_round_trip_and_refuse_anything_else() {
        for detection in [GapDetection::Confirmed, GapDetection::Suspected] {
            assert_eq!(GapDetection::parse(detection.as_str()), Some(detection));
        }
        assert_eq!(GapDetection::parse("probable"), None);
        assert_eq!(GapDetection::parse("Confirmed"), None);
    }

    #[test]
    fn edge_tokens_round_trip_and_refuse_anything_else() {
        for edge in [GapEdge::Opened, GapEdge::Closed] {
            assert_eq!(GapEdge::parse(edge.as_str()), Some(edge));
        }
        assert_eq!(GapEdge::parse("open"), None);
    }

    #[test]
    fn only_a_transport_failure_is_confirmed() {
        assert!(GapDetection::Confirmed.is_confirmed());
        assert!(!GapDetection::Suspected.is_confirmed());
    }

    #[test]
    fn a_zero_threshold_disables_the_watchdog_entirely() {
        // The one value that means something other than a duration, and it must win over
        // every other input — including a race that is running and a reader silent for a day.
        assert_eq!(assess_silence(0, true, u64::MAX), SilenceVerdict::Idle);
    }

    #[test]
    fn silence_means_nothing_while_no_race_is_running() {
        // A device on a bench overnight is silent for twelve hours. Recording that as a fault
        // would fill the table with gaps nobody can act on.
        assert_eq!(assess_silence(1_000, false, u64::MAX), SilenceVerdict::Idle);
    }

    #[test]
    fn a_race_that_is_not_running_does_not_close_an_open_gap_either() {
        // Idle rather than Close: stopping a race is not evidence that a reader came back,
        // and a gap that was open when the gun stopped keeps its start.
        assert_ne!(assess_silence(1_000, false, 0), SilenceVerdict::Close);
    }

    #[test]
    fn the_threshold_is_the_boundary_and_it_is_inclusive() {
        assert_eq!(assess_silence(1_000, true, 999), SilenceVerdict::Close);
        assert_eq!(assess_silence(1_000, true, 1_000), SilenceVerdict::Open);
        assert_eq!(assess_silence(1_000, true, 1_001), SilenceVerdict::Open);
    }

    #[test]
    fn reads_arriving_close_whatever_is_open() {
        // Including a confirmed gap: a reader that is delivering is a reader that came back,
        // however the outage was first noticed.
        assert_eq!(assess_silence(120_000, true, 0), SilenceVerdict::Close);
    }
}
