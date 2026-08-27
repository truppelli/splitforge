//! What the system's time daemon believes, and what that makes [`DeviceClockState`].
//!
//! [`DeviceClockState`] is recorded on every read as evidence, and
//! [`DeviceClockState::is_trustworthy`] is what decides whether results derived under it can
//! be published without a caveat. Until now nothing in this workspace *determined* it —
//! [the roadmap](../../../docs/roadmap.md) recorded that determining it needed syscalls
//! `unsafe_code = "deny"` rules out reaching for directly.
//!
//! That turned out to be the wrong way round the problem.
//! [`deploy/splitforge-edge.service`](../../../deploy/splitforge-edge.service) already says
//! it: *"the service reads the clock and never sets it. Clock discipline is the system's NTP
//! or GPS daemon's job."* So **ask the daemon rather than the kernel.** `chronyc -c tracking`
//! prints one CSV line describing exactly what SplitForge needs to know, no `unsafe` is
//! involved, and `ProtectClock=yes` stays untouched because reading is all that happens.
//!
//! This module is the parsing and the classification, which are pure. Running the command is
//! I/O and lives in the caller.
//!
//! # What this can and cannot determine
//!
//! Three of the five states are reachable from tracking output. **Two are not**, and the
//! distinction is worth stating plainly because
//! [hardware-plan.md § 7](../../../docs/hardware-plan.md#7-software-plan) originally
//! expected `chronyc` to report an RTC. That plan has since been corrected in place; this
//! table is what corrected it:
//!
//! | State | From tracking output? |
//! |---|---|
//! | [`DeviceClockState::GpsLocked`] | **Yes** — a local reference clock with a GPS/PPS refid |
//! | [`DeviceClockState::NtpSynced`] | **Yes** — synchronized to any other source |
//! | [`DeviceClockState::Unsynced`] | **Yes** — leap status says so, or there is no reference |
//! | [`DeviceClockState::Rtc`] | **No** |
//! | [`DeviceClockState::Manual`] | **No** |
//!
//! A Pi whose clock was set from a DS3231 at boot and has reached no time source since
//! reports **"Not synchronised"**, exactly like a Pi that booted with no clock at all —
//! because from chrony's point of view they are the same situation. Telling them apart needs
//! to know whether an RTC device exists and was read at boot, which is a different question
//! asked of a different place.
//!
//! So this module reports [`DeviceClockState::Unsynced`] for both, which is the **safe**
//! direction: `is_trustworthy` is false for `Unsynced` and true for `Rtc`, so the error is
//! toward warning about a clock that was fine rather than staying quiet about one that was
//! not.

use std::net::IpAddr;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::read::DeviceClockState;

/// How many comma-separated fields `chronyc -c tracking` prints.
///
/// Read by index, and extras are ignored rather than rejected: a future chrony that appends
/// a field should not take the clock check offline, and appending is the only schema change
/// a positional reader survives. A chrony that *reordered* fields would be misread, which is
/// the cost of positional parsing and the reason [`ClockTracking::looks_plausible`] exists.
pub const TRACKING_FIELDS: usize = 14;

/// Reference names that mean a locally attached GPS or pulse-per-second source.
///
/// Matched case-insensitively, as a prefix, because chrony pads a refid to four characters
/// and drivers append an index — `PPS0`, `NMEA1`. Anything not on this list that is still a
/// local reference is reported as [`DeviceClockState::NtpSynced`] rather than guessed at:
/// both are trustworthy, so the practical difference is a label, and inventing a GPS that is
/// not there would be the one direction that matters.
const GPS_REFERENCE_PREFIXES: &[&str] = &["PPS", "GPS", "NMEA", "GPSD", "SHM", "SOCK"];

/// Chrony's leap-second status field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeapStatus {
    /// Synchronized, no leap second pending.
    Normal,
    /// A leap second will be inserted at the end of the current UTC day.
    InsertSecond,
    /// A leap second will be deleted at the end of the current UTC day.
    DeleteSecond,
    /// The daemon is not synchronized to any source.
    NotSynchronised,
}

impl LeapStatus {
    /// Whether the daemon considers itself synchronized at all.
    #[must_use]
    pub const fn is_synchronized(self) -> bool {
        !matches!(self, Self::NotSynchronised)
    }

    /// Whether a leap second is pending, which
    /// [Q12](../../../docs/open-questions.md#q12-leap-second-handling) cares about.
    ///
    /// Recording that this was true during an event is cheap and is the part
    /// [Q12](../../../docs/open-questions.md#q12-leap-second-handling) says must not happen
    /// by accident. Deciding what to *do* about it is still open.
    #[must_use]
    pub const fn leap_pending(self) -> bool {
        matches!(self, Self::InsertSecond | Self::DeleteSecond)
    }
}

impl FromStr for LeapStatus {
    type Err = TrackingError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text.trim() {
            "Normal" => Ok(Self::Normal),
            "Insert second" => Ok(Self::InsertSecond),
            "Delete second" => Ok(Self::DeleteSecond),
            // Chrony spells this the British way. Accepting both costs nothing and removes
            // a way for this to break on a locale or a version nobody tested against.
            "Not synchronised" | "Not synchronized" => Ok(Self::NotSynchronised),
            other => Err(TrackingError::UnknownLeapStatus {
                found: other.to_owned(),
            }),
        }
    }
}

/// One reading of what the time daemon believes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClockTracking {
    /// The reference's identifier, as chrony's hex refid.
    pub reference_id: u32,
    /// The reference's name or address. Empty when there is no reference.
    pub reference_name: String,
    /// Distance from a reference clock. 1 is a directly attached source; 0 means none.
    pub stratum: u8,
    /// Chrony's estimate of the offset between system time and true time, in seconds.
    pub system_time_offset_seconds: f64,
    /// The offset measured at the last update, in seconds.
    pub last_offset_seconds: f64,
    /// Long-term root-mean-square offset, in seconds. The honest accuracy number.
    pub rms_offset_seconds: f64,
    /// The rate the system clock is being corrected by, in ppm.
    pub frequency_ppm: f64,
    /// Residual frequency error against the current source, in ppm.
    pub residual_frequency_ppm: f64,
    /// Estimated error bound on the frequency, in ppm.
    pub skew_ppm: f64,
    /// Total round-trip delay to the stratum-1 reference, in seconds.
    pub root_delay_seconds: f64,
    /// Total accumulated dispersion, in seconds.
    pub root_dispersion_seconds: f64,
    /// Seconds between the last two clock updates.
    pub update_interval_seconds: f64,
    /// Leap-second status.
    pub leap: LeapStatus,
}

impl ClockTracking {
    /// Whether the reference is a locally attached clock rather than a network peer.
    ///
    /// Decided by whether the reference name parses as an IP address: chrony prints an
    /// address for a network source and a short refid string for a hardware one.
    #[must_use]
    pub fn is_local_reference(&self) -> bool {
        !self.reference_name.is_empty() && self.reference_name.parse::<IpAddr>().is_err()
    }

    /// Whether the local reference looks like a GPS or PPS source.
    #[must_use]
    pub fn is_gps_reference(&self) -> bool {
        if !self.is_local_reference() {
            return false;
        }
        let name = self.reference_name.trim().to_ascii_uppercase();
        GPS_REFERENCE_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(prefix))
    }

    /// Classifies this reading as a [`DeviceClockState`].
    ///
    /// Never returns [`DeviceClockState::Rtc`] or [`DeviceClockState::Manual`] — see the
    /// module documentation for why neither is visible from here, and why reporting
    /// [`DeviceClockState::Unsynced`] instead is the safe direction to be wrong in.
    #[must_use]
    pub fn device_clock_state(&self) -> DeviceClockState {
        // Any one of these means there is nothing to trust, and they are checked before the
        // reference is inspected because an unsynchronized daemon can still be holding a
        // stale reference name from before it lost the source.
        if !self.leap.is_synchronized() || self.stratum == 0 || self.reference_id == 0 {
            return DeviceClockState::Unsynced;
        }

        if self.is_gps_reference() {
            DeviceClockState::GpsLocked
        } else {
            DeviceClockState::NtpSynced
        }
    }

    /// A sanity check on the shape of a parsed reading.
    ///
    /// Positional parsing cannot notice that chrony reordered its columns — every field
    /// would still parse, and the answer would be quietly wrong. This catches the coarse
    /// version of that: values that are individually well-formed and jointly impossible.
    ///
    /// It is not a proof. It is the difference between a misparse that announces itself and
    /// one that reports a stratum of 143 as a plausible clock.
    #[must_use]
    pub fn looks_plausible(&self) -> bool {
        // Stratum 16 is NTP's "unsynchronized" marker and the highest chrony emits.
        self.stratum <= 16
            && self.root_delay_seconds >= 0.0
            && self.root_dispersion_seconds >= 0.0
            && self.update_interval_seconds >= 0.0
            && self.skew_ppm >= 0.0
            && self.rms_offset_seconds >= 0.0
    }
}

/// Why a tracking line could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TrackingError {
    /// The line had fewer fields than the format defines.
    #[error(
        "expected at least {TRACKING_FIELDS} comma-separated fields from `chronyc -c tracking`, found {found}"
    )]
    TooFewFields {
        /// How many arrived.
        found: usize,
    },
    /// A field that should hold a number did not.
    #[error("field {index} ({name}) is not a number: {value:?}")]
    NotANumber {
        /// Zero-based position in the line.
        index: usize,
        /// What that position is supposed to be.
        name: &'static str,
        /// The text that was there.
        value: String,
    },
    /// The leap-status field held something unrecognized.
    #[error("unrecognized leap status {found:?}")]
    UnknownLeapStatus {
        /// The text that was there.
        found: String,
    },
    /// There was no line at all.
    #[error("`chronyc -c tracking` produced no output")]
    Empty,
}

/// Parses one line of `chronyc -c tracking` output.
///
/// Leading and trailing whitespace is ignored, and only the first non-empty line is read —
/// which is all the command produces, but a caller passing captured output should not have
/// to strip a trailing newline to be understood.
///
/// # Errors
///
/// Returns [`TrackingError`] if the line is absent, too short, or holds a field that cannot
/// be read as the type its position requires.
pub fn parse_chrony_tracking(output: &str) -> Result<ClockTracking, TrackingError> {
    let line = output
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .ok_or(TrackingError::Empty)?;

    let fields: Vec<&str> = line.split(',').map(str::trim).collect();
    if fields.len() < TRACKING_FIELDS {
        return Err(TrackingError::TooFewFields {
            found: fields.len(),
        });
    }

    // `field` is infallible for every index below TRACKING_FIELDS, which the check above has
    // just established. Written as a closure returning a default rather than as indexing so
    // that a future edit which adds a field cannot turn a typo into a panic.
    let field = |index: usize| fields.get(index).copied().unwrap_or_default();

    let number = |index: usize, name: &'static str| -> Result<f64, TrackingError> {
        let value = field(index);
        value.parse::<f64>().map_err(|_| TrackingError::NotANumber {
            index,
            name,
            value: value.to_owned(),
        })
    };

    let reference_id =
        u32::from_str_radix(field(0), 16).map_err(|_| TrackingError::NotANumber {
            index: 0,
            name: "reference id",
            value: field(0).to_owned(),
        })?;

    let stratum = field(2)
        .parse::<u8>()
        .map_err(|_| TrackingError::NotANumber {
            index: 2,
            name: "stratum",
            value: field(2).to_owned(),
        })?;

    Ok(ClockTracking {
        reference_id,
        reference_name: field(1).to_owned(),
        stratum,
        // Field 3 is the reference timestamp. Deliberately not parsed: nothing here needs
        // it, and a field parsed only to be discarded is a field that can fail for nothing.
        system_time_offset_seconds: number(4, "system time offset")?,
        last_offset_seconds: number(5, "last offset")?,
        rms_offset_seconds: number(6, "rms offset")?,
        frequency_ppm: number(7, "frequency")?,
        residual_frequency_ppm: number(8, "residual frequency")?,
        skew_ppm: number(9, "skew")?,
        root_delay_seconds: number(10, "root delay")?,
        root_dispersion_seconds: number(11, "root dispersion")?,
        update_interval_seconds: number(12, "update interval")?,
        leap: field(13).parse()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Pi disciplined by a PPS refclock — the deployment this project recommends.
    const GPS_LOCKED: &str = "50505300,PPS,1,1776153600.123456,0.000000015,0.000000021,\
         0.000000034,-1.234,0.001,0.015,0.000000000,0.000015000,16.0,Normal";

    /// A laptop on an ordinary network, synchronized to an upstream server.
    const NTP_SYNCED: &str = "C0A80101,192.168.1.1,3,1776153600.123456,0.000123,0.000045,\
         0.000067,12.345,0.010,0.250,0.001200,0.004500,64.0,Normal";

    /// A device that has reached no time source. What a Pi 3 looks like at a trailhead,
    /// with or without an RTC fitted.
    const UNSYNCED: &str = "00000000,,0,0.000000000,0.000000000,0.000000000,0.000000000,\
         0.000,0.000,0.000,0.000000000,0.000000000,0.0,Not synchronised";

    #[test]
    fn a_pps_reference_is_gps_locked() {
        let tracking = parse_chrony_tracking(GPS_LOCKED).expect("well-formed");

        assert_eq!(tracking.stratum, 1);
        assert_eq!(tracking.reference_name, "PPS");
        assert!(tracking.is_local_reference());
        assert!(tracking.is_gps_reference());
        assert_eq!(tracking.device_clock_state(), DeviceClockState::GpsLocked);
        assert!(tracking.device_clock_state().is_trustworthy());
    }

    #[test]
    fn a_network_source_is_ntp_synced() {
        let tracking = parse_chrony_tracking(NTP_SYNCED).expect("well-formed");

        assert_eq!(tracking.stratum, 3);
        assert!(
            !tracking.is_local_reference(),
            "a dotted quad is a network peer, not a hardware clock"
        );
        assert_eq!(tracking.device_clock_state(), DeviceClockState::NtpSynced);
    }

    #[test]
    fn no_source_is_unsynced() {
        let tracking = parse_chrony_tracking(UNSYNCED).expect("well-formed");

        assert_eq!(tracking.leap, LeapStatus::NotSynchronised);
        assert!(!tracking.leap.is_synchronized());
        assert_eq!(tracking.device_clock_state(), DeviceClockState::Unsynced);
        assert!(!tracking.device_clock_state().is_trustworthy());
    }

    #[test]
    fn an_rtc_only_device_reports_unsynced_rather_than_rtc() {
        // The correction to hardware-plan.md § 7, asserted rather than only written down.
        // A DS3231 that set the clock at boot leaves chrony saying exactly what a device
        // with no clock at all says, so `Rtc` is not reachable from here — and the state
        // this returns instead must be the untrustworthy one.
        let tracking = parse_chrony_tracking(UNSYNCED).expect("well-formed");

        assert_ne!(tracking.device_clock_state(), DeviceClockState::Rtc);
        assert_eq!(tracking.device_clock_state(), DeviceClockState::Unsynced);
        assert!(
            !tracking.device_clock_state().is_trustworthy(),
            "being wrong about an RTC must fail toward warning, never toward silence"
        );
    }

    #[test]
    fn an_unsynchronized_daemon_holding_a_stale_reference_is_still_unsynced() {
        // Losing a source does not always clear the reference name, so leap status has to
        // win over a reference that looks perfectly good.
        let stale = GPS_LOCKED.replace("Normal", "Not synchronised");
        let tracking = parse_chrony_tracking(&stale).expect("well-formed");

        assert!(
            tracking.is_gps_reference(),
            "the reference still reads as GPS"
        );
        assert_eq!(tracking.device_clock_state(), DeviceClockState::Unsynced);
    }

    #[test]
    fn a_zero_reference_id_is_unsynced_whatever_else_it_says() {
        let contradictory = UNSYNCED.replace("Not synchronised", "Normal");
        let tracking = parse_chrony_tracking(&contradictory).expect("well-formed");

        assert_eq!(tracking.reference_id, 0);
        assert_eq!(tracking.device_clock_state(), DeviceClockState::Unsynced);
    }

    #[test]
    fn every_gps_reference_spelling_is_recognized() {
        for name in [
            "PPS", "PPS0", "pps", "GPS", "NMEA", "NMEA1", "GPSD", "SHM0", "SOCK",
        ] {
            let line = GPS_LOCKED.replace(",PPS,", &format!(",{name},"));
            let tracking = parse_chrony_tracking(&line).expect("well-formed");
            assert_eq!(
                tracking.device_clock_state(),
                DeviceClockState::GpsLocked,
                "{name} should read as a GPS reference"
            );
        }
    }

    #[test]
    fn an_unrecognized_local_reference_is_not_promoted_to_gps() {
        // A local refclock this code does not know about is trustworthy but is not a GPS,
        // and claiming otherwise would put a reference in the record that never existed.
        let line = GPS_LOCKED.replace(",PPS,", ",XYZ,");
        let tracking = parse_chrony_tracking(&line).expect("well-formed");

        assert!(tracking.is_local_reference());
        assert!(!tracking.is_gps_reference());
        assert_eq!(tracking.device_clock_state(), DeviceClockState::NtpSynced);
    }

    #[test]
    fn an_ipv6_reference_is_a_network_peer() {
        let line = NTP_SYNCED.replace(",192.168.1.1,", ",2001:db8::1,");
        let tracking = parse_chrony_tracking(&line).expect("well-formed");

        assert!(!tracking.is_local_reference());
        assert_eq!(tracking.device_clock_state(), DeviceClockState::NtpSynced);
    }

    #[test]
    fn the_measurements_are_read_from_the_right_columns() {
        // Positional parsing's whole risk is an off-by-one that still parses. Every numeric
        // field here is a different value, so a shifted column shows up as a wrong number
        // rather than as a passing test.
        let tracking = parse_chrony_tracking(NTP_SYNCED).expect("well-formed");

        assert!((tracking.system_time_offset_seconds - 0.000123).abs() < f64::EPSILON);
        assert!((tracking.last_offset_seconds - 0.000045).abs() < f64::EPSILON);
        assert!((tracking.rms_offset_seconds - 0.000067).abs() < f64::EPSILON);
        assert!((tracking.frequency_ppm - 12.345).abs() < f64::EPSILON);
        assert!((tracking.residual_frequency_ppm - 0.010).abs() < f64::EPSILON);
        assert!((tracking.skew_ppm - 0.250).abs() < f64::EPSILON);
        assert!((tracking.root_delay_seconds - 0.001200).abs() < f64::EPSILON);
        assert!((tracking.root_dispersion_seconds - 0.004500).abs() < f64::EPSILON);
        assert!((tracking.update_interval_seconds - 64.0).abs() < f64::EPSILON);
    }

    #[test]
    fn a_negative_frequency_is_accepted_because_clocks_run_fast() {
        let tracking = parse_chrony_tracking(GPS_LOCKED).expect("well-formed");
        assert!(tracking.frequency_ppm < 0.0);
        assert!(tracking.looks_plausible());
    }

    #[test]
    fn a_pending_leap_second_is_visible_without_changing_the_state() {
        // Q12 asks what to do about a leap second. Recording that one is pending is the
        // cheap half, and it must not quietly downgrade a clock that is otherwise fine.
        let line = GPS_LOCKED.replace("Normal", "Insert second");
        let tracking = parse_chrony_tracking(&line).expect("well-formed");

        assert!(tracking.leap.leap_pending());
        assert!(tracking.leap.is_synchronized());
        assert_eq!(tracking.device_clock_state(), DeviceClockState::GpsLocked);
    }

    #[test]
    fn trailing_newlines_and_blank_lines_are_tolerated() {
        let padded = format!("\n\n  {GPS_LOCKED}  \n\n");
        let tracking = parse_chrony_tracking(&padded).expect("well-formed");
        assert_eq!(tracking.stratum, 1);
    }

    #[test]
    fn extra_trailing_fields_are_ignored_rather_than_rejected() {
        // A future chrony that appends a column must not take the clock check offline.
        let extended = format!("{GPS_LOCKED},something-new,42");
        let tracking = parse_chrony_tracking(&extended).expect("well-formed");
        assert_eq!(tracking.device_clock_state(), DeviceClockState::GpsLocked);
    }

    #[test]
    fn empty_output_is_an_error_rather_than_a_default() {
        // The case that actually happens: chronyd is not running, so chronyc prints
        // nothing. Returning a zeroed reading would report a confident `Unsynced` that was
        // never measured.
        assert_eq!(parse_chrony_tracking(""), Err(TrackingError::Empty));
        assert_eq!(parse_chrony_tracking("\n  \n"), Err(TrackingError::Empty));
    }

    #[test]
    fn a_short_line_names_how_many_fields_arrived() {
        match parse_chrony_tracking("50505300,PPS,1,Normal") {
            Err(TrackingError::TooFewFields { found }) => assert_eq!(found, 4),
            other => panic!("expected a field-count error, got {other:?}"),
        }
    }

    #[test]
    fn a_bad_number_names_the_column_it_was_in() {
        let broken = NTP_SYNCED.replace(",12.345,", ",banana,");
        match parse_chrony_tracking(&broken) {
            Err(TrackingError::NotANumber { name, value, .. }) => {
                assert_eq!(name, "frequency");
                assert_eq!(value, "banana");
            }
            other => panic!("expected a number error naming the column, got {other:?}"),
        }
    }

    #[test]
    fn an_unknown_leap_status_is_refused_rather_than_assumed_normal() {
        let broken = GPS_LOCKED.replace("Normal", "Sideways");
        match parse_chrony_tracking(&broken) {
            Err(TrackingError::UnknownLeapStatus { found }) => assert_eq!(found, "Sideways"),
            other => panic!("expected a leap-status error, got {other:?}"),
        }
    }

    #[test]
    fn both_spellings_of_not_synchronised_are_accepted() {
        for spelling in ["Not synchronised", "Not synchronized"] {
            let line = GPS_LOCKED.replace("Normal", spelling);
            let tracking = parse_chrony_tracking(&line).expect("well-formed");
            assert_eq!(tracking.device_clock_state(), DeviceClockState::Unsynced);
        }
    }

    #[test]
    fn an_implausible_stratum_is_flagged() {
        let shifted = NTP_SYNCED.replace(",3,", ",143,");
        let tracking = parse_chrony_tracking(&shifted).expect("parses, but");
        assert!(
            !tracking.looks_plausible(),
            "stratum 143 should not read as a plausible clock"
        );
    }

    #[test]
    fn the_recommended_deployments_all_look_plausible() {
        for line in [GPS_LOCKED, NTP_SYNCED, UNSYNCED] {
            let tracking = parse_chrony_tracking(line).expect("well-formed");
            assert!(tracking.looks_plausible(), "{line}");
        }
    }

    #[test]
    fn no_input_makes_the_parser_panic() {
        // Sweeping the shapes a broken daemon, a truncated pipe, or a different tool
        // entirely could produce. The assertion is that control returns at all.
        let corpus = [
            "",
            ",",
            ",,,,,,,,,,,,,",
            "\0",
            "not csv at all",
            "1,2,3,4,5,6,7,8,9,10,11,12,13,14",
            "-,-,-,-,-,-,-,-,-,-,-,-,-,-",
        ];
        for input in corpus {
            let _ = parse_chrony_tracking(input);
        }

        // Every truncation of a good line, which is what a half-read pipe looks like.
        for cut in 0..GPS_LOCKED.len() {
            if GPS_LOCKED.is_char_boundary(cut) {
                let _ = parse_chrony_tracking(&GPS_LOCKED[..cut]);
            }
        }
    }
}
