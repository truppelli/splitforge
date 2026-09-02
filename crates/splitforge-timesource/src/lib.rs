//! # splitforge-timesource
//!
//! Asking the system's time daemon what it believes.
//!
//! ## Boundaries
//!
//! - **May depend on:** splitforge-domain
//! - **Must never depend on:** anything else in the workspace
//!
//! These rules come from ADR-0001, are tabulated in `docs/architecture.md`, and are enforced
//! by `crates/splitforge-testkit/tests/dependency_rules.rs` rather than left to review
//! (ADR-0012).
//!
//! ## Why this is a crate rather than a module
//!
//! Two callers need it and they sit on opposite sides of the workspace. `splitforge doctor`
//! reports the device's time source once, before a race; `splitforge-edge` stamps a
//! [`DeviceClockState`] on **every read it writes**, which is permanent evidence, and a
//! service that guessed there would be writing a guess into the journal.
//!
//! It lives with the adapters because it runs a process, and
//! [architecture § 2](../../../docs/architecture.md) puts I/O in adapters. The parsing and
//! the classification it delegates to are pure and stay in
//! [`splitforge_domain::timesource`].
//!
//! ## Why a subprocess rather than a syscall
//!
//! `deploy/splitforge-edge.service` sets `ProtectClock=yes` and its comment says why: *"the
//! service reads the clock and never sets it. Clock discipline is the system's NTP or GPS
//! daemon's job."* Reading the daemon's own answer keeps that true, needs no `unsafe` —
//! which `unsafe_code = "deny"` would refuse anyway — and gets a richer answer than
//! `adjtimex` would, because chrony knows *which source* it is following.
//!
//! ## The shape of this crate
//!
//! [`read_tracking`] runs the process and is the part that cannot be unit-tested on a machine
//! that may not have `chronyc` installed, so it is kept as thin as it can be. Every decision
//! is in [`classify`], which takes strings and is tested directly.
//!
//! That split is the same one the ThingMagic frame codec uses, and for the same reason: the
//! interesting failures are in the interpretation, not in the `Command`.
//!
//! ## Calling this from an async context
//!
//! [`read_tracking`] blocks, and `chronyc` blocks for as long as it takes to give up on a
//! daemon that is not answering. That is fine for a one-shot command and wrong for anything
//! serving requests, so `splitforge-edge` samples it on its own interval inside
//! `spawn_blocking` and caches the answer rather than calling it per request.

// No panicking on any path reachable during an event: a corrupt frame, a missing
// field, or an out-of-range value must become an error the caller can act on, never a
// timer that stops mid-race. Test code is exempt, where panicking on the unexpected is
// the point. See CONTRIBUTING.md, "Code standards".
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use std::io;
use std::process::Command;

use splitforge_domain::{ClockTracking, DeviceClockState, parse_chrony_tracking};

/// The program asked. Not configurable: a path from configuration would be a way to point
/// the clock check at something that answers however the operator wants.
const CHRONYC: &str = "chronyc";

/// How much of a failing command's error text is kept.
///
/// Long enough to name the problem, short enough that a wedged daemon cannot push a page of
/// text into an operator's terminal.
pub const MAX_DETAIL_BYTES: usize = 200;

/// What asking the daemon produced.
#[derive(Debug, Clone, PartialEq)]
pub enum ClockReading {
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
    #[must_use]
    pub fn device_clock_state(&self) -> Option<DeviceClockState> {
        match self {
            Self::Tracking(tracking) => Some(tracking.device_clock_state()),
            _ => None,
        }
    }

    /// The state to **stamp on a read**, which is not the same question as
    /// [`Self::device_clock_state`].
    ///
    /// A read has to carry some state; the column is not nullable, because every read ever
    /// written has carried one. So the absence of a measurement has to become a value here,
    /// and the value is [`DeviceClockState::Unsynced`] — the safe direction, since
    /// `is_trustworthy` is false for it. The error is toward warning about a clock that was
    /// fine rather than staying quiet about one that was not.
    ///
    /// **Reporting keeps the distinction; evidence cannot.** `doctor` and `/health` both
    /// show "not measured" and "measured, and bad" as different facts, because an operator
    /// can act on the difference. A `raw_reads` row has nowhere to put it.
    #[must_use]
    pub fn state_for_evidence(&self) -> DeviceClockState {
        self.device_clock_state()
            .unwrap_or(DeviceClockState::Unsynced)
    }

    /// Which kind of answer this is, as a fixed token.
    ///
    /// Prose is the wrong thing for a watchdog or a diagnostic bundle to branch on, and both
    /// need to tell "not measured" apart from "measured badly" without reading a sentence
    /// written for a person. Lives here rather than beside either reader so that `doctor` and
    /// `/health` cannot drift into spelling the same fact two ways.
    #[must_use]
    pub const fn measurement(&self) -> &'static str {
        match self {
            Self::Tracking(_) => MEASURED,
            Self::NoDaemonTool => NO_DAEMON_TOOL,
            Self::DaemonUnreachable { .. } => DAEMON_UNREACHABLE,
            Self::Unreadable { .. } => UNREADABLE,
        }
    }
}

/// A daemon answered and its answer was believed.
pub const MEASURED: &str = "measured";
/// There is no `chronyc` here, so the question was never put to anything.
pub const NO_DAEMON_TOOL: &str = "no_daemon_tool";
/// `chronyc` is installed and could not get an answer out of `chronyd`.
pub const DAEMON_UNREACHABLE: &str = "daemon_unreachable";
/// Something answered and it was not something this code can read.
pub const UNREADABLE: &str = "unreadable";

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
#[must_use]
pub fn classify(succeeded: bool, stdout: &str, stderr: &str) -> ClockReading {
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
///
/// **Blocks.** See [the note on async callers](self#calling-this-from-an-async-context).
#[must_use]
pub fn read_tracking() -> ClockReading {
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

#[cfg(test)]
mod tests {
    use super::*;

    const GPS_LOCKED: &str = "50505300,PPS,1,1776153600.123456,0.000000015,0.000000021,\
         0.000000034,-1.234,0.001,0.015,0.000000000,0.000015000,16.0,Normal";
    const UNSYNCED: &str = "00000000,,0,0.000000000,0.000000000,0.000000000,0.000000000,\
         0.000,0.000,0.000,0.000000000,0.000000000,0.0,Not synchronised";

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
    fn evidence_gets_a_state_even_when_nothing_was_measured() {
        // The one place the distinction above has to collapse, and it collapses toward
        // distrust. A read's clock-state column is not nullable and never has been.
        assert_eq!(
            ClockReading::NoDaemonTool.state_for_evidence(),
            DeviceClockState::Unsynced
        );
        assert!(
            !ClockReading::NoDaemonTool
                .state_for_evidence()
                .is_trustworthy()
        );

        // A real measurement is still passed through unchanged.
        assert_eq!(
            classify(true, GPS_LOCKED, "").state_for_evidence(),
            DeviceClockState::GpsLocked
        );
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
