//! Capturing a serial session byte for byte, beside the evidence and never in it (ADR-0040).
//!
//! Almost every claim this crate makes about a module is believed when a capture agrees with
//! it. This keeps the capture: every byte written to the port and every byte read from it,
//! with when and which way, and the port opening, failing, erring and closing.
//!
//! **It wraps the port factory**, so nothing above the port knows it is there: [`capturing`]
//! takes any [`PortFactory`] and returns one whose ports copy what they carry.
//!
//! **The reading thread never waits for it.** The module has no flow control (§ 5.1.4.1), so a
//! thread that stops reading loses reads. Records go to a writer thread through a bounded queue;
//! when it is full a record is dropped and counted, and the next one written says how many were
//! lost. When writing fails the capture stops and says so once, and reading goes on.
//!
//! **It is not evidence.** Nothing in SplitForge reads a capture back. It is not fsynced, and
//! its first line says what it is. The journal and its sidecar remain the record of the reads.

use std::fs::OpenOptions;
use std::io::{self, BufRead, BufWriter, ErrorKind, Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::thread;
use std::time::{Duration, Instant};

use splitforge_domain::ReaderId;
use splitforge_reader::ReaderMessage;
use time::macros::format_description;
use time::{OffsetDateTime, PrimitiveDateTime};

use crate::command::OpCode;
use crate::frame::Response;
use crate::port::{Port, PortFactory};
use crate::provider::{SessionAnchor, TagReportDecoder};
use crate::reassembly::Reassembler;
use crate::tag_report::StreamDecoder;

/// How many records may wait for the writer before new ones are dropped.
///
/// A record is one read or write on the port, so this is several seconds of a busy stream
/// behind a disk that has stalled.
const QUEUE: usize = 4096;

/// What happened on the port.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Event {
    Open,
    OpenFailed(String),
    Sent(Vec<u8>),
    Received(Vec<u8>),
    Error(String),
    Eof,
    Close,
}

/// One line of the capture, as taken on the thread that saw it happen.
#[derive(Debug)]
struct Record {
    at: OffsetDateTime,
    since: Duration,
    event: Event,
    /// Records dropped since the last one that reached the queue.
    dropped_before: u64,
}

/// Where a session's bytes go. Cheap to clone: every port a capturing factory opens holds one.
#[derive(Clone)]
pub struct Capture {
    queue: SyncSender<Record>,
    started: Instant,
    dropped: Arc<AtomicU64>,
    stopped: Arc<AtomicBool>,
}

impl std::fmt::Debug for Capture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Capture")
            .field("dropped", &self.dropped.load(Ordering::Relaxed))
            .field("stopped", &self.stopped.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl Capture {
    /// Opens `path` for appending and starts writing a capture to it.
    ///
    /// Created `0640` on Unix, like the sidecar: it holds every chip identifier the module
    /// reports. An existing file is appended to and keeps its mode.
    ///
    /// # Errors
    ///
    /// Whatever opening the file returned. Asking for a capture that cannot be written is
    /// worth refusing at start, before a bench session relies on it.
    pub fn create(path: &Path) -> io::Result<Self> {
        let mut options = OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o640);
        }
        let file = options.open(path)?;
        Ok(Self::writing_to(file, QUEUE))
    }

    /// A capture written to `writer` by its own thread, through a queue of `capacity`.
    fn writing_to(writer: impl Write + Send + 'static, capacity: usize) -> Self {
        let (queue, records) = mpsc::sync_channel(capacity);
        let stopped = Arc::new(AtomicBool::new(false));
        let writer_stopped = Arc::clone(&stopped);
        let header_at = OffsetDateTime::now_utc();
        // Detached. It ends when every port and factory holding a sender is gone, or when a
        // write fails; neither is anything the read path waits for.
        let _ = thread::Builder::new()
            .name("splitforge-capture".to_owned())
            .spawn(move || write_records(writer, &records, header_at, &writer_stopped));
        Self {
            queue,
            started: Instant::now(),
            dropped: Arc::new(AtomicU64::new(0)),
            stopped,
        }
    }

    /// Records `event`, or counts it as dropped. Never waits.
    fn record(&self, event: Event) {
        if self.stopped.load(Ordering::Relaxed) {
            return;
        }
        let dropped_before = self.dropped.swap(0, Ordering::Relaxed);
        let record = Record {
            at: OffsetDateTime::now_utc(),
            since: self.started.elapsed(),
            event,
            dropped_before,
        };
        match self.queue.try_send(record) {
            Ok(()) => {}
            Err(TrySendError::Full(record)) => {
                self.dropped
                    .fetch_add(record.dropped_before + 1, Ordering::Relaxed);
            }
            Err(TrySendError::Disconnected(_)) => self.stopped.store(true, Ordering::Relaxed),
        }
    }

    /// Whether the capture has stopped because its file could not be written.
    #[must_use]
    pub fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::Relaxed)
    }
}

/// The writer thread: the header, then every record as it comes, flushed when the queue empties.
fn write_records(
    writer: impl Write,
    records: &Receiver<Record>,
    header_at: OffsetDateTime,
    stopped: &AtomicBool,
) {
    let mut out = BufWriter::new(writer);
    let header = format!(
        "# splitforge serial capture, started {}. Diagnostic, not evidence: nothing in \
         SplitForge reads this file back (ADR-0040). `>` was sent to the module, `<` was \
         received from it.\n",
        timestamp(header_at)
    );
    if let Err(error) = out.write_all(header.as_bytes()).and_then(|()| out.flush()) {
        return stop(stopped, &error);
    }
    while let Ok(first) = records.recv() {
        let mut next = Some(first);
        while let Some(record) = next {
            if let Err(error) = out.write_all(line(&record).as_bytes()) {
                return stop(stopped, &error);
            }
            next = records.try_recv().ok();
        }
        if let Err(error) = out.flush() {
            return stop(stopped, &error);
        }
    }
}

fn stop(stopped: &AtomicBool, error: &io::Error) {
    stopped.store(true, Ordering::Relaxed);
    eprintln!(
        "splitforge-thingmagic: the capture stopped because it could not be written ({error}); \
         reading continues without it"
    );
}

fn timestamp(at: OffsetDateTime) -> String {
    let format =
        format_description!("[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:6]Z");
    at.to_offset(time::UtcOffset::UTC)
        .format(&format)
        .unwrap_or_else(|_| "?".to_owned())
}

/// The inverse of [`timestamp`].
fn parse_timestamp(text: &str) -> Option<OffsetDateTime> {
    let format =
        format_description!("[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:6]");
    PrimitiveDateTime::parse(text.strip_suffix('Z')?, &format)
        .ok()
        .map(PrimitiveDateTime::assume_utc)
}

/// One record as its line or lines.
fn line(record: &Record) -> String {
    let mut text = String::new();
    if record.dropped_before > 0 {
        text.push_str(&format!(
            "# {} record(s) dropped before the next: the writer fell behind\n",
            record.dropped_before
        ));
    }
    let what = match &record.event {
        Event::Open => "open".to_owned(),
        Event::OpenFailed(reason) => format!("fail {}", one_line(reason)),
        Event::Sent(bytes) => format!("> {}", hex(bytes)),
        Event::Received(bytes) => format!("< {}", hex(bytes)),
        Event::Error(reason) => format!("error {}", one_line(reason)),
        Event::Eof => "eof".to_owned(),
        Event::Close => "close".to_owned(),
    };
    let micros = record.since.as_micros();
    text.push_str(&format!(
        "{} +{}.{:03}ms {what}\n",
        timestamp(record.at),
        micros / 1000,
        micros % 1000
    ));
    text
}

/// What reading a capture back found, besides the reads themselves (ADR-0041).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Replay {
    /// Connections the capture recorded opening.
    pub connections: u64,
    /// When its first record was taken.
    pub first_at: Option<OffsetDateTime>,
    /// When its last record was taken.
    pub last_at: Option<OffsetDateTime>,
    /// Bytes the service sent to the module.
    pub sent_bytes: u64,
    /// Bytes the service received from it.
    pub received_bytes: u64,
    /// `0x22` responses with status `0x0000`: the frames reads come from.
    pub tag_reports: u64,
    /// Reads decoded from them.
    pub reads: u64,
    /// Tag reports the decoder refused, which the service counted as decode faults.
    pub refused: u64,
    /// `0x22` responses with status `0x0400`: the end of a search cycle in an empty field.
    pub end_of_cycle: u64,
    /// `0x22` responses with any other status, such as `0x0504` for heat.
    pub other_status: u64,
    /// Answers to other commands: the start sequence's.
    pub answers: u64,
    /// Bytes that never formed a frame, as the reassembler counts them.
    pub framing_faults: u64,
    /// Records the capture itself dropped, as its own lines say.
    pub dropped_records: u64,
    /// Lines that are neither a comment nor a record.
    pub unreadable_lines: u64,
}

/// Decodes frames the way the service's provider does, and counts what they were.
struct Replayer<F> {
    found: Replay,
    decoder: StreamDecoder,
    each: F,
}

impl<F: FnMut(ReaderMessage)> Replayer<F> {
    fn frame(&mut self, anchor: &SessionAnchor, response: &Response<'_>) {
        let tag_report = OpCode::ReadTagIdMultiple.to_byte();
        if response.opcode != tag_report {
            self.found.answers += 1;
            return;
        }
        match response.status {
            0x0000 => {
                self.found.tag_reports += 1;
                let refused_before = self.decoder.faults();
                let mut reads = Vec::new();
                self.decoder.decode(response, anchor, &mut reads);
                if self.decoder.faults() > refused_before {
                    self.found.refused += 1;
                }
                self.found.reads += reads.len() as u64;
                for read in reads {
                    (self.each)(read);
                }
            }
            0x0400 => self.found.end_of_cycle += 1,
            _ => self.found.other_status += 1,
        }
    }

    /// Settles a connection that has ended, as the provider does when its port ends.
    fn end(&mut self, connection: Option<(Reassembler, SessionAnchor)>) {
        if let Some((mut reassembler, anchor)) = connection {
            reassembler.flush(|response| self.frame(&anchor, response));
            self.found.framing_faults += reassembler.stats().errors();
        }
    }
}

/// Reads a capture back a line at a time, handing every read in it to `each`, decoded the way
/// the service decoded it (ADR-0041).
///
/// A fresh reassembler for each connection, flushed when it ends, and [`StreamDecoder`] on every
/// `0x22` response with status `0x0000`. The service also flushes when the line is quiet, which a
/// capture does not record; the reassembler reaches the same frames once more bytes arrive
/// (ADR-0030), so the reads are the same. Nothing is held but one line and one partial frame.
///
/// # Errors
///
/// Whatever reading `input` returned. A line that cannot be understood is counted, not an error.
pub fn replay(
    input: impl BufRead,
    reader_id: ReaderId,
    each: impl FnMut(ReaderMessage),
) -> io::Result<Replay> {
    let mut replayer = Replayer {
        found: Replay::default(),
        decoder: StreamDecoder::new(reader_id),
        each,
    };
    let mut connection: Option<(Reassembler, SessionAnchor)> = None;

    for line in input.lines() {
        let line = line?;
        if line.is_empty() {
            continue;
        }
        if let Some(comment) = line.strip_prefix("# ") {
            if let Some((count, _)) = comment.split_once(" record(s) dropped") {
                replayer.found.dropped_records += count.parse::<u64>().unwrap_or(0);
            }
            continue;
        }
        let mut fields = line.splitn(3, ' ');
        let (Some(at), Some(_since), Some(what)) = (fields.next(), fields.next(), fields.next())
        else {
            replayer.found.unreadable_lines += 1;
            continue;
        };
        let Some(at) = parse_timestamp(at) else {
            replayer.found.unreadable_lines += 1;
            continue;
        };
        replayer.found.first_at.get_or_insert(at);
        replayer.found.last_at = Some(at);

        if what == "open" {
            replayer.end(connection.take());
            replayer.found.connections += 1;
            connection = Some((Reassembler::new(), SessionAnchor::now()));
        } else if what == "close" || what == "eof" || what.starts_with("error ") {
            replayer.end(connection.take());
        } else if let Some(hex) = what.strip_prefix("> ") {
            match unhex(hex) {
                Some(bytes) => replayer.found.sent_bytes += bytes.len() as u64,
                None => replayer.found.unreadable_lines += 1,
            }
        } else if let Some(hex) = what.strip_prefix("< ") {
            let Some(bytes) = unhex(hex) else {
                replayer.found.unreadable_lines += 1;
                continue;
            };
            replayer.found.received_bytes += bytes.len() as u64;
            let (reassembler, anchor) =
                connection.get_or_insert_with(|| (Reassembler::new(), SessionAnchor::now()));
            reassembler.feed(&bytes, |response| replayer.frame(anchor, response));
        } else if !what.starts_with("fail ") {
            replayer.found.unreadable_lines += 1;
        }
    }
    replayer.end(connection.take());
    Ok(replayer.found)
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    text.as_bytes()
        .chunks(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok())
        .collect()
}

fn one_line(text: &str) -> String {
    text.replace(['\n', '\r'], " ")
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// A port factory whose ports copy everything they carry to `capture`.
pub fn capturing<P: PortFactory>(factory: P, capture: Capture) -> impl PortFactory {
    Capturing {
        factory,
        capture,
        last_failure: None,
    }
}

struct Capturing<P> {
    factory: P,
    capture: Capture,
    /// The last reason the port would not open, so a port retried every few seconds for an
    /// hour is one line and not a thousand.
    last_failure: Option<String>,
}

impl<P: PortFactory> PortFactory for Capturing<P> {
    fn open(&mut self) -> io::Result<Port> {
        match self.factory.open() {
            Ok(port) => {
                self.last_failure = None;
                self.capture.record(Event::Open);
                Ok(Box::new(Tee {
                    port,
                    capture: self.capture.clone(),
                }))
            }
            Err(error) => {
                let reason = format!("{:?}: {error}", error.kind());
                if self.last_failure.as_deref() != Some(reason.as_str()) {
                    self.capture.record(Event::OpenFailed(reason.clone()));
                    self.last_failure = Some(reason);
                }
                Err(error)
            }
        }
    }
}

/// A port that records what passes through it, and passes everything through unchanged.
struct Tee {
    port: Port,
    capture: Capture,
}

impl Read for Tee {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self.port.read(buffer) {
            Ok(0) => {
                self.capture.record(Event::Eof);
                Ok(0)
            }
            Ok(count) => {
                self.capture
                    .record(Event::Received(buffer[..count].to_vec()));
                Ok(count)
            }
            // An idle module times out several times a second. Recording each would bury
            // everything else, and the absence of anything else says the same.
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::TimedOut | ErrorKind::WouldBlock | ErrorKind::Interrupted
                ) =>
            {
                Err(error)
            }
            Err(error) => {
                self.capture
                    .record(Event::Error(format!("{:?}: {error}", error.kind())));
                Err(error)
            }
        }
    }
}

impl Write for Tee {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        match self.port.write(bytes) {
            Ok(count) => {
                self.capture.record(Event::Sent(bytes[..count].to_vec()));
                Ok(count)
            }
            Err(error) => {
                self.capture
                    .record(Event::Error(format!("{:?}: {error}", error.kind())));
                Err(error)
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.port.flush()
    }
}

impl Drop for Tee {
    fn drop(&mut self) {
        self.capture.record(Event::Close);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    /// What the writer thread wrote, shared with the test.
    #[derive(Clone, Default)]
    struct Written(Arc<Mutex<Vec<u8>>>);

    impl Write for Written {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().expect("written").extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Written {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().expect("written").clone()).expect("utf-8")
        }

        /// Waits for the writer thread to write `needle`, and returns everything written.
        fn until(&self, needle: &str) -> String {
            for _ in 0..400 {
                let text = self.text();
                if text.contains(needle) {
                    return text;
                }
                thread::sleep(Duration::from_millis(5));
            }
            panic!("{needle:?} was never written; got:\n{}", self.text());
        }
    }

    /// A port that answers reads from a script and keeps what is written to it.
    struct Script {
        reads: VecDeque<io::Result<Vec<u8>>>,
    }

    impl Read for Script {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            match self.reads.pop_front() {
                Some(Ok(bytes)) => {
                    buffer[..bytes.len()].copy_from_slice(&bytes);
                    Ok(bytes.len())
                }
                Some(Err(error)) => Err(error),
                None => Ok(0),
            }
        }
    }

    impl Write for Script {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_session_is_recorded_in_order_and_passed_through_unchanged() {
        let written = Written::default();
        let capture = Capture::writing_to(written.clone(), 64);
        let mut factory = capturing(
            || -> io::Result<Port> {
                Ok(Box::new(Script {
                    reads: VecDeque::from([
                        Err(io::Error::from(ErrorKind::TimedOut)),
                        Ok(vec![0xFF, 0x01, 0x2F]),
                        Err(io::Error::from(ErrorKind::BrokenPipe)),
                    ]),
                }))
            },
            capture,
        );

        let mut port = factory.open().expect("open");
        port.write_all(&[0xFF, 0x03, 0x2F]).expect("write");
        let mut buffer = [0_u8; 8];
        assert_eq!(
            port.read(&mut buffer).expect_err("timed out").kind(),
            ErrorKind::TimedOut
        );
        assert_eq!(port.read(&mut buffer).expect("read"), 3);
        assert_eq!(&buffer[..3], [0xFF, 0x01, 0x2F], "passed through unchanged");
        port.read(&mut buffer).expect_err("the port ended");
        drop(port);

        let text = written.until(" close\n");
        let tags: Vec<&str> = text
            .lines()
            .filter(|line| !line.starts_with('#'))
            .map(|line| line.splitn(3, ' ').nth(2).expect("a tag"))
            .collect();
        assert_eq!(
            tags,
            [
                "open",
                "> ff032f",
                "< ff012f",
                "error BrokenPipe: broken pipe",
                "close"
            ],
            "a timeout is not recorded, and everything else is, in order:\n{text}"
        );
        assert!(text.starts_with("# splitforge serial capture"), "{text}");
    }

    #[test]
    fn a_port_that_will_not_open_is_recorded_once_per_reason() {
        let written = Written::default();
        let capture = Capture::writing_to(written.clone(), 64);
        let mut factory = capturing(
            || -> io::Result<Port> { Err(io::Error::from(ErrorKind::NotFound)) },
            capture.clone(),
        );
        for _ in 0..5 {
            factory.open().err().expect("not there");
        }
        capture.record(Event::Eof);

        let text = written.until(" eof\n");
        assert_eq!(
            text.matches(" fail NotFound").count(),
            1,
            "five retries for one reason are one line:\n{text}"
        );
    }

    /// A writer that waits for the test before its first write.
    struct Gated {
        gate: Option<Receiver<()>>,
        written: Written,
    }

    impl Write for Gated {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if let Some(gate) = self.gate.take() {
                let _ = gate.recv();
            }
            self.written.write(bytes)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_writer_that_falls_behind_costs_records_and_says_how_many() {
        // The reading thread must never wait. With the writer stuck and room for one record,
        // the next three are dropped, and the first record after they are is preceded by a
        // line counting them.
        let written = Written::default();
        let (release, gate) = mpsc::channel();
        let capture = Capture::writing_to(
            Gated {
                gate: Some(gate),
                written: written.clone(),
            },
            1,
        );
        for byte in 1..=4_u8 {
            capture.record(Event::Received(vec![byte]));
        }
        release.send(()).expect("release the writer");
        written.until("< 01\n");
        capture.record(Event::Received(vec![5]));

        let text = written.until("< 05\n");
        assert!(
            text.contains("# 3 record(s) dropped before the next"),
            "{text}"
        );
        assert!(!text.contains("< 02") && !text.contains("< 04"), "{text}");
    }

    /// A writer whose disk has failed.
    struct Failing;

    impl Write for Failing {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("the card is full"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_capture_that_cannot_be_written_stops_and_the_port_reads_on() {
        let capture = Capture::writing_to(Failing, 64);
        let mut factory = capturing(
            || -> io::Result<Port> {
                Ok(Box::new(Script {
                    reads: VecDeque::from([Ok(vec![0x22])]),
                }))
            },
            capture.clone(),
        );
        let mut port = factory.open().expect("open");

        for _ in 0..400 {
            if capture.is_stopped() {
                break;
            }
            capture.record(Event::Eof);
            thread::sleep(Duration::from_millis(5));
        }
        assert!(capture.is_stopped(), "a capture that cannot write stops");

        let mut buffer = [0_u8; 4];
        assert_eq!(port.read(&mut buffer).expect("the read is unaffected"), 1);
        assert_eq!(buffer[0], 0x22);
    }

    /// A response frame, as the module sends it.
    fn frame(opcode: u8, status: u16, data: &[u8]) -> Vec<u8> {
        let mut frame = vec![
            crate::frame::SOH,
            u8::try_from(data.len()).expect("small"),
            opcode,
        ];
        frame.extend_from_slice(&status.to_be_bytes());
        frame.extend_from_slice(data);
        let crc = crate::crc::crc16(&frame[1..]);
        frame.extend_from_slice(&crc.to_be_bytes());
        frame
    }

    /// What the decoder makes of `CAPTURED_FRAME`, decoded directly rather than replayed.
    fn the_captured_read() -> ReaderMessage {
        let bytes = crate::crc::CAPTURED_FRAME;
        let Ok(crate::frame::Decoded::Frame { response, .. }) = crate::frame::decode(&bytes) else {
            panic!("CAPTURED_FRAME is a whole frame");
        };
        let mut reads = Vec::new();
        StreamDecoder::new(ReaderId::new("mat")).decode(
            &response,
            &SessionAnchor::now(),
            &mut reads,
        );
        reads.pop().expect("CAPTURED_FRAME decodes to a read")
    }

    #[test]
    fn a_capture_read_back_yields_the_reads_the_service_decoded() {
        // ADR-0041. Written by a real `Capture` through a capturing port, with the tag report
        // split across two reads of the port, as a serial line splits them.
        let written = Written::default();
        let capture = Capture::writing_to(written.clone(), 64);
        let report = crate::crc::CAPTURED_FRAME.to_vec();
        let (first, second) = report.split_at(10);
        let mut factory = capturing(
            {
                let (first, second) = (first.to_vec(), second.to_vec());
                move || -> io::Result<Port> {
                    Ok(Box::new(Script {
                        reads: VecDeque::from([
                            Ok(frame(0x03, 0, &[0x01, 0x02])),
                            Ok(first.clone()),
                            Ok(second.clone()),
                            Ok(frame(0x22, 0x0400, &[])),
                            Ok(frame(0x22, 0x0504, &[])),
                        ]),
                    }))
                }
            },
            capture,
        );
        let mut port = factory.open().expect("open");
        port.write_all(&[0xFF, 0x00, 0x03]).expect("write");
        let mut buffer = [0_u8; 64];
        for _ in 0..5 {
            let _ = port.read(&mut buffer).expect("read");
        }
        drop(port);
        let text = written.until(" close\n");

        let mut reads = Vec::new();
        let found = replay(text.as_bytes(), ReaderId::new("mat"), |read| {
            reads.push(read)
        })
        .expect("replay");

        assert_eq!(found.connections, 1, "{found:?}");
        assert_eq!(found.tag_reports, 1);
        assert_eq!(found.reads, 1);
        assert_eq!(found.refused, 0);
        assert_eq!(found.end_of_cycle, 1);
        assert_eq!(found.other_status, 1, "the 0x0504 a hot module sends");
        assert_eq!(found.answers, 1);
        assert_eq!(found.framing_faults, 0);
        assert_eq!(found.sent_bytes, 3);
        assert_eq!(found.unreadable_lines, 0);
        assert!(found.first_at.is_some() && found.last_at >= found.first_at);

        let expected = the_captured_read();
        assert_eq!(reads.len(), 1);
        assert_eq!(reads[0].raw_payload, expected.raw_payload, "byte for byte");
        assert_eq!(reads[0].chip, expected.chip);
    }

    #[test]
    fn a_frame_cut_off_by_a_closed_connection_is_not_spliced_into_the_next() {
        // The provider starts every connection with a fresh reassembler. A replay that carried
        // half a frame across a reconnection would decode something that was never sent.
        let report = crate::crc::CAPTURED_FRAME;
        let at = "2026-09-26T10:00:00.000000Z";
        let text = format!(
            "{at} +0.000ms open\n\
             {at} +1.000ms < {}\n\
             {at} +2.000ms close\n\
             {at} +3.000ms open\n\
             {at} +4.000ms < {}\n\
             {at} +5.000ms close\n",
            hex(&report[..20]),
            hex(&report[20..]),
        );
        let mut reads = 0;
        let found = replay(text.as_bytes(), ReaderId::new("mat"), |_| reads += 1).expect("replay");
        assert_eq!(found.connections, 2);
        assert_eq!(reads, 0, "{found:?}");
        assert!(found.framing_faults > 0, "{found:?}");
    }

    #[test]
    fn what_a_capture_says_it_lost_and_what_cannot_be_read_are_counted() {
        let at = "2026-09-26T10:00:00.000000Z";
        let text = format!(
            "# splitforge serial capture, started {at}. Diagnostic, not evidence\n\
             {at} +0.000ms open\n\
             # 7 record(s) dropped before the next: the writer fell behind\n\
             {at} +1.000ms < zz\n\
             not a record\n\
             {at} +2.000ms fail NotFound: no such device\n"
        );
        let found = replay(text.as_bytes(), ReaderId::new("mat"), |_| {}).expect("replay");
        assert_eq!(found.dropped_records, 7);
        assert_eq!(found.unreadable_lines, 2, "{found:?}");
    }

    #[cfg(unix)]
    #[test]
    fn a_capture_file_is_readable_by_its_owner_and_group_only() {
        use std::os::unix::fs::PermissionsExt as _;
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("session.capture");
        let _capture = Capture::create(&path).expect("create");
        let mode = std::fs::metadata(&path).expect("stat").permissions().mode();
        assert_eq!(mode & 0o777, 0o640, "it holds chip identifiers: {mode:o}");
    }
}
