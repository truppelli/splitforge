//! `splitforge-capture check`: a capture against the journal, payload for payload (ADR-0041).
//!
//! The captures here are written as `splitforge-edge --capture` writes them, and the journal is
//! filled as the read path fills it, so the check runs over the same two things it will at the
//! bench. The frames are `CAPTURED_FRAME`, a real module's, with one byte of the tag's own
//! checksum varied so that each is a distinct read with the layout the decoder was anchored on.

use std::path::{Path, PathBuf};
use std::process::Command;

use splitforge_domain::{
    ChipId, DeviceClockState, RawRead, RawReadId, RawReadJournal as _, ReaderId, TimestampSource,
};
use splitforge_storage::SqliteJournal;
use splitforge_thingmagic::crc::{CAPTURED_FRAME, crc16};
use splitforge_thingmagic::{Decoded, SessionAnchor, StreamDecoder, TagReportDecoder as _, decode};
use tempfile::TempDir;
use time::OffsetDateTime;
use time::macros::format_description;

/// The binary, built by Cargo for this test run.
const CHECK: &str = env!("CARGO_BIN_EXE_splitforge-capture");

/// `CAPTURED_FRAME`, with the second-to-last byte of the tag record set to `variant`.
fn report(variant: u8) -> Vec<u8> {
    let mut frame = CAPTURED_FRAME.to_vec();
    let last_data = frame.len() - 3;
    frame[last_data] = variant;
    let crc = crc16(&frame[1..frame.len() - 2]);
    let at = frame.len() - 2;
    frame[at..].copy_from_slice(&crc.to_be_bytes());
    frame
}

/// What the service would journal as the payload of `frame`.
fn payload(frame: &[u8]) -> Vec<u8> {
    let Ok(Decoded::Frame { response, .. }) = decode(frame) else {
        panic!("a whole frame");
    };
    let mut reads = Vec::new();
    StreamDecoder::new(ReaderId::new("mat")).decode(&response, &SessionAnchor::now(), &mut reads);
    reads
        .pop()
        .expect("the frame decodes to a read")
        .raw_payload
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn stamp(at: OffsetDateTime) -> String {
    let format =
        format_description!("[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:6]Z");
    at.format(&format).expect("format")
}

/// A database and a capture beside it.
struct Bench {
    _directory: TempDir,
    database: PathBuf,
    capture: PathBuf,
}

impl Bench {
    fn new() -> Self {
        let directory = TempDir::new().expect("tempdir");
        let database = directory.path().join("event.db");
        let capture = directory.path().join("session.capture");
        Self {
            _directory: directory,
            database,
            capture,
        }
    }

    /// A capture of one connection that received `frames`, with `dropped` records lost.
    fn capture(&self, frames: &[Vec<u8>], dropped: u64) {
        let at = stamp(OffsetDateTime::now_utc());
        let mut text = format!(
            "# splitforge serial capture, started {at}. Diagnostic, not evidence\n{at} +0.000ms open\n"
        );
        if dropped > 0 {
            text.push_str(&format!(
                "# {dropped} record(s) dropped before the next: the writer fell behind\n"
            ));
        }
        for frame in frames {
            text.push_str(&format!("{at} +1.000ms < {}\n", hex(frame)));
        }
        text.push_str(&format!("{at} +2.000ms close\n"));
        std::fs::write(&self.capture, text).expect("write the capture");
    }

    /// Journals a read for each frame, the way the read path would.
    fn journal(&self, frames: &[Vec<u8>]) {
        let mut journal = SqliteJournal::open(&self.database).expect("open the journal");
        for frame in frames {
            journal.append(&read(payload(frame))).expect("append");
        }
    }

    /// Sets a read aside for each frame, as the read path does with one it cannot store.
    fn set_aside(&self, frames: &[Vec<u8>]) {
        let mut journal = SqliteJournal::open(&self.database).expect("open the journal");
        for frame in frames {
            journal
                .record_unstorable(&read(payload(frame)), "a test", "splitforge-edge")
                .expect("set aside");
        }
    }

    fn check(&self) -> (i32, serde_json::Value) {
        let output = Command::new(CHECK)
            .arg("check")
            .arg(&self.capture)
            .args(["--database", self.database.to_str().expect("utf-8")])
            .output()
            .expect("run splitforge-capture");
        let report = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "not JSON ({error}): {}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        });
        (output.status.code().expect("an exit code"), report)
    }
}

fn read(raw_payload: Vec<u8>) -> RawRead {
    RawRead {
        id: RawReadId::new(),
        source: ReaderId::new("mat"),
        antenna: Some(1),
        chip: ChipId::new("E2"),
        reader_timestamp: None,
        reader_uptime_us: Some(295_000),
        received_at: OffsetDateTime::now_utc(),
        received_at_monotonic_ns: None,
        rssi_dbm: Some(-60),
        device_clock_state: DeviceClockState::Unsynced,
        timestamp_source: TimestampSource::DeviceReceipt {
            reason: splitforge_domain::FallbackReason::ReaderUptimeOnly,
        },
        clock_offset_ms: None,
        raw_payload,
    }
}

fn frames(variants: &[u8]) -> Vec<Vec<u8>> {
    variants.iter().map(|variant| report(*variant)).collect()
}

#[test]
fn a_journal_holding_every_read_the_capture_holds_agrees() {
    let bench = Bench::new();
    bench.capture(&frames(&[1, 2, 3]), 0);
    bench.journal(&frames(&[1, 2, 3]));

    let (code, report) = bench.check();
    assert_eq!(report["verdict"], "agree", "{report:#}");
    assert_eq!(code, 0);
    assert_eq!(report["capture"]["reads"], 3);
    assert_eq!(report["journal"]["reads"], 3);
}

#[test]
fn a_read_that_arrived_and_is_not_in_the_journal_disagrees_and_is_named() {
    let bench = Bench::new();
    bench.capture(&frames(&[1, 2, 3]), 0);
    bench.journal(&frames(&[1, 3]));

    let (code, report) = bench.check();
    assert_eq!(report["verdict"], "disagree", "{report:#}");
    assert_eq!(code, 1);
    assert_eq!(report["missing_from_journal"], 1);
    assert_eq!(
        report["only_in_capture"],
        serde_json::json!([hex(&payload(&crate::report(2)))])
    );
}

#[test]
fn a_capture_that_dropped_records_cannot_vouch_for_the_journal() {
    // Nothing is missing from the journal, and the journal holds a read the capture lost. With
    // a hole in the capture, that is incomplete, not disagreement.
    let bench = Bench::new();
    bench.capture(&frames(&[1, 2]), 1);
    bench.journal(&frames(&[1, 2, 3]));

    let (code, report) = bench.check();
    assert_eq!(report["verdict"], "incomplete", "{report:#}");
    assert_eq!(code, 2);
    assert_eq!(report["missing_from_capture"], 1);
}

#[test]
fn a_journal_read_that_never_arrived_disagrees_when_the_capture_is_complete() {
    // Something wrote a read the wire never carried: the sidecar route the 2026-09-13 review
    // found, or a hand.
    let bench = Bench::new();
    bench.capture(&frames(&[1, 2]), 0);
    bench.journal(&frames(&[1, 2, 9]));

    let (code, report) = bench.check();
    assert_eq!(report["verdict"], "disagree", "{report:#}");
    assert_eq!(code, 1);
    assert_eq!(report["missing_from_capture"], 1);
}

#[test]
fn a_read_set_aside_on_the_audit_trail_is_accounted_for() {
    // ADR-0031: a read the journal cannot store arrived, and is on the audit trail with its
    // payload rather than in raw_reads. It is not missing.
    let bench = Bench::new();
    bench.capture(&frames(&[1, 2, 3]), 0);
    bench.journal(&frames(&[1, 3]));
    bench.set_aside(&frames(&[2]));

    let (code, report) = bench.check();
    assert_eq!(report["verdict"], "agree", "{report:#}");
    assert_eq!(code, 0);
    assert_eq!(report["journal"]["set_aside"], 1);
}

#[test]
fn a_capture_that_cannot_be_opened_is_not_a_verdict() {
    let bench = Bench::new();
    let output = Command::new(CHECK)
        .arg("check")
        .arg(Path::new("no-such.capture"))
        .args(["--database", bench.database.to_str().expect("utf-8")])
        .output()
        .expect("run splitforge-capture");
    assert_eq!(output.status.code(), Some(3));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("no-such.capture"),
        "the error names the file"
    );
}
