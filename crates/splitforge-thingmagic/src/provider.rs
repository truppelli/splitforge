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
//! Most of that gap is now closed. Opcodes and search flags are in [`crate::command`], and the
//! report's field *order* — read count, RSSI, antenna, frequency, timestamp, phase, protocol,
//! data, GPIO, then the EPC — is recorded in `vendor-documents.md`, taken from the parser
//! itself. **What is still missing is which bit selects which field.** The
//! `TMR_TRD_METADATA_FLAG_*` values live in a header the archived mirror carries only a 2009
//! copy of, and that copy contains none of the modern symbols. Without them the field order is
//! a sequence with no way to know which members of it are present.
//!
//! So the seam is a trait, and the decision is
//! [ADR-0004](../../../docs/adr/0004-llrp-first-reader-adapter.md)'s: *do not start from
//! protocol documentation alone*. Inventing a byte layout that happens to compile would
//! produce a parser that is internally consistent, externally wrong, and fully tested —
//! which is worse than no parser at all, because it looks finished.
//!
//! Everything around that one seam is real and tested: opening, reading, reassembly,
//! resynchronization, bounded jittered reconnect, and the counters that make loss visible.

use std::io::{ErrorKind, Read};
use std::thread;
use std::time::{Duration, Instant};

use splitforge_domain::ReaderId;
use splitforge_reader::{ReaderEvent, ReaderMessage, ReaderProvider};
use time::OffsetDateTime;
use tokio::sync::mpsc;

use crate::frame::Response;
use crate::port::{Port, PortFactory};
use crate::reassembly::Reassembler;

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
        }
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
    /// **The connection edges are announced on the same channel as the reads**
    /// ([ADR-0027](../../../docs/adr/0027-a-reader-reports-connection-events-on-the-read-channel.md)),
    /// which is what lets the consumer record a gap that cannot start before a read taken
    /// inside it.
    ///
    /// They are *edges*, not a level. A port that has been missing for an hour produces one
    /// [`ReaderEvent::Disconnected`] rather than one per retry — a disconnection repeated
    /// every backoff interval is a level wearing an event's clothes, and it would say the
    /// reader disconnected two hundred times when it disconnected once.
    fn run(&mut self, sender: &mpsc::Sender<ReaderEvent>) {
        let mut failures: u32 = 0;
        let mut attempt: u64 = 0;
        // Whether the consumer has already been told the transport is down.
        //
        // It guards only the failed-open path below, and is never cleared: every route back to
        // a working port runs through `pump`, which announces its own ending unconditionally.
        // So a connection that comes up and dies again is always reported, and a port that has
        // simply never appeared is reported once.
        let mut announced_down = false;

        loop {
            attempt += 1;

            match self.factory.open() {
                Ok(port) => {
                    failures = 0;
                    if sender.blocking_send(ReaderEvent::Connected).is_err() {
                        return;
                    }
                    match self.pump(port, sender) {
                        // The consumer is gone. A reader with nowhere to deliver should stop
                        // reading rather than buffer a race into memory.
                        Ended::Consumer => return,
                        // `pump` announced it, from where the error actually was.
                        Ended::Port => announced_down = true,
                    }
                }
                Err(error) => {
                    failures = failures.saturating_add(1);
                    if !announced_down {
                        announced_down = true;
                        // A port that will not open is a disconnection the operating system
                        // reported, which is exactly the claim this event is allowed to make.
                        // On a first start it is also the honest one: a module that is not
                        // there has not gone quiet, it is absent.
                        if sender
                            .blocking_send(ReaderEvent::Disconnected {
                                detail: Some(error.to_string()),
                            })
                            .is_err()
                        {
                            return;
                        }
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

    /// Reads one connection to its end.
    ///
    /// **Announces the disconnection itself**, rather than returning a reason for [`Self::run`]
    /// to announce. The event then enters the channel at the point in the sequence where the
    /// port actually failed — immediately behind the last read that connection delivered, and
    /// never ahead of one still queued.
    fn pump(&mut self, mut port: Port, sender: &mpsc::Sender<ReaderEvent>) -> Ended {
        // A fresh connection starts from no partial frame. Bytes held over from the previous
        // one would splice two sessions into a frame that was never transmitted.
        let mut reassembler = Reassembler::new();
        let anchor = SessionAnchor::now();
        let mut chunk = [0_u8; 512];
        let mut reads: Vec<ReaderMessage> = Vec::new();

        loop {
            let count = match port.read(&mut chunk) {
                Ok(0) => return Self::announce_down(sender, "the port reported end of stream"),
                Ok(count) => count,
                // A timeout means the module had nothing to say, which during a race is most
                // of the time. Treating it as a disconnection would reopen the port between
                // every pair of runners — and, now that a disconnection is written down as
                // evidence, would manufacture a confirmed gap out of a quiet minute.
                Err(error) if error.kind() == ErrorKind::TimedOut => continue,
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(error) => return Self::announce_down(sender, &error.to_string()),
            };

            let decoder = &mut self.decoder;
            reassembler.feed(&chunk[..count], |response| {
                decoder.decode(response, &anchor, &mut reads);
            });

            for message in reads.drain(..) {
                // Blocking is the backpressure: if the journal cannot keep up, the right
                // response is to stop taking bytes off the port, not to grow a queue.
                if sender.blocking_send(ReaderEvent::Read(message)).is_err() {
                    return Ended::Consumer;
                }
            }
        }
    }

    /// Says the port died, behind everything it delivered before dying.
    fn announce_down(sender: &mpsc::Sender<ReaderEvent>, detail: &str) -> Ended {
        if sender
            .blocking_send(ReaderEvent::Disconnected {
                detail: Some(detail.to_owned()),
            })
            .is_err()
        {
            return Ended::Consumer;
        }
        Ended::Port
    }
}

/// Why one connection ended.
///
/// Two outcomes that a `bool` used to carry and that call for opposite responses: reopen the
/// port, or stop reading it. Naming them keeps the caller from having to remember which way
/// round `true` went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ended {
    /// The port stopped delivering. Reconnecting is the right response.
    Port,
    /// The consumer dropped the channel. There is nobody left to deliver to, or to tell.
    Consumer,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crc::crc16;
    use crate::frame::{RESPONSE_HEADER_LEN, SOH};
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

    /// A port that yields scripted chunks and then fails, as often as the script says.
    struct ScriptedPort {
        chunks: Vec<Vec<u8>>,
        index: usize,
    }

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

    /// The next read, stepping over the connection events around it.
    ///
    /// For the tests whose subject is the *reads* — reassembly, resynchronization, reconnect.
    /// The tests whose subject is the connection match on the events themselves.
    async fn next_read(receiver: &mut mpsc::Receiver<ReaderEvent>) -> Option<ReaderMessage> {
        while let Some(event) = receiver.recv().await {
            if let ReaderEvent::Read(message) = event {
                return Some(message);
            }
        }
        None
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
        let message = next_read(&mut receiver).await.expect("one read");
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
        let message = next_read(&mut receiver).await.expect("one read");
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
            let message = next_read(&mut receiver)
                .await
                .expect("a read per connection");
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
        let message = next_read(&mut receiver)
            .await
            .expect("a read once the port appears");
        assert_eq!(message.chip, ChipId::new("FF"));
        assert!(attempts.load(Ordering::SeqCst) >= 4);
    }

    #[tokio::test]
    async fn a_timeout_is_not_a_disconnection() {
        // A port that times out twice, then delivers. The claim is now checkable directly
        // rather than through an open count: nothing may say `Disconnected` between the
        // connection opening and the read arriving. A timeout that produced one would write a
        // *confirmed* gap into the evidence for a checkpoint that was merely quiet.
        let opens = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&opens);

        struct TimeoutThenData {
            remaining: usize,
            frame: Vec<u8>,
            sent: bool,
        }

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
        assert_eq!(
            receiver.recv().await,
            Some(ReaderEvent::Connected),
            "the connection is announced before anything it carries"
        );
        assert_eq!(opens.load(Ordering::SeqCst), 1);

        match receiver.recv().await.expect("the read after the timeouts") {
            ReaderEvent::Read(message) => assert_eq!(message.chip, ChipId::new("5A")),
            other => panic!("two timeouts must not disconnect the port, but got {other:?}"),
        }
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

    #[tokio::test]
    async fn a_dead_port_is_announced_behind_the_reads_it_delivered() {
        // The property the whole decision rests on (ADR-0027). One connection delivers a read
        // and then dies; the disconnection must arrive *after* that read, because a consumer
        // that saw it first would open a gap starting before a read taken inside it.
        let script = vec![response(0x22, &[0xAB, 0xCD])];
        let factory = move || -> io::Result<Port> {
            Ok(Box::new(ScriptedPort {
                chunks: script.clone(),
                index: 0,
            }) as Port)
        };

        let mut receiver = reader(factory).start();

        assert_eq!(receiver.recv().await, Some(ReaderEvent::Connected));
        match receiver.recv().await.expect("the read") {
            ReaderEvent::Read(message) => assert_eq!(message.chip, ChipId::new("ABCD")),
            other => panic!("the read must precede the disconnection, but got {other:?}"),
        }
        match receiver.recv().await.expect("the disconnection") {
            // The wording is the operating system's and nothing parses it — that it is
            // *present* is the claim, because it is what a human reads off the gap later.
            ReaderEvent::Disconnected { detail } => {
                assert!(detail.is_some(), "a disconnection carries what the OS said");
            }
            other => panic!("a broken port must announce itself, but got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_port_that_will_not_open_says_so_once_rather_than_once_per_retry() {
        // A cable left out for an hour disconnected once. Re-announcing it every backoff
        // interval would turn an edge into a level and claim hundreds of outages.
        let factory = || -> io::Result<Port> { Err(io::Error::from(ErrorKind::NotFound)) };

        let mut receiver = reader(factory).start();
        assert!(matches!(
            receiver.recv().await,
            Some(ReaderEvent::Disconnected { .. })
        ));

        // Many backoff intervals at 1–5 ms. Anything arriving in that window is a repeat.
        thread::sleep(Duration::from_millis(80));
        assert!(
            receiver.try_recv().is_err(),
            "a port that is still missing has nothing new to say"
        );
    }

    #[tokio::test]
    async fn coming_back_is_announced_so_the_gap_can_be_closed() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&attempts);
        let factory = move || -> io::Result<Port> {
            if counter.fetch_add(1, Ordering::SeqCst) < 2 {
                return Err(io::Error::from(ErrorKind::NotFound));
            }
            Ok(Box::new(ScriptedPort {
                chunks: vec![response(0x22, &[0x7E])],
                index: 0,
            }) as Port)
        };

        let mut receiver = reader(factory).start();

        assert!(matches!(
            receiver.recv().await,
            Some(ReaderEvent::Disconnected { .. })
        ));
        // Without this the consumer could only learn the reader was back by seeing a read —
        // which never comes at a checkpoint nobody is crossing, leaving a gap open over a
        // reader that recovered.
        assert_eq!(receiver.recv().await, Some(ReaderEvent::Connected));
        match receiver
            .recv()
            .await
            .expect("the read once the port appears")
        {
            ReaderEvent::Read(message) => assert_eq!(message.chip, ChipId::new("7E")),
            other => panic!("expected the read, got {other:?}"),
        }
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
