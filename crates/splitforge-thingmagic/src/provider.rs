//! The connection lifecycle, and the [`ReaderProvider`] it presents to the rest of
//! SplitForge.
//!
//! Everything below this module is pure. This is where the bytes actually come from, where
//! a port that vanishes is reopened, and where frames become [`ReaderMessage`] values that
//! the timing engine cannot distinguish from the simulator's.
//!
//! # What is not here, and why
//!
//! **Nothing turns a frame's payload into a tag read.** That is [`TagReportDecoder`], and
//! this crate ships no implementation of it.
//!
//! The frame layout came from the user guide, which is archived and quoted in
//! [vendor-documents.md](../../../docs/readers/vendor-documents.md). The *payload* layout did
//! not: the guide describes tag metadata in prose — Table 13 and § 8.8.3 name the fields and
//! their meanings — and defers to the MercuryAPI SDK for "code details".
//!
//! That gap is now closed. Opcodes and search flags are in [`crate::command`], and
//! `vendor-documents.md` records the whole tag-report layout: the `TMR_TRD_METADATA_FLAG_*`
//! values, the fourteen flagged fields in flag-bit order, and the response-type byte that
//! separates a tag frame from a status frame.
//!
//! **The seam stays a trait anyway**, and the decision is
//! [ADR-0004](../../../docs/adr/0004-llrp-first-reader-adapter.md)'s: *do not start from
//! protocol documentation alone*. A byte layout taken from a document and never checked against
//! a capture produces a parser that is internally consistent, externally wrong, and fully
//! tested — which is worse than no parser at all, because it looks finished. **This crate has
//! already done that once**, with a CRC the user guide named and the module did not compute.
//!
//! Three specifics from the archived SDK belong in whatever fills this seam, because each is a
//! way to be confidently wrong:
//!
//! - **Walk the flag bits ascending and reject an unknown high bit.** The layout gained five
//!   fields between 2009 and 2023. A decoder that stops at `0x0100` does not fail loudly on a
//!   module that sets `0x1000`; it reads the brand identifier as the EPC length and returns a
//!   plausible wrong chip id.
//! - **Three fields are conditional on the protocol as well as on their flag**, so the protocol
//!   field has to be kept rather than skipped over.
//! - **Reject a status frame before reading the flags word.** Parsed as a tag report it becomes
//!   a fabricated read in an append-only table.
//!
//! Everything around that one seam is real and tested: opening, reading, reassembly,
//! resynchronization, bounded jittered reconnect, and the counters that make loss visible.

use std::cell::Cell;
use std::io::{self, ErrorKind, Read};
use std::thread;
use std::time::{Duration, Instant};

use splitforge_domain::ReaderId;
use splitforge_reader::{Disconnection, ReaderEvent, ReaderFaults, ReaderMessage, ReaderProvider};
use time::OffsetDateTime;
use tokio::sync::mpsc;

use crate::frame::Response;
use crate::port::{Port, PortFactory};
use crate::reassembly::Reassembler;
use crate::start::{Progress, Refusal, StartSequence, Starting};

/// When a connection began, on both clocks.
///
/// The module's per-read timestamp is *relative*: user guide § 8.8.3 defines it as "the time
/// the tag was read, relative to the time the command to read was issued, in milliseconds."
/// It is therefore **not** microseconds since boot, and calling it an uptime without saying
/// so would hand a later reader of the journal a number they cannot place on a calendar.
///
/// This is the anchor that makes it placeable, recorded per connection because that is the
/// scope over which the module's own counter is continuous. Both clocks are captured at the
/// same instant: the wall clock says *when*, and the monotonic clock is the only safe basis
/// for the subtraction, per
/// [clock discipline § 3](../../../docs/clock-and-time-discipline.md#3-the-three-clocks).
#[derive(Debug, Clone, Copy)]
pub struct SessionAnchor {
    /// Device wall clock when the connection opened.
    pub opened_at_utc: OffsetDateTime,
    /// Device monotonic clock at the same instant.
    pub opened_at: Instant,
}

impl SessionAnchor {
    /// Captures both clocks now.
    #[must_use]
    pub fn now() -> Self {
        Self {
            opened_at_utc: OffsetDateTime::now_utc(),
            opened_at: Instant::now(),
        }
    }
}

/// Turns a verified frame into the reads it carries, if any.
///
/// **This crate ships no implementation.** See the module documentation: the payload layout
/// is not in any document this project holds, and guessing at one would produce a parser
/// that passes its own tests and misreads a race.
///
/// A frame is not a read. Most frames are answers to configuration commands and carry none,
/// which is why this appends to a buffer rather than returning one value.
pub trait TagReportDecoder: Send {
    /// Appends every read this frame carries to `out`.
    ///
    /// `anchor` is the connection this frame arrived on, for turning the module's
    /// relative-millisecond timestamp into something that can later be placed on a calendar.
    ///
    /// Implementations must not panic on any payload. A frame has passed its CRC by the time
    /// it arrives here, which proves it was transmitted intact — not that it means what this
    /// decoder expects.
    fn decode(
        &mut self,
        response: &Response<'_>,
        anchor: &SessionAnchor,
        out: &mut Vec<ReaderMessage>,
    );

    /// Verified frames this decoder refused, since it was created.
    ///
    /// Reported to the service as [`ReaderFaults::decoding`]. Zero by default, which is the
    /// truth for a decoder that refuses nothing.
    fn faults(&self) -> u64 {
        0
    }
}

/// A decoder that reads nothing, and counts what it declined to read.
///
/// **This is not a parser and it is not a stub for one.** The distinction is the whole reason
/// it can exist in a crate that refuses to ship a tag-report decoder: a guessed parser emits
/// reads that look right and are wrong, which is what
/// [ADR-0004](../../../docs/adr/0004-llrp-first-reader-adapter.md) forbids and what this crate
/// has already done once with the CRC. An empty decoder emits nothing, so there is no chip
/// identifier for it to be wrong about.
///
/// # What it is for
///
/// Everything in Milestone 3a's exit criterion that is about the **transport** rather than
/// about reads. A port that opens, a cable pulled out, a reconnection, a confirmed gap in the
/// evidence, health degrading and recovering — none of that needs a single tag to be decoded,
/// and all of it needs a real module on a real port. Composing a reader with this decoder is
/// how that half becomes observable the day the hardware arrives, without waiting for the
/// other half.
///
/// # What it costs
///
/// A service composed this way records **no reads at all**, and says so: `reads_received`
/// stays at zero while [`Self::frames`] climbs. That is an honest report of a device that is
/// receiving frames and understanding none of them, and it must not be mistaken for a working
/// timer. The silence watchdog will open *suspected* gaps during a running race, correctly —
/// nothing is being recorded.
#[derive(Debug, Default)]
pub struct UndecodedReports {
    frames: u64,
}

impl UndecodedReports {
    /// A decoder that has seen nothing yet.
    #[must_use]
    pub const fn new() -> Self {
        Self { frames: 0 }
    }

    /// How many frames have arrived and been declined.
    ///
    /// The one number that distinguishes *"the module is streaming and nothing here can read
    /// it"* from *"the module is silent"* — two situations that are otherwise identical from
    /// outside, because both produce no reads.
    #[must_use]
    pub const fn frames(&self) -> u64 {
        self.frames
    }
}

impl TagReportDecoder for UndecodedReports {
    fn decode(
        &mut self,
        _response: &Response<'_>,
        _anchor: &SessionAnchor,
        _out: &mut Vec<ReaderMessage>,
    ) {
        // Deliberately does not touch `out`. Every other line in this file exists so that a
        // read which arrives is preserved; this one exists so that a read which cannot be
        // understood is not invented.
        self.frames = self.frames.saturating_add(1);
    }
}

/// Bounded, jittered reconnect delays.
///
/// Bounded because a reader that has been unplugged for an hour should still be picked up
/// within seconds of being plugged back in; jittered because two devices that lost the same
/// switch should not retry in lockstep forever.
#[derive(Debug, Clone, Copy)]
pub struct Backoff {
    /// Delay after the first failure.
    pub first: Duration,
    /// The ceiling every later delay is clamped to.
    pub max: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Self {
            first: Duration::from_millis(100),
            max: Duration::from_secs(5),
        }
    }
}

impl Backoff {
    /// The delay before attempt `failures + 1`, jittered into the upper half of its window.
    ///
    /// Jitter comes from a counter-seeded xorshift rather than a random-number dependency.
    /// Spreading retries is the entire requirement here, and a crate on the read path is a
    /// crate that can lose a read.
    #[must_use]
    pub fn delay(&self, failures: u32, seed: u64) -> Duration {
        let shift = failures.min(16);
        let scaled = self.first.saturating_mul(1_u32 << shift);
        let capped = scaled.min(self.max);

        let mut state = seed
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .wrapping_add(u64::from(failures) | 1);
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;

        // Half the window, plus up to half again: never zero, never above the cap.
        let half = capped / 2;
        let spread = u32::try_from(state % 1_000).unwrap_or(0);
        half + (half * spread) / 1_000
    }
}

/// A ThingMagic serial module, presented as a source of reads.
#[derive(Debug)]
pub struct ThingMagicReader<P, D> {
    reader_id: ReaderId,
    factory: P,
    decoder: D,
    backoff: Backoff,
    capacity: usize,
    /// Framing faults from connections that have ended. Each connection starts a fresh
    /// [`Reassembler`], so the running total has to be kept here.
    framing_before: u64,
    /// The totals last sent, so a report goes out only when one changes.
    faults_reported: ReaderFaults,
    /// What each connection sends before it streams, or `None` to only listen.
    start: Option<StartSequence>,
    /// The last refusal logged, so a module refusing the same command on every attempt says
    /// so once rather than once per reconnect.
    last_refusal: Option<Refusal>,
    /// The last reason a port would not open, logged once per outage rather than once per
    /// retry.
    last_open_failure: Option<String>,
}

/// How a connection's read loop finished.
enum Pumped {
    /// The consumer dropped the channel, so there is nobody left to read for.
    ConsumerGone,
    /// The port ended, and reconnecting is the right response.
    Ended {
        /// Whether the connection proved itself before it ended. See
        /// [`ThingMagicReader::established`].
        established: bool,
        /// Which way it ended.
        cause: Disconnection,
    },
}

impl<P, D> ThingMagicReader<P, D>
where
    P: PortFactory,
    D: TagReportDecoder,
{
    /// A reader that opens its port through `factory` and decodes payloads with `decoder`.
    pub fn new(reader_id: ReaderId, factory: P, decoder: D) -> Self {
        Self {
            reader_id,
            factory,
            decoder,
            backoff: Backoff::default(),
            capacity: 256,
            framing_before: 0,
            faults_reported: ReaderFaults::default(),
            start: None,
            last_refusal: None,
            last_open_failure: None,
        }
    }

    /// Starts the stream on every connection with `sequence`.
    ///
    /// Without one, the reader only listens, which hears nothing from a module that has not
    /// been told to read (user guide § 7). With one, a connection is announced when the module
    /// accepts the start command, and a module that refuses a step ends the connection as
    /// [`Disconnection::NotStarted`].
    #[must_use]
    pub fn with_start(mut self, sequence: StartSequence) -> Self {
        self.start = Some(sequence);
        self
    }

    /// Overrides the reconnect schedule.
    #[must_use]
    pub const fn with_backoff(mut self, backoff: Backoff) -> Self {
        self.backoff = backoff;
        self
    }

    /// Overrides how many reads may queue before the reader stops reading.
    ///
    /// Deliberately finite. An unbounded queue in front of a consumer that has stalled turns
    /// a slow disk into an out-of-memory kill, which loses the whole race rather than the
    /// tail of it.
    #[must_use]
    pub const fn with_capacity(mut self, capacity: usize) -> Self {
        self.capacity = capacity;
        self
    }
}

impl<P, D> ReaderProvider for ThingMagicReader<P, D>
where
    P: PortFactory + 'static,
    D: TagReportDecoder + 'static,
{
    fn reader_id(&self) -> ReaderId {
        self.reader_id.clone()
    }

    fn start(self: Box<Self>) -> mpsc::Receiver<ReaderEvent> {
        let (sender, receiver) = mpsc::channel(self.capacity.max(1));

        // A dedicated OS thread rather than an async task, because `serialport` reads block
        // and blocking inside the runtime would stall every other task on that worker. It
        // also keeps `tokio-serial` and its transitive dependencies off the read path for
        // the sake of an ergonomic gain this loop does not need.
        thread::spawn(move || {
            let mut this = *self;
            this.run(&sender);
        });

        receiver
    }
}

impl<P, D> ThingMagicReader<P, D>
where
    P: PortFactory,
    D: TagReportDecoder,
{
    /// Connects, reads, and reconnects until the consumer goes away.
    ///
    /// **Every turn of this loop is reported**, which is the point of it being a loop over
    /// events rather than over reads. This function already knew when the port died — it had
    /// to, in order to decide whether to reopen one — and used to keep that to itself, which
    /// left the service downstream with nothing to distinguish a dead reader from a quiet
    /// one. The knowledge now leaves the thread it was computed on.
    ///
    /// **Only a connection that proved itself resets the backoff.** Opening a port proves
    /// nothing: a wrong device node or a loose cable opens and ends at once, and resetting on
    /// every open kept that loop at the shortest delay forever, writing a pair of gap rows each
    /// time.
    fn run(&mut self, sender: &mpsc::Sender<ReaderEvent>) {
        let mut failures: u32 = 0;
        let mut attempt: u64 = 0;

        loop {
            attempt += 1;

            match self.factory.open() {
                Ok(port) => {
                    let Pumped::Ended { established, cause } = self.pump(port, sender) else {
                        // The consumer is gone. A reader with nowhere to deliver should stop
                        // reading rather than buffer a race into memory.
                        return;
                    };
                    failures = if established {
                        self.last_open_failure = None;
                        0
                    } else {
                        failures.saturating_add(1)
                    };
                    // `pump` returned because the connection ended rather than because the
                    // consumer left, so this is the induced disconnection the exit criterion
                    // is about — reported before the backoff sleep, not after it, so the gap
                    // is recorded as starting when the port died rather than seconds later.
                    //
                    // Sent for a connection that never proved itself too. If it follows an
                    // outage, the gap is already open and opening it again writes nothing.
                    if sender
                        .blocking_send(ReaderEvent::Disconnected { cause })
                        .is_err()
                    {
                        return;
                    }
                }
                Err(error) => {
                    failures = failures.saturating_add(1);
                    self.report_open_failure(&error);
                    // Sent on every failed attempt rather than only the first. Opening a gap
                    // is idempotent at the journal — `open_reader_gap` returns the one
                    // already open — so a stateless report here costs a row nobody writes,
                    // and saves this loop from keeping a duplicate of state the evidence
                    // already holds durably.
                    if sender
                        .blocking_send(ReaderEvent::Disconnected {
                            cause: Disconnection::NotOpened,
                        })
                        .is_err()
                    {
                        return;
                    }
                }
            }

            if sender.is_closed() {
                return;
            }

            // Measured on the monotonic clock by construction: `thread::sleep` cannot be
            // dragged backwards by a wall-clock step mid-race.
            thread::sleep(self.backoff.delay(failures, attempt));
        }
    }

    /// Whether a connection has shown there is a module on the other end.
    ///
    /// A verified frame shows it. So does staying up for [`Backoff::max`], for a module that
    /// is there and has nothing to say yet. That threshold is the longest reconnect delay, so a
    /// port that dies just after it still cannot churn faster than the capped backoff would
    /// allow, and it adds no second number to justify.
    ///
    /// **The connection is announced here rather than when the port opens**, so that a run of
    /// ports that open and die at once is one outage and one gap rather than two rows per
    /// attempt. The cost is that a quiet module is reported connected up to `max` late.
    ///
    /// **With a start sequence, only the module accepting the start command shows it.** A
    /// module that answers the version and refuses the region is there, and is not recording.
    /// Announcing it at its first answer would reset the backoff and write a pair of gap rows
    /// on every attempt, which is the churn this function exists to prevent.
    fn established(
        &self,
        reassembler: &Reassembler,
        opened: &SessionAnchor,
        started: bool,
    ) -> bool {
        if self.start.is_some() {
            return started;
        }
        reassembler.stats().frames > 0 || opened.opened_at.elapsed() >= self.backoff.max
    }

    /// Logs why a connection did not start, unless it is the reason already logged.
    fn report_refusal(&mut self, refusal: Refusal) {
        if self.last_refusal == Some(refusal) {
            return;
        }
        self.last_refusal = Some(refusal);
        eprintln!(
            "splitforge-thingmagic: the module did not start streaming: {refusal}. The \
             connection is closed and retried; this is logged again only if the reason changes."
        );
    }

    /// Logs why the port would not open, unless it is the reason already logged. Returns
    /// whether it logged.
    ///
    /// **The reason is the whole diagnosis, and it used to be discarded.** A port that would
    /// not open was a confirmed gap with no cause attached anywhere, so the three failures an
    /// operator actually meets were indistinguishable: *No such file or directory* (nothing
    /// plugged in, or no device name), *Permission denied* (the node's owner), and *Operation
    /// not permitted* (a device filter refusing it). They are fixed in three different
    /// places — see docs/deployment.md — and `port::open` had already kept them apart, only
    /// for this loop to drop the error on the floor.
    ///
    /// Forgotten once a connection proves itself, so the next outage says why it happened.
    fn report_open_failure(&mut self, error: &io::Error) -> bool {
        let reason = error.to_string();
        if self.last_open_failure.as_deref() == Some(reason.as_str()) {
            return false;
        }
        eprintln!(
            "splitforge-thingmagic: the port did not open: {reason}. It is retried with \
             backoff; this is logged again only if the reason changes."
        );
        self.last_open_failure = Some(reason);
        true
    }

    /// Reads one connection to its end.
    fn pump(&mut self, mut port: Port, sender: &mpsc::Sender<ReaderEvent>) -> Pumped {
        // A fresh connection starts from no partial frame. Bytes held over from the previous
        // one would splice two sessions into a frame that was never transmitted.
        let mut reassembler = Reassembler::new();
        let opened = SessionAnchor::now();
        // What a tag report's relative timestamp counts from. User guide § 8.8.3 defines it as
        // relative to *"the time the command to read was issued"*, so it moves when the start
        // command is sent. A `Cell`, because the frame handler below both reads and moves it.
        let anchor = Cell::new(opened);
        let mut chunk = [0_u8; 512];
        let mut reads: Vec<ReaderMessage> = Vec::new();
        let mut announced = false;

        let mut starting = self.start.as_ref().map(Starting::new);
        let mut progress = Progress::Waiting;
        if let Some(sequence) = starting.as_mut() {
            match sequence.begin(&mut port, Instant::now()) {
                Ok(step) => progress = step,
                Err(_) => {
                    return Pumped::Ended {
                        established: false,
                        cause: Disconnection::Ended,
                    };
                }
            }
        }

        loop {
            let read = port.read(&mut chunk);
            let decoder = &mut self.decoder;
            let mut written: io::Result<()> = Ok(());
            // Every verified frame goes one of two ways. The answer the start sequence is
            // waiting for goes to it; everything else is a report, including one from a stream
            // the last connection left running, and goes to the decoder.
            let mut decode = |response: &Response<'_>| {
                if let Some(sequence) = starting.as_mut()
                    && sequence.claims(response)
                {
                    let was_starting = sequence.is_starting();
                    match sequence.answer(response, &mut port, Instant::now()) {
                        Ok(step) => progress = step,
                        Err(error) => written = Err(error),
                    }
                    if !was_starting && sequence.is_starting() {
                        anchor.set(SessionAnchor::now());
                    }
                } else {
                    decoder.decode(response, &anchor.get(), &mut reads);
                }
            };

            let ended = match read {
                Ok(0) => true,
                Ok(count) => {
                    reassembler.feed(&chunk[..count], &mut decode);
                    false
                }
                // A timeout means the module had nothing to say, which during a race is most
                // of the time. Treating it as a disconnection would reopen the port between
                // every pair of runners.
                //
                // It does mean nothing is still arriving for a partial frame, so whatever the
                // reassembler is holding is settled now rather than at the next runner.
                Err(error) if error.kind() == ErrorKind::TimedOut => {
                    reassembler.flush(&mut decode);
                    false
                }
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(_) => true,
            };

            // Nothing more can arrive on a connection that has ended, so whole frames held
            // behind a partial one are handed over rather than dropped with the buffer.
            if ended {
                reassembler.flush(&mut decode);
            }

            // The deadline for the answer being waited on, checked on every turn, which a real
            // port makes at least once per read timeout.
            if !ended
                && written.is_ok()
                && matches!(progress, Progress::Waiting)
                && let Some(sequence) = starting.as_mut()
            {
                match sequence.tick(&mut port, Instant::now()) {
                    Ok(step) => progress = step,
                    Err(error) => written = Err(error),
                }
            }

            let started = matches!(progress, Progress::Started);
            if started {
                self.last_refusal = None;
            }
            let cause = match progress {
                Progress::Refused(refusal) => {
                    self.report_refusal(refusal);
                    Some(Disconnection::NotStarted)
                }
                _ if ended || written.is_err() => Some(Disconnection::Ended),
                _ => None,
            };

            if !announced && self.established(&reassembler, &opened, started) {
                if sender.blocking_send(ReaderEvent::Connected).is_err() {
                    return Pumped::ConsumerGone;
                }
                announced = true;
            }

            for message in reads.drain(..) {
                // Blocking is the backpressure: if the journal cannot keep up, the right
                // response is to stop taking bytes off the port, not to grow a queue.
                if sender.blocking_send(ReaderEvent::Read(message)).is_err() {
                    return Pumped::ConsumerGone;
                }
            }

            let framing = self.framing_before + reassembler.stats().errors();
            if !self.report_faults(framing, sender) {
                return Pumped::ConsumerGone;
            }

            if let Some(cause) = cause {
                self.framing_before = framing;
                return Pumped::Ended {
                    established: announced,
                    cause,
                };
            }
        }
    }

    /// Sends the fault totals if either has changed. Returns `false` if the consumer is gone.
    ///
    /// After the reads, so a consumer that sees the count has already counted every read
    /// that came out of the same bytes.
    fn report_faults(&mut self, framing: u64, sender: &mpsc::Sender<ReaderEvent>) -> bool {
        let faults = ReaderFaults {
            framing,
            decoding: self.decoder.faults(),
        };
        if faults == self.faults_reported {
            return true;
        }
        self.faults_reported = faults;
        sender.blocking_send(ReaderEvent::Faults(faults)).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crc::crc16;
    use crate::frame::{Decoded, RESPONSE_HEADER_LEN, SOH};
    use splitforge_domain::ChipId;
    use splitforge_reader::ReaderTimestamp;
    use std::io;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn response(opcode: u8, data: &[u8]) -> Vec<u8> {
        let mut frame = Vec::with_capacity(RESPONSE_HEADER_LEN + data.len() + 2);
        frame.push(SOH);
        frame.push(data.len() as u8);
        frame.push(opcode);
        frame.push(0);
        frame.push(0);
        frame.extend_from_slice(data);
        let crc = crc16(&frame[1..]);
        frame.push((crc >> 8) as u8);
        frame.push((crc & 0xFF) as u8);
        frame
    }

    /// Turns every frame into one read whose chip id is the frame's payload, in hex.
    ///
    /// Not a protocol. A stand-in that lets the lifecycle be tested without anybody
    /// pretending to know what a real payload looks like.
    struct StubDecoder;

    impl TagReportDecoder for StubDecoder {
        fn decode(
            &mut self,
            response: &Response<'_>,
            _anchor: &SessionAnchor,
            out: &mut Vec<ReaderMessage>,
        ) {
            let chip: String = response
                .data
                .iter()
                .map(|byte| format!("{byte:02X}"))
                .collect();
            out.push(ReaderMessage {
                source: ReaderId::new("mat"),
                antenna: Some(1),
                chip: ChipId::new(&chip),
                timestamp: ReaderTimestamp::Absent,
                rssi_dbm: None,
                raw_payload: response.data.to_vec(),
            });
        }
    }

    /// Accepts and discards everything, as a port with nobody listening for commands would.
    macro_rules! discards_writes {
        ($port:ty) => {
            impl io::Write for $port {
                fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                    Ok(bytes.len())
                }

                fn flush(&mut self) -> io::Result<()> {
                    Ok(())
                }
            }
        };
    }

    /// A port that yields scripted chunks and then fails, as often as the script says.
    struct ScriptedPort {
        chunks: Vec<Vec<u8>>,
        index: usize,
    }

    discards_writes!(ScriptedPort);

    impl Read for ScriptedPort {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let Some(chunk) = self.chunks.get(self.index) else {
                // End of script: behave like a cable pulled out.
                return Err(io::Error::from(ErrorKind::BrokenPipe));
            };
            self.index += 1;
            let count = chunk.len().min(buffer.len());
            buffer[..count].copy_from_slice(&chunk[..count]);
            Ok(count)
        }
    }

    /// The next actual read, skipping the connection lifecycle around it.
    ///
    /// The tests that use this are about bytes becoming reads. That a connection is
    /// announced before them, and its loss after them, is the subject of
    /// `the_lifecycle_is_reported_...` below rather than a detail every test restates.
    async fn next_read(receiver: &mut mpsc::Receiver<ReaderEvent>) -> ReaderMessage {
        loop {
            match receiver.recv().await.expect("the reader is still running") {
                ReaderEvent::Read(message) => return message,
                ReaderEvent::Connected
                | ReaderEvent::Disconnected { .. }
                | ReaderEvent::Faults(_) => {}
            }
        }
    }

    fn reader<P: PortFactory + 'static>(factory: P) -> Box<dyn ReaderProvider> {
        Box::new(
            ThingMagicReader::new(ReaderId::new("mat"), factory, StubDecoder)
                .with_backoff(Backoff {
                    first: Duration::from_millis(1),
                    max: Duration::from_millis(5),
                })
                .with_capacity(16),
        )
    }

    #[tokio::test]
    async fn reads_arrive_through_the_port_boundary() {
        let script = vec![response(0x22, &[0xAB, 0xCD])];
        let factory = move || -> io::Result<Port> {
            Ok(Box::new(ScriptedPort {
                chunks: script.clone(),
                index: 0,
            }) as Port)
        };

        let mut receiver = reader(factory).start();
        let message = next_read(&mut receiver).await;
        assert_eq!(message.chip, ChipId::new("ABCD"));
        assert_eq!(message.raw_payload, vec![0xAB, 0xCD]);
    }

    #[tokio::test]
    async fn a_frame_split_across_two_reads_still_arrives() {
        let whole = response(0x22, &[0x01, 0x02, 0x03]);
        let (head, tail) = whole.split_at(4);
        let script = vec![head.to_vec(), tail.to_vec()];
        let factory = move || -> io::Result<Port> {
            Ok(Box::new(ScriptedPort {
                chunks: script.clone(),
                index: 0,
            }) as Port)
        };

        let mut receiver = reader(factory).start();
        let message = next_read(&mut receiver).await;
        assert_eq!(message.chip, ChipId::new("010203"));
    }

    #[tokio::test]
    async fn the_port_is_reopened_after_it_fails() {
        // Every open yields one frame and then breaks, so reads only keep arriving if the
        // lifecycle actually reconnects.
        let opens = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&opens);
        let factory = move || -> io::Result<Port> {
            let index = counter.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(ScriptedPort {
                chunks: vec![response(0x22, &[index as u8])],
                index: 0,
            }) as Port)
        };

        let mut receiver = reader(factory).start();
        for expected in 0..3_u8 {
            let message = next_read(&mut receiver).await;
            assert_eq!(message.chip, ChipId::new(format!("{expected:02X}")));
        }
        assert!(opens.load(Ordering::SeqCst) >= 3);
    }

    #[tokio::test]
    async fn a_port_that_will_not_open_is_retried_rather_than_fatal() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&attempts);
        let factory = move || -> io::Result<Port> {
            if counter.fetch_add(1, Ordering::SeqCst) < 3 {
                return Err(io::Error::from(ErrorKind::NotFound));
            }
            Ok(Box::new(ScriptedPort {
                chunks: vec![response(0x22, &[0xFF])],
                index: 0,
            }) as Port)
        };

        let mut receiver = reader(factory).start();
        let message = next_read(&mut receiver).await;
        assert_eq!(message.chip, ChipId::new("FF"));
        assert!(attempts.load(Ordering::SeqCst) >= 4);
    }

    #[tokio::test]
    async fn a_timeout_is_not_a_disconnection() {
        // A port that times out twice, then delivers. If a timeout were treated as the port
        // ending, the open count would climb; it must not.
        let opens = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&opens);

        struct TimeoutThenData {
            remaining: usize,
            frame: Vec<u8>,
            sent: bool,
        }

        discards_writes!(TimeoutThenData);

        impl Read for TimeoutThenData {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                if self.remaining > 0 {
                    self.remaining -= 1;
                    return Err(io::Error::from(ErrorKind::TimedOut));
                }
                if self.sent {
                    return Err(io::Error::from(ErrorKind::BrokenPipe));
                }
                self.sent = true;
                let count = self.frame.len().min(buffer.len());
                buffer[..count].copy_from_slice(&self.frame[..count]);
                Ok(count)
            }
        }

        let factory = move || -> io::Result<Port> {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(TimeoutThenData {
                remaining: 2,
                frame: response(0x22, &[0x5A]),
                sent: false,
            }) as Port)
        };

        let mut receiver = reader(factory).start();
        let message = next_read(&mut receiver).await;
        assert_eq!(message.chip, ChipId::new("5A"));
        assert_eq!(opens.load(Ordering::SeqCst), 1);
    }

    /// A port that delivers one chunk and then times out, many times, before it ends.
    ///
    /// `ended` is set only when it finally ends, so a test can tell a read released by a
    /// timeout from one released by the connection closing.
    struct ChunkThenQuiet {
        chunk: Option<Vec<u8>>,
        timeouts: usize,
        ended: Arc<AtomicUsize>,
    }

    discards_writes!(ChunkThenQuiet);

    impl Read for ChunkThenQuiet {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if let Some(chunk) = self.chunk.take() {
                let count = chunk.len().min(buffer.len());
                buffer[..count].copy_from_slice(&chunk[..count]);
                return Ok(count);
            }
            if self.timeouts > 0 {
                self.timeouts -= 1;
                thread::sleep(Duration::from_millis(1));
                return Err(io::Error::from(ErrorKind::TimedOut));
            }
            self.ended.store(1, Ordering::SeqCst);
            Err(io::Error::from(ErrorKind::BrokenPipe))
        }
    }

    #[tokio::test]
    async fn a_quiet_line_releases_a_read_held_behind_a_false_header() {
        // `0xFF` and a legal length, then a real frame far shorter than the length claims. The
        // reassembler cannot tell whether the header is real, so it waits. A timeout is what
        // tells it nothing more is coming, and the read must arrive then, with the port still
        // open, rather than when the next runner crosses.
        let mut chunk = vec![SOH, 200];
        chunk.extend(response(0x22, &[0xAB, 0xCD]));
        let ended = Arc::new(AtomicUsize::new(0));
        let flag = Arc::clone(&ended);

        let factory = move || -> io::Result<Port> {
            Ok(Box::new(ChunkThenQuiet {
                chunk: Some(chunk.clone()),
                timeouts: 2_000,
                ended: Arc::clone(&flag),
            }) as Port)
        };

        let mut receiver = reader(factory).start();
        let message = next_read(&mut receiver).await;
        assert_eq!(message.chip, ChipId::new("ABCD"));
        assert_eq!(
            ended.load(Ordering::SeqCst),
            0,
            "the read waited for the connection to end instead of for the line to go quiet"
        );
    }

    #[tokio::test]
    async fn a_read_held_behind_a_false_header_survives_the_connection_ending() {
        // The same bytes, and then the cable comes out. The frame is whole and verified; the
        // buffer it was sitting in belongs to a connection that no longer exists, and it must
        // not go with it.
        let mut chunk = vec![SOH, 200];
        chunk.extend(response(0x22, &[0x01, 0x02]));
        let script = vec![chunk];
        let factory = move || -> io::Result<Port> {
            Ok(Box::new(ScriptedPort {
                chunks: script.clone(),
                index: 0,
            }) as Port)
        };

        let mut receiver = reader(factory).start();
        let message = next_read(&mut receiver).await;
        assert_eq!(message.chip, ChipId::new("0102"));
    }

    #[tokio::test]
    async fn fault_totals_leave_the_provider_thread() {
        // Noise, then a frame that passes its CRC and that this decoder refuses. The service
        // can only degrade health on counts it is told, and these used to be read only in
        // tests.
        struct Refuses;

        impl TagReportDecoder for Refuses {
            fn decode(
                &mut self,
                _response: &Response<'_>,
                _anchor: &SessionAnchor,
                _out: &mut Vec<ReaderMessage>,
            ) {
            }

            fn faults(&self) -> u64 {
                7
            }
        }

        let mut chunk = vec![0x00, 0x11, 0x22];
        chunk.extend(response(0x22, &[0x01]));
        let script = vec![chunk];
        let factory = move || -> io::Result<Port> {
            Ok(Box::new(ScriptedPort {
                chunks: script.clone(),
                index: 0,
            }) as Port)
        };

        let mut receiver: mpsc::Receiver<ReaderEvent> = Box::new(
            ThingMagicReader::new(ReaderId::new("mat"), factory, Refuses)
                .with_backoff(Backoff {
                    first: Duration::from_millis(1),
                    max: Duration::from_secs(1),
                })
                .with_capacity(16),
        )
        .start();

        assert_eq!(receiver.recv().await, Some(ReaderEvent::Connected));
        assert_eq!(
            receiver.recv().await,
            Some(ReaderEvent::Faults(ReaderFaults {
                framing: 1,
                decoding: 7
            }))
        );
        assert_eq!(
            receiver.recv().await,
            Some(ReaderEvent::Disconnected {
                cause: Disconnection::Ended
            }),
            "unchanged totals are not sent again"
        );
    }

    /// A response frame with a status word, as the module sends one.
    fn answer(opcode: u8, status: u16, data: &[u8]) -> Vec<u8> {
        let mut frame = Vec::with_capacity(RESPONSE_HEADER_LEN + data.len() + 2);
        frame.push(SOH);
        frame.push(u8::try_from(data.len()).expect("small"));
        frame.push(opcode);
        frame.extend_from_slice(&status.to_be_bytes());
        frame.extend_from_slice(data);
        let crc = crc16(&frame[1..]);
        frame.extend_from_slice(&crc.to_be_bytes());
        frame
    }

    /// A module on the other end of the port: it answers every command it is sent, echoing the
    /// option byte of a `0x2F` the way MercuryAPI documents, and streams once it is started.
    struct FakeModule {
        pending: std::collections::VecDeque<u8>,
        written: Arc<std::sync::Mutex<Vec<u8>>>,
        refuse: Option<(u8, u16)>,
        streams: Vec<Vec<u8>>,
        quiet_reads: usize,
    }

    impl FakeModule {
        fn new(written: &Arc<std::sync::Mutex<Vec<u8>>>) -> Self {
            Self {
                pending: std::collections::VecDeque::new(),
                written: Arc::clone(written),
                refuse: None,
                streams: Vec::new(),
                quiet_reads: 3,
            }
        }
    }

    impl io::Write for FakeModule {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.written
                .lock()
                .expect("the written bytes")
                .extend_from_slice(bytes);

            let length = usize::from(bytes[1]);
            let opcode = bytes[2];
            let body = &bytes[3..3 + length];
            let status = match self.refuse {
                Some((refused, status)) if refused == opcode => status,
                _ => 0x0000,
            };
            let echo: Vec<u8> = if opcode == 0x2F {
                vec![body[2]]
            } else {
                Vec::new()
            };
            self.pending.extend(answer(opcode, status, &echo));

            let started = opcode == 0x2F && body[2] == 0x01 && status == 0;
            if started {
                for frame in &self.streams {
                    self.pending.extend(frame.iter().copied());
                }
            }
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Read for FakeModule {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if !self.pending.is_empty() {
                let count = self.pending.len().min(buffer.len());
                for slot in buffer.iter_mut().take(count) {
                    *slot = self.pending.pop_front().expect("counted");
                }
                return Ok(count);
            }
            if self.quiet_reads > 0 {
                self.quiet_reads -= 1;
                thread::sleep(Duration::from_millis(1));
                return Err(io::Error::from(ErrorKind::TimedOut));
            }
            Err(io::Error::from(ErrorKind::BrokenPipe))
        }
    }

    fn starting_reader<P: PortFactory + 'static>(factory: P) -> Box<dyn ReaderProvider> {
        Box::new(
            ThingMagicReader::new(ReaderId::new("mat"), factory, StubDecoder)
                .with_start(StartSequence::gen2(crate::command::Region::Na))
                .with_backoff(Backoff {
                    first: Duration::from_millis(1),
                    max: Duration::from_secs(1),
                })
                .with_capacity(16),
        )
    }

    /// The bytes of the whole start sequence, as the command builders encode it.
    fn the_start_sequence() -> Vec<u8> {
        let mut bytes = Vec::new();
        for command in StartSequence::gen2(crate::command::Region::Na).commands() {
            let mut out = [0_u8; crate::frame::MAX_FRAME_LEN];
            bytes.extend_from_slice(command.encode(&mut out).expect("encode"));
        }
        bytes
    }

    #[tokio::test]
    async fn a_module_told_to_start_is_announced_when_it_accepts_and_its_reports_follow() {
        let written = Arc::new(std::sync::Mutex::new(Vec::new()));
        let record = Arc::clone(&written);
        let factory = move || -> io::Result<Port> {
            let mut module = FakeModule::new(&record);
            module.streams = vec![response(0x22, &[0x01, 0x02])];
            Ok(Box::new(module) as Port)
        };

        let mut receiver = starting_reader(factory).start();

        assert_eq!(receiver.recv().await, Some(ReaderEvent::Connected));
        let Some(ReaderEvent::Read(message)) = receiver.recv().await else {
            panic!("the stream's first report");
        };
        assert_eq!(message.chip, ChipId::new("0102"));
        assert_eq!(
            receiver.recv().await,
            Some(ReaderEvent::Disconnected {
                cause: Disconnection::Ended
            })
        );

        let sent = written.lock().expect("the written bytes").clone();
        let sequence = the_start_sequence();
        assert_eq!(
            &sent[..sequence.len()],
            &sequence[..],
            "stop, version, Gen2, region, read filter off, start, byte for byte"
        );
    }

    #[tokio::test]
    async fn a_module_that_refuses_a_step_is_not_announced_and_is_not_churned() {
        // A module that answers and will not take the region: it is there, and it is not
        // recording. Announcing it would close and reopen a gap on every attempt.
        let opens = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&opens);
        let written = Arc::new(std::sync::Mutex::new(Vec::new()));
        let record = Arc::clone(&written);
        let factory = move || -> io::Result<Port> {
            counter.fetch_add(1, Ordering::SeqCst);
            let mut module = FakeModule::new(&record);
            module.refuse = Some((0x97, 0x0105));
            Ok(Box::new(module) as Port)
        };

        let mut receiver = starting_reader(factory).start();
        for _ in 0..3 {
            assert_eq!(
                receiver.recv().await,
                Some(ReaderEvent::Disconnected {
                    cause: Disconnection::NotStarted
                }),
                "refused, so never connected"
            );
        }

        let sent = written.lock().expect("the written bytes").clone();
        let opcodes: Vec<u8> = decode_commands(&sent);
        assert_eq!(
            &opcodes[..4],
            &[0x2F, 0x03, 0x93, 0x97],
            "nothing is sent after the refusal"
        );
        assert!(
            !opcodes.windows(2).any(|pair| pair == [0x97, 0x9A]),
            "the read filter and the start never follow a refused region: {opcodes:02X?}"
        );
    }

    /// The opcode of every command frame in `bytes`.
    fn decode_commands(bytes: &[u8]) -> Vec<u8> {
        let mut opcodes = Vec::new();
        let mut rest = bytes;
        while rest.len() > 2 {
            let length = usize::from(rest[1]);
            opcodes.push(rest[2]);
            rest = &rest[(3 + length + 2).min(rest.len())..];
        }
        opcodes
    }

    #[tokio::test]
    async fn a_report_from_a_stream_left_running_is_kept_while_the_start_waits() {
        // The module goes on streaming into a broken link, so a reconnection can open on the
        // tail of the last session's stream. Those reports reached the host; they are
        // evidence, and they arrive before the new stream is announced.
        let written = Arc::new(std::sync::Mutex::new(Vec::new()));
        let record = Arc::clone(&written);
        let factory = move || -> io::Result<Port> {
            let mut module = FakeModule::new(&record);
            module.pending.extend(response(0x22, &[0xAA]));
            Ok(Box::new(module) as Port)
        };

        let mut receiver = starting_reader(factory).start();

        let Some(ReaderEvent::Read(message)) = receiver.recv().await else {
            panic!("the leftover report comes first");
        };
        assert_eq!(message.chip, ChipId::new("AA"));
        assert_eq!(receiver.recv().await, Some(ReaderEvent::Connected));
    }

    #[tokio::test]
    async fn the_lifecycle_is_reported_before_and_after_the_reads() {
        // One connection that delivers a frame and then breaks. The three events around it
        // are the whole of what this change adds: the consumer can now tell that the port
        // opened, produced, and died, rather than inferring the last from an absence.
        let script = vec![response(0x22, &[0x01])];
        let factory = move || -> io::Result<Port> {
            Ok(Box::new(ScriptedPort {
                chunks: script.clone(),
                index: 0,
            }) as Port)
        };

        let mut receiver = reader(factory).start();

        assert_eq!(
            receiver.recv().await.expect("the connection"),
            ReaderEvent::Connected,
            "a connection is announced before anything it carries"
        );
        assert!(matches!(
            receiver.recv().await.expect("the read"),
            ReaderEvent::Read(_)
        ));
        assert_eq!(
            receiver.recv().await.expect("the disconnection"),
            ReaderEvent::Disconnected {
                cause: Disconnection::Ended
            },
            "a port that dies mid-stream is reported as a connection that ended"
        );
    }

    #[test]
    fn why_a_port_would_not_open_is_logged_once_per_reason() {
        // An unplugged module is retried with backoff for as long as it stays unplugged, and a
        // line per retry would bury everything else in journald by lunchtime.
        let mut reader = ThingMagicReader::new(
            ReaderId::new("mat"),
            || -> io::Result<Port> { Err(io::Error::from(ErrorKind::NotFound)) },
            StubDecoder,
        );
        let missing = io::Error::new(ErrorKind::NotFound, "opening /dev/r: No such file");
        let refused = io::Error::new(ErrorKind::PermissionDenied, "opening /dev/r: not permitted");

        assert!(
            reader.report_open_failure(&missing),
            "the first reason is logged"
        );
        assert!(!reader.report_open_failure(&missing), "and not again");
        assert!(
            reader.report_open_failure(&refused),
            "a different reason is news"
        );
        assert!(
            reader.report_open_failure(&missing),
            "and so is the first one, back again"
        );
    }

    #[tokio::test]
    async fn a_port_that_never_opens_is_reported_as_not_opened() {
        // The other cause, and a different diagnosis: nothing was ever reachable. An
        // operator reading this at setup should look for a cable, not for a fault.
        let factory = move || -> io::Result<Port> { Err(io::Error::from(ErrorKind::NotFound)) };

        let mut receiver = reader(factory).start();
        for _ in 0..3 {
            assert_eq!(
                receiver.recv().await.expect("a report per attempt"),
                ReaderEvent::Disconnected {
                    cause: Disconnection::NotOpened
                }
            );
        }
    }

    #[tokio::test]
    async fn a_reconnection_is_announced_so_a_gap_can_be_closed() {
        // A port that fails once and then works. The `Connected` after the failure is what
        // ends the gap the failure opened — without it a reader that came back to an empty
        // field would stay in a gap until the next runner crossed.
        let attempts = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&attempts);
        let factory = move || -> io::Result<Port> {
            if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                return Err(io::Error::from(ErrorKind::NotFound));
            }
            Ok(Box::new(ScriptedPort {
                chunks: vec![response(0x22, &[0x07])],
                index: 0,
            }) as Port)
        };

        let mut receiver = reader(factory).start();
        assert_eq!(
            receiver.recv().await.expect("the failed attempt"),
            ReaderEvent::Disconnected {
                cause: Disconnection::NotOpened
            }
        );
        assert_eq!(
            receiver.recv().await.expect("the reconnection"),
            ReaderEvent::Connected
        );
    }

    /// A port that opens and ends at once, as a wrong device node or a loose cable does.
    fn a_port_that_ends_at_once() -> io::Result<Port> {
        Ok(Box::new(io::empty()) as Port)
    }

    #[tokio::test]
    async fn a_port_that_opens_and_ends_at_once_is_never_announced_as_connected() {
        // The 2026-09-13 review's reproduction: a port whose `read` returns `Ok(0)` produced 29
        // connections and 28 disconnections in two seconds, and every pair is two rows in an
        // append-only table. A connection that never proved a reader was on the other end is
        // not one, so the whole run is one outage and one gap.
        //
        // The ceiling is a second so that no scheduler pause, however long, can make one of
        // these connections look established by having lasted.
        let mut receiver: mpsc::Receiver<ReaderEvent> = Box::new(
            ThingMagicReader::new(ReaderId::new("mat"), a_port_that_ends_at_once, StubDecoder)
                .with_backoff(Backoff {
                    first: Duration::from_millis(1),
                    max: Duration::from_secs(1),
                })
                .with_capacity(16),
        )
        .start();

        for _ in 0..5 {
            assert_eq!(
                receiver.recv().await,
                Some(ReaderEvent::Disconnected {
                    cause: Disconnection::Ended
                }),
                "a port that ends before anything arrives is still in the outage"
            );
        }
    }

    #[test]
    fn a_flapping_port_backs_off_instead_of_retrying_at_the_floor() {
        // The other half of the same finding. `run` reset its failure count on every open, so a
        // port that opened and died stayed at the first delay forever. Here that is 10–20 ms,
        // which is twenty or more opens in the window below; backing off allows five.
        let opens = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&opens);
        let factory = move || -> io::Result<Port> {
            counter.fetch_add(1, Ordering::SeqCst);
            a_port_that_ends_at_once()
        };

        let receiver = Box::new(
            ThingMagicReader::new(ReaderId::new("mat"), factory, StubDecoder)
                .with_backoff(Backoff {
                    first: Duration::from_millis(20),
                    max: Duration::from_secs(2),
                })
                .with_capacity(256),
        )
        .start();

        thread::sleep(Duration::from_millis(400));
        let opened = opens.load(Ordering::SeqCst);
        drop(receiver);

        assert!(
            opened <= 6,
            "{opened} opens in 400 ms: the delay is not growing"
        );
    }

    #[tokio::test]
    async fn dropping_the_receiver_stops_the_reader() {
        let opens = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&opens);
        let factory = move || -> io::Result<Port> {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(ScriptedPort {
                chunks: vec![response(0x22, &[0x01])],
                index: 0,
            }) as Port)
        };

        let receiver = reader(factory).start();
        drop(receiver);

        // Give the thread room to notice, then confirm it stopped reopening.
        thread::sleep(Duration::from_millis(60));
        let settled = opens.load(Ordering::SeqCst);
        thread::sleep(Duration::from_millis(60));
        assert_eq!(opens.load(Ordering::SeqCst), settled);
    }

    #[test]
    fn the_empty_decoder_never_invents_a_read() {
        // The whole of its contract. A guessed parser would put a plausible, wrong chip id
        // into an append-only table; this one cannot, because it never writes to `out` —
        // and that is the difference that lets it exist in a crate refusing to ship a
        // decoder at all (ADR-0004).
        let mut decoder = UndecodedReports::new();
        let anchor = SessionAnchor::now();
        let mut out = Vec::new();

        for byte in 0..8_u8 {
            let frame = response(0x22, &[byte, 0xAB, 0xCD]);
            let Decoded::Frame { response, .. } = crate::frame::decode(&frame).expect("a frame")
            else {
                panic!("the fixture builds a whole frame");
            };
            decoder.decode(&response, &anchor, &mut out);
        }

        assert!(
            out.is_empty(),
            "an empty decoder that produced a read would be a fabricated one: {out:?}"
        );
        assert_eq!(
            decoder.frames(),
            8,
            "the count is what separates 'streaming and undecodable' from 'silent'"
        );
    }

    #[tokio::test]
    async fn a_reader_with_the_empty_decoder_still_reports_its_lifecycle() {
        // The reason to compose one before a decoder exists. No read can arrive, and the
        // connection edges — which are what a confirmed gap is made of — arrive anyway.
        let script = vec![response(0x22, &[0x01, 0x02])];
        let factory = move || -> io::Result<Port> {
            Ok(Box::new(ScriptedPort {
                chunks: script.clone(),
                index: 0,
            }) as Port)
        };

        let mut receiver: mpsc::Receiver<ReaderEvent> = Box::new(
            ThingMagicReader::new(ReaderId::new("mat"), factory, UndecodedReports::new())
                .with_backoff(Backoff {
                    first: Duration::from_millis(1),
                    max: Duration::from_millis(5),
                })
                .with_capacity(16),
        )
        .start();

        assert_eq!(receiver.recv().await, Some(ReaderEvent::Connected));
        // The frame the scripted port delivered produced no `Read`, so the next event is
        // the port ending — which is exactly the evidence M3a's third clause asks for.
        assert_eq!(
            receiver.recv().await,
            Some(ReaderEvent::Disconnected {
                cause: Disconnection::Ended
            }),
            "a decoder that reads nothing must not swallow the connection lifecycle too"
        );
    }

    #[test]
    fn backoff_grows_and_is_capped() {
        let backoff = Backoff {
            first: Duration::from_millis(100),
            max: Duration::from_secs(5),
        };

        assert!(backoff.delay(0, 1) <= Duration::from_millis(100));
        assert!(backoff.delay(20, 1) <= backoff.max);
        assert!(backoff.delay(20, 1) >= backoff.max / 2);

        // Never zero: a retry loop with no delay is a busy loop.
        for failures in 0..24 {
            assert!(backoff.delay(failures, u64::from(failures)) > Duration::ZERO);
        }
    }

    #[test]
    fn backoff_is_jittered_rather_than_identical() {
        let backoff = Backoff::default();
        let delays: Vec<Duration> = (0..16).map(|seed| backoff.delay(4, seed)).collect();
        let first = delays[0];
        assert!(
            delays.iter().any(|delay| *delay != first),
            "every delay was identical, so retries would stay in lockstep"
        );
    }

    #[test]
    fn the_session_anchor_captures_both_clocks() {
        let anchor = SessionAnchor::now();
        thread::sleep(Duration::from_millis(5));

        // The monotonic clock is what intervals are measured on; the wall clock only says
        // when the session began.
        assert!(anchor.opened_at.elapsed() >= Duration::from_millis(5));
        assert!(anchor.opened_at_utc.year() >= 2024);
    }
}
