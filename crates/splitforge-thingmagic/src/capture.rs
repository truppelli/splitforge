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
use std::io::{self, BufWriter, ErrorKind, Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::thread;
use std::time::{Duration, Instant};

use time::OffsetDateTime;
use time::macros::format_description;

use crate::port::{Port, PortFactory};

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
