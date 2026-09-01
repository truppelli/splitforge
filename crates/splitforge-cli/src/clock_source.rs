//! What `doctor` says about the device's time source.
//!
//! **The asking moved out.** Running `chronyc` and turning its output into a
//! [`ClockReading`] is [`splitforge_timesource`]; what remains here is the operator-facing
//! half — the findings `doctor` raises and the view it prints.
//!
//! The split happened when `splitforge-edge` needed the same answer for a different purpose:
//! `doctor` reports the clock once before a race, and the service stamps a
//! `DeviceClockState` on **every read it writes**. Two callers on opposite sides of the
//! workspace, one of them writing permanent evidence, is what turned a module into a crate.
//!
//! ## Why this half stayed
//!
//! [`findings_for`] returns [`Finding`], which is `doctor`'s type, and [`ClockSourceView`] is
//! what `doctor` and the diagnostic bundle serialize. Neither is meaningful to a service that
//! never prints anything — the service reads a `DeviceClockState` and reports its own shape
//! on `/health`.

use serde::Serialize;
use splitforge_domain::DeviceClockState;
// Re-exported rather than imported at each call site: `clock_source::read_tracking()` reads
// correctly where it is called, and the move to another crate is not something `doctor` has
// an opinion about.
pub(crate) use splitforge_timesource::{ClockReading, read_tracking};

use crate::report::Finding;

/// What `doctor` should say about a reading, if anything.
///
/// Pure, and separate from `doctor` itself, because the branch that matters most — an
/// unsynchronized clock before a race — is otherwise reachable only on a machine whose
/// daemon happens to be in that state. A check nobody can test is a check nobody can
/// change safely.
///
/// **Warns and blocks nothing.** Which clock states should refuse a `race start` is
/// [Q11](../../../docs/open-questions.md#q11-clock-error-budget-enforcement), which has no
/// answer; picking one here would be answering it silently.
pub(crate) fn findings_for(reading: &ClockReading) -> Vec<Finding> {
    match reading {
        // A measurement saying the clock is bad. The reads have not happened yet, which is
        // what makes this worth saying here rather than after the race.
        ClockReading::Tracking(tracking) if !tracking.device_clock_state().is_trustworthy() => {
            vec![Finding::warning(
                CHECK,
                "the device clock is not synchronized to any time source. Gun and finish \
                 times will still be recorded from it, and their accuracy is whatever the \
                 clock happens to be — see docs/clock-and-time-discipline.md.",
            )]
        }
        // A daemon is installed and did not answer usefully. Different from having no
        // daemon: something is there and it is not working.
        ClockReading::DaemonUnreachable { .. } | ClockReading::Unreadable { .. } => {
            vec![Finding::warning(
                CHECK,
                "a time daemon is installed but gave no usable answer, so this device's \
                 clock state is unknown going into the race.",
            )]
        }
        // A trustworthy source, or no daemon at all. The first is fine; the second is a
        // laptop as often as it is a misconfigured Pi, and `doctor`'s clock_source field
        // reports which happened without spending an alarm on it.
        ClockReading::Tracking(_) | ClockReading::NoDaemonTool => Vec::new(),
    }
}

/// The check name, used in findings and matched by the bundle's allowlist.
///
/// This check **is** on `bundle::DETAIL_IS_SAFE_TO_SHARE`, and that was decided here rather
/// than assumed. Both messages [`findings_for`] can produce are compile-time constants with
/// no interpolation whatsoever — no count, no path, no name, nothing taken from the
/// database or from `chronyc`'s output. That is the strongest case the allowlist can be
/// given, and ADR-0020 asks only that somebody make the decision, not that the answer always
/// be "withhold".
const CHECK: &str = "clock.source";

/// What `doctor` reports about the device's time source.
///
/// Always present, even when nothing could be measured, because "we did not find out" is
/// something an operator checking a device before a race needs to see rather than infer
/// from an absent field.
#[derive(Debug, Serialize)]
pub struct ClockSourceView {
    /// Which kind of answer this is, as a fixed token: `measured`, `no_daemon_tool`,
    /// `daemon_unreachable`, or `unreadable`.
    ///
    /// [`Self::detail`] says the same thing in prose, and prose is the wrong thing for a
    /// watchdog or a bundle to branch on. This exists so neither has to parse a sentence
    /// that was written for a person — the bundle in particular needs to tell "not
    /// measured" apart from "measured badly" while copying none of the text that
    /// distinguishes them.
    pub measurement: &'static str,
    /// The determined state, or `null` when nothing could be measured.
    pub state: Option<String>,
    /// One phrase describing where that came from.
    pub detail: String,
    /// Whether the reference is a locally attached clock or a network peer — `local` or
    /// `network`. Absent when there is no reference.
    ///
    /// Separate from [`Self::reference`] because the two have different audiences. The name
    /// can be a LAN address and stays on the device; which *kind* of source a device was
    /// following is the diagnostic, and it names nothing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference_kind: Option<&'static str>,
    /// What the daemon is following. Omitted when there is nothing to follow.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    /// Distance from a reference clock.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stratum: Option<u8>,
    /// Long-term RMS offset in milliseconds — the honest accuracy figure, against the
    /// ±0.1 s budget in `docs/clock-and-time-discipline.md`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rms_offset_ms: Option<f64>,
    /// Whether a leap second is pending, which Q12 asks about and nothing else records.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub leap_pending: Option<bool>,
}

impl ClockSourceView {
    /// Builds the operator-facing view of a reading.
    ///
    /// Not `pub`: the view is this crate's output, and constructing one from a reading is
    /// `doctor`'s business rather than a caller's. Anything wanting the underlying answer
    /// should call [`splitforge_timesource::read_tracking`] and read it directly.
    pub(crate) fn of(reading: &ClockReading) -> Self {
        // Taken once, from the reading rather than from the match arm below, so that the
        // `None`-means-not-measured rule lives in one place and is the same rule the
        // bundle reads. A `state` computed per arm is a `state` that can disagree with
        // `measurement` after somebody edits one arm.
        let state = reading
            .device_clock_state()
            .map(|state| state_name(state).to_owned());

        match reading {
            ClockReading::Tracking(tracking) => {
                // Three cases, not two. An empty reference name means the daemon is
                // following *nothing*, and `is_local_reference` answers false for it for
                // the same reason it answers false for a network peer — so asking only
                // that question reports an unsynchronized device as "following a network
                // time source", which is the one description that is definitely wrong.
                let (reference_kind, following) = if tracking.reference_name.is_empty() {
                    (None, "following no reference at all")
                } else if tracking.is_local_reference() {
                    (Some("local"), "following a local reference clock")
                } else {
                    (Some("network"), "following a network time source")
                };
                Self {
                    measurement: reading.measurement(),
                    state,
                    detail: format!("chrony reports stratum {}, {following}", tracking.stratum),
                    reference_kind,
                    // Absent rather than empty: `"reference": ""` reads as a name that
                    // failed to render, and there is no name to render.
                    reference: reference_kind.map(|_| tracking.reference_name.clone()),
                    stratum: Some(tracking.stratum),
                    rms_offset_ms: Some(tracking.rms_offset_seconds * 1_000.0),
                    leap_pending: Some(tracking.leap.leap_pending()),
                }
            }
            ClockReading::NoDaemonTool => Self {
                measurement: reading.measurement(),
                state,
                detail: "no `chronyc` on this machine, so the time source was not determined"
                    .to_owned(),
                reference_kind: None,
                reference: None,
                stratum: None,
                rms_offset_ms: None,
                leap_pending: None,
            },
            ClockReading::DaemonUnreachable { detail } => Self {
                measurement: reading.measurement(),
                state,
                detail: format!("`chronyc` could not reach the time daemon: {detail}"),
                reference_kind: None,
                reference: None,
                stratum: None,
                rms_offset_ms: None,
                leap_pending: None,
            },
            ClockReading::Unreadable { detail } => Self {
                measurement: reading.measurement(),
                state,
                detail: format!("`chronyc` answered unreadably: {detail}"),
                reference_kind: None,
                reference: None,
                stratum: None,
                rms_offset_ms: None,
                leap_pending: None,
            },
        }
    }
}

/// The stored spelling of a clock state, matching the journal's own column values.
const fn state_name(state: DeviceClockState) -> &'static str {
    match state {
        DeviceClockState::GpsLocked => "gps_locked",
        DeviceClockState::Rtc => "rtc",
        DeviceClockState::NtpSynced => "ntp_synced",
        DeviceClockState::Manual => "manual",
        DeviceClockState::Unsynced => "unsynced",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use splitforge_timesource::classify;

    // Sample `chronyc -c tracking` output. Restated here rather than shared with
    // `splitforge-timesource`'s own tests: these belong to the *view's* cases, and a
    // fixture exported from a shipped crate purely so a test elsewhere can borrow it is a
    // public API nobody asked for.
    const GPS_LOCKED: &str = "50505300,PPS,1,1776153600.123456,0.000000015,0.000000021,\
         0.000000034,-1.234,0.001,0.015,0.000000000,0.000015000,16.0,Normal";
    const UNSYNCED: &str = "00000000,,0,0.000000000,0.000000000,0.000000000,0.000000000,\
         0.000,0.000,0.000,0.000000000,0.000000000,0.0,Not synchronised";
    const NTP_SYNCED: &str = "C0A80101,192.168.1.1,3,1776153600.123456,0.000123,0.000045,\
         0.000067,12.345,0.010,0.250,0.001200,0.004500,64.0,Normal";

    #[test]
    fn the_view_says_what_was_not_measured() {
        let view = ClockSourceView::of(&ClockReading::NoDaemonTool);
        assert!(view.state.is_none());
        assert!(view.detail.contains("not determined"));
        assert!(view.reference.is_none());
    }

    #[test]
    fn a_daemon_following_nothing_is_not_described_as_following_a_network() {
        // Found by running `doctor` against a stub daemon rather than by a test failing.
        // `is_local_reference` is false for a network peer *and* for no reference at all,
        // so classifying on it alone described an unsynchronized device as "following a
        // network time source" — the one answer that is definitely wrong, and the one an
        // operator would read as "the network is fine, look elsewhere".
        let view = ClockSourceView::of(&classify(true, UNSYNCED, ""));

        assert_eq!(view.state.as_deref(), Some("unsynced"));
        assert_eq!(view.reference_kind, None, "got: {:?}", view.detail);
        assert!(view.reference.is_none(), "an empty name is not a name");
        assert!(
            !view.detail.contains("network"),
            "nothing here is following a network: {}",
            view.detail
        );
    }

    #[test]
    fn a_real_reference_still_says_which_kind_it_is() {
        let gps = ClockSourceView::of(&classify(true, GPS_LOCKED, ""));
        assert_eq!(gps.reference_kind, Some("local"));
        assert_eq!(gps.reference.as_deref(), Some("PPS"));

        let peer = ClockSourceView::of(&classify(true, NTP_SYNCED, ""));
        assert_eq!(peer.reference_kind, Some("network"));
        assert_eq!(peer.reference.as_deref(), Some("192.168.1.1"));
    }

    #[test]
    fn the_view_carries_the_accuracy_figure_in_milliseconds() {
        let reading = classify(true, GPS_LOCKED, "");
        let view = ClockSourceView::of(&reading);

        assert_eq!(view.state.as_deref(), Some("gps_locked"));
        assert_eq!(view.stratum, Some(1));
        assert_eq!(view.leap_pending, Some(false));
        // 0.000000034 s expressed in ms.
        let rms = view.rms_offset_ms.expect("a tracking view carries one");
        assert!((rms - 0.000_034).abs() < 1e-9, "got {rms}");
    }

    #[test]
    fn an_unsynchronized_clock_warns_before_the_race() {
        let findings = findings_for(&classify(true, UNSYNCED, ""));

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].check, "clock.source");
        assert_eq!(findings[0].severity, "warning");
        assert!(findings[0].detail.contains("not synchronized"));
    }

    #[test]
    fn a_good_clock_says_nothing() {
        assert!(findings_for(&classify(true, GPS_LOCKED, "")).is_empty());
    }

    #[test]
    fn a_machine_with_no_time_daemon_says_nothing() {
        // The anti-noise rule: a developer laptop must not warn on every `doctor`.
        assert!(findings_for(&ClockReading::NoDaemonTool).is_empty());
    }

    #[test]
    fn a_daemon_that_is_there_and_broken_does_warn() {
        // The distinction the previous test relies on: absent is quiet, present-and-broken
        // is not, because something was configured and is not working.
        for reading in [
            classify(false, "", "506 Cannot talk to daemon"),
            classify(true, "not csv", ""),
        ] {
            let findings = findings_for(&reading);
            assert_eq!(findings.len(), 1, "{reading:?}");
            assert!(findings[0].detail.contains("unknown"), "{reading:?}");
        }
    }

    #[test]
    fn nothing_here_is_ever_an_error() {
        // Q11 is unanswered, so this check warns and blocks. An `error` from `doctor` is
        // reserved for something that will produce wrong results or lose data, and a
        // clock nobody has characterized is not yet known to be either.
        for reading in [
            classify(true, UNSYNCED, ""),
            classify(true, GPS_LOCKED, ""),
            classify(false, "", "boom"),
            classify(true, "not csv", ""),
            ClockReading::NoDaemonTool,
        ] {
            assert!(
                findings_for(&reading)
                    .iter()
                    .all(|finding| finding.severity == "warning"),
                "{reading:?}"
            );
        }
    }
}
