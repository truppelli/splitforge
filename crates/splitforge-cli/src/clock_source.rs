//! Asking the system's time daemon what it believes.
//!
//! The parsing and the classification are pure and live in
//! [`splitforge_domain::timesource`]. This module is the part that runs a process, which is
//! the part that cannot be unit-tested on a machine that may not have `chronyc` installed —
//! so it is kept as thin as it can be, and every decision it makes is in [`classify`], which
//! takes strings and is tested directly.
//!
//! That split is the same one the ThingMagic frame codec uses, and for the same reason: the
//! interesting failures are in the interpretation, not in the `Command`.
//!
//! ## Why a subprocess rather than a syscall
//!
//! `deploy/splitforge-edge.service` sets `ProtectClock=yes` and its comment says why: *"the
//! service reads the clock and never sets it. Clock discipline is the system's NTP or GPS
//! daemon's job."* Reading the daemon's own answer keeps that true, needs no `unsafe` —
//! which `unsafe_code = "deny"` would refuse anyway — and gets a richer answer than
//! `adjtimex` would, because chrony knows *which source* it is following.
//!
//! ## Why this is not on the health endpoint yet
//!
//! `chronyc` blocks while it retries a daemon that is not answering. That is fine for
//! `doctor`, which an operator runs once before the gun, and wrong for `/health`, which a
//! watchdog may poll every few seconds. Giving the service a clock-source field means
//! caching a reading on the existing sample loop rather than shelling out per request, and
//! it belongs with the reader-state work that reshapes health anyway.

use std::io;
use std::process::Command;

use serde::Serialize;
use splitforge_domain::{ClockTracking, DeviceClockState, parse_chrony_tracking};

use crate::report::Finding;

/// The program asked. Not configurable: a path from configuration would be a way to point
/// the clock check at something that answers however the operator wants.
const CHRONYC: &str = "chronyc";

/// How much of a failing command's error text is kept.
///
/// Long enough to name the problem, short enough that a wedged daemon cannot push a page of
/// text into an operator's terminal.
const MAX_DETAIL_BYTES: usize = 200;

/// What asking the daemon produced.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ClockReading {
    /// The daemon answered and its answer parsed.
    Tracking(ClockTracking),
    /// There is no `chronyc` on this machine.
    ///
    /// Deliberately **not** a problem on its own. A developer's laptop runs
    /// `systemd-timesyncd` or nothing at all, and a warning that fires on every machine that
    /// is not a Pi is a warning operators learn to scroll past — the same argument
    /// `STEP_THRESHOLD_MS` makes for not alarming on healthy devices.
    NoDaemonTool,
    /// `chronyc` ran and could not get an answer out of `chronyd`.
    DaemonUnreachable {
        /// What it said, truncated.
        detail: String,
    },
    /// Something answered, and it was not something this code can read.
    Unreadable {
        /// What went wrong, truncated.
        detail: String,
    },
}

impl ClockReading {
    /// The clock state this reading implies, when it implies one.
    ///
    /// `None` means *not measured*, which is different from
    /// [`DeviceClockState::Unsynced`] — that is a measurement saying the clock is bad. A
    /// caller that collapsed the two would report a definite problem it never observed.
    pub(crate) fn device_clock_state(&self) -> Option<DeviceClockState> {
        match self {
            Self::Tracking(tracking) => Some(tracking.device_clock_state()),
            _ => None,
        }
    }
}

/// Trims text to something an operator can read on one line.
fn shorten(text: &str) -> String {
    let cleaned = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if cleaned.len() <= MAX_DETAIL_BYTES {
        return cleaned;
    }
    // Truncate on a character boundary, which matters because this is program output and
    // nothing guarantees it is ASCII.
    let mut end = MAX_DETAIL_BYTES;
    while end > 0 && !cleaned.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &cleaned[..end])
}

/// Turns a finished `chronyc` invocation into a reading.
///
/// Pure, so every branch below is reachable from a test on a machine with no time daemon
/// installed — which includes CI.
pub(crate) fn classify(succeeded: bool, stdout: &str, stderr: &str) -> ClockReading {
    if !succeeded {
        // chronyc reports "506 Cannot talk to daemon" on stdout, not stderr, so both are
        // considered rather than assuming which one carries the complaint.
        let detail = if stderr.trim().is_empty() {
            shorten(stdout)
        } else {
            shorten(stderr)
        };
        return ClockReading::DaemonUnreachable {
            detail: if detail.is_empty() {
                "chronyc exited non-zero and said nothing".to_owned()
            } else {
                detail
            },
        };
    }

    // A zero exit with a complaint on stdout happens: some chronyc builds report an
    // unreachable daemon this way. Catching it here keeps it out of the parser, where it
    // would surface as a confusing field-count error.
    if stdout.contains("Cannot talk to daemon") || stdout.contains("506") && !stdout.contains(',') {
        return ClockReading::DaemonUnreachable {
            detail: shorten(stdout),
        };
    }

    match parse_chrony_tracking(stdout) {
        Ok(tracking) if tracking.looks_plausible() => ClockReading::Tracking(tracking),
        Ok(_) => ClockReading::Unreadable {
            detail: "chronyc's answer parsed but holds impossible values; \
                     its output format may have changed"
                .to_owned(),
        },
        Err(error) => ClockReading::Unreadable {
            detail: shorten(&error.to_string()),
        },
    }
}

/// Asks the daemon. Returns a reading either way; never fails.
///
/// A clock check that returned `Err` would make `doctor` fail on every machine without
/// chrony, which is the opposite of useful — the absence of an answer is itself one of the
/// answers.
pub(crate) fn read_tracking() -> ClockReading {
    match Command::new(CHRONYC).args(["-c", "tracking"]).output() {
        Ok(output) => classify(
            output.status.success(),
            &String::from_utf8_lossy(&output.stdout),
            &String::from_utf8_lossy(&output.stderr),
        ),
        Err(error) if error.kind() == io::ErrorKind::NotFound => ClockReading::NoDaemonTool,
        Err(error) => ClockReading::Unreadable {
            detail: shorten(&error.to_string()),
        },
    }
}

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
    /// Not `pub`: it takes a [`ClockReading`], which is an internal type. The view is the
    /// public surface; how it was arrived at is not.
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
                    measurement: MEASURED,
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
                measurement: NO_DAEMON_TOOL,
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
                measurement: DAEMON_UNREACHABLE,
                state,
                detail: format!("`chronyc` could not reach the time daemon: {detail}"),
                reference_kind: None,
                reference: None,
                stratum: None,
                rms_offset_ms: None,
                leap_pending: None,
            },
            ClockReading::Unreadable { detail } => Self {
                measurement: UNREADABLE,
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

/// A daemon answered and its answer was believed.
pub(crate) const MEASURED: &str = "measured";
/// There is no `chronyc` here, so the question was never put to anything.
pub(crate) const NO_DAEMON_TOOL: &str = "no_daemon_tool";
/// `chronyc` is installed and could not get an answer out of `chronyd`.
pub(crate) const DAEMON_UNREACHABLE: &str = "daemon_unreachable";
/// Something answered and it was not something this code can read.
pub(crate) const UNREADABLE: &str = "unreadable";

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

    const GPS_LOCKED: &str = "50505300,PPS,1,1776153600.123456,0.000000015,0.000000021,\
         0.000000034,-1.234,0.001,0.015,0.000000000,0.000015000,16.0,Normal";
    const UNSYNCED: &str = "00000000,,0,0.000000000,0.000000000,0.000000000,0.000000000,\
         0.000,0.000,0.000,0.000000000,0.000000000,0.0,Not synchronised";
    const NTP_SYNCED: &str = "C0A80101,192.168.1.1,3,1776153600.123456,0.000123,0.000045,\
         0.000067,12.345,0.010,0.250,0.001200,0.004500,64.0,Normal";

    #[test]
    fn a_good_answer_becomes_a_tracking_reading() {
        let reading = classify(true, GPS_LOCKED, "");
        assert_eq!(
            reading.device_clock_state(),
            Some(DeviceClockState::GpsLocked)
        );
    }

    #[test]
    fn an_unsynchronized_daemon_is_a_measurement_not_a_gap() {
        // The distinction this whole type exists for: a daemon that answers "not
        // synchronized" has told us something, and it must not look like not asking.
        let reading = classify(true, UNSYNCED, "");
        assert_eq!(
            reading.device_clock_state(),
            Some(DeviceClockState::Unsynced)
        );

        let missing = ClockReading::NoDaemonTool;
        assert_eq!(missing.device_clock_state(), None);
    }

    #[test]
    fn a_non_zero_exit_reports_the_daemon_as_unreachable() {
        match classify(false, "", "506 Cannot talk to daemon") {
            ClockReading::DaemonUnreachable { detail } => {
                assert!(detail.contains("Cannot talk to daemon"));
            }
            other => panic!("expected an unreachable daemon, got {other:?}"),
        }
    }

    #[test]
    fn a_complaint_on_stdout_is_found_even_with_a_zero_exit() {
        match classify(true, "506 Cannot talk to daemon", "") {
            ClockReading::DaemonUnreachable { .. } => {}
            other => panic!("expected an unreachable daemon, got {other:?}"),
        }
    }

    #[test]
    fn a_silent_failure_still_says_something() {
        match classify(false, "", "") {
            ClockReading::DaemonUnreachable { detail } => assert!(!detail.is_empty()),
            other => panic!("expected an unreachable daemon, got {other:?}"),
        }
    }

    #[test]
    fn unparseable_output_is_unreadable_rather_than_unsynced() {
        // Reporting `Unsynced` here would be inventing a measurement. The clock might be
        // perfect; what failed was the asking.
        match classify(true, "this is not csv", "") {
            ClockReading::Unreadable { detail } => assert!(!detail.is_empty()),
            other => panic!("expected an unreadable answer, got {other:?}"),
        }
        assert_eq!(
            classify(true, "this is not csv", "").device_clock_state(),
            None
        );
    }

    #[test]
    fn an_implausible_answer_is_refused_rather_than_believed() {
        let shifted = GPS_LOCKED.replace(",1,", ",143,");
        match classify(true, &shifted, "") {
            ClockReading::Unreadable { detail } => assert!(detail.contains("impossible")),
            other => panic!("expected the implausible answer to be refused, got {other:?}"),
        }
    }

    #[test]
    fn long_error_text_is_truncated_on_a_character_boundary() {
        let noisy = "é".repeat(500);
        match classify(false, "", &noisy) {
            ClockReading::DaemonUnreachable { detail } => {
                assert!(detail.len() <= MAX_DETAIL_BYTES + 4);
                assert!(detail.ends_with('…'));
            }
            other => panic!("expected an unreachable daemon, got {other:?}"),
        }
    }

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

    #[test]
    fn reading_the_real_daemon_never_panics_and_never_errors() {
        // Runs whatever is on this machine — chrony, something else, or nothing. The point
        // is that all four outcomes are ordinary values and none of them is a failure.
        let reading = read_tracking();
        match reading {
            ClockReading::Tracking(_)
            | ClockReading::NoDaemonTool
            | ClockReading::DaemonUnreachable { .. }
            | ClockReading::Unreadable { .. } => {}
        }
    }
}
