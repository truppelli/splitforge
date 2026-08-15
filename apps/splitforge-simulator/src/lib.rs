//! # splitforge-simulator
//!
//! Synthetic readers and race scenarios for development and automated tests.
//!
//! ## Boundaries
//!
//! - **May depend on:** splitforge-domain, splitforge-reader
//! - **Must never depend on:** splitforge-llrp
//!
//! These rules come from ADR-0001 and are tabulated in `docs/architecture.md`.
//! They are enforced by `crates/splitforge-testkit/tests/dependency_rules.rs`, not left to
//! review (ADR-0012).
//!
//! ## Why this is a library, not a test helper
//!
//! [`SimulatedReader`] implements [`ReaderProvider`], the same port an LLRP adapter will
//! implement in Milestone 3. Nothing downstream — not the journal, not the engine, not the
//! CLI — can tell a simulated reader from a real one. That is the whole point: it means the
//! data path is proven before hardware is introduced as a variable, and it means the
//! simulator keeps working as a regression harness afterwards.
//!
//! The package builds a binary too (a self-contained demo race), but the library is the
//! part other crates use.
//!
//! ## What it does not simulate
//!
//! It does not model antenna physics, tag collision, reader duty cycles, or protocol
//! framing. It models the two things the data path actually has to survive: **many reads
//! per crossing**, and **timestamps that come from somewhere other than this process**.
//! Claiming more would be claiming hardware validation this project has not done — see
//! `docs/hardware-support.md`.

// No panicking on any path reachable during an event: a corrupt frame, a missing
// field, or an out-of-range value must become an error the caller can act on, never a
// timer that stops mid-race. Test code is exempt, where panicking on the unexpected is
// the point. See CONTRIBUTING.md, "Code standards".
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use splitforge_domain::{ChipId, ReaderId};
use splitforge_reader::{ReaderMessage, ReaderProvider, ReaderTimestamp};
use time::{Duration, OffsetDateTime};
use tokio::sync::mpsc;

/// SplitMix64.
///
/// Hand-rolled rather than pulled from `rand`, deliberately. `rand`'s output is explicitly
/// not stable across versions, so a dependency bump would silently change every fixture
/// this crate generates. Determinism is a property the tests depend on, so it is written
/// down here where a version bump cannot move it.
#[derive(Debug, Clone, Copy)]
struct Rng(u64);

impl Rng {
    const fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A value in `low..=high`. Returns `low` if the range is inverted.
    fn range(&mut self, low: i64, high: i64) -> i64 {
        let Some(span) = high.checked_sub(low).filter(|span| *span > 0) else {
            return low;
        };
        let span = u64::try_from(span).unwrap_or(u64::MAX).saturating_add(1);
        let offset = i64::try_from(self.next_u64() % span).unwrap_or(0);
        low.saturating_add(offset)
    }
}

/// The shape of the read burst a single crossing produces.
///
/// A chip does not produce one read as it passes an antenna; it produces a stream of them
/// as it enters, crosses, and leaves the field. Reproducing that is the point of the
/// simulator — deduplicating it correctly is the point of the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BurstProfile {
    /// Fewest reads a crossing produces.
    pub min_reads: u32,
    /// Most reads a crossing produces.
    pub max_reads: u32,
    /// Shortest gap between consecutive reads, in milliseconds.
    pub min_spacing_ms: i64,
    /// Longest gap between consecutive reads, in milliseconds.
    pub max_spacing_ms: i64,
    /// Signal strength at closest approach, in dBm.
    pub peak_rssi_dbm: i16,
    /// How far below the peak the edges of the burst fall, in dB.
    pub rssi_spread_db: i16,
    /// Random variation applied to each read's signal strength, in dB.
    pub rssi_jitter_db: i16,
}

impl Default for BurstProfile {
    /// A runner clearing a mat: roughly one to two seconds of reads, peaking at the line.
    fn default() -> Self {
        Self {
            min_reads: 14,
            max_reads: 46,
            min_spacing_ms: 22,
            max_spacing_ms: 58,
            peak_rssi_dbm: -47,
            rssi_spread_db: 26,
            rssi_jitter_db: 2,
        }
    }
}

/// One chip passing one antenna at one instant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Crossing {
    /// The chip.
    pub chip: ChipId,
    /// The antenna it passes, when the deployment distinguishes them.
    pub antenna: Option<u16>,
    /// The instant of closest approach — the peak of the burst, not its first read.
    pub at: OffsetDateTime,
}

/// One synthetic read, ready to be emitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptedRead {
    /// When the reader detects the chip. Drives pacing and ordering.
    pub at: OffsetDateTime,
    /// What the reader reports.
    pub message: ReaderMessage,
}

/// A planned race: who crosses which antenna, when, and how noisily.
#[derive(Debug, Clone)]
pub struct Scenario {
    reader: ReaderId,
    profile: BurstProfile,
    seed: u64,
    crossings: Vec<Crossing>,
}

impl Scenario {
    /// Starts an empty scenario.
    ///
    /// `seed` fixes every random choice the scenario makes. Two scenarios built with the
    /// same seed and the same crossings produce byte-identical scripts.
    #[must_use]
    pub fn new(reader: ReaderId, profile: BurstProfile, seed: u64) -> Self {
        Self {
            reader,
            profile,
            seed,
            crossings: Vec::new(),
        }
    }

    /// Adds a crossing.
    #[must_use]
    pub fn crossing(mut self, chip: ChipId, antenna: Option<u16>, at: OffsetDateTime) -> Self {
        self.crossings.push(Crossing { chip, antenna, at });
        self
    }

    /// The reader this scenario runs on.
    #[must_use]
    pub fn reader(&self) -> &ReaderId {
        &self.reader
    }

    /// How many crossings the scenario contains.
    #[must_use]
    pub fn len(&self) -> usize {
        self.crossings.len()
    }

    /// Whether the scenario contains no crossings.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.crossings.is_empty()
    }

    /// Expands every crossing into its burst of reads, in chronological order.
    #[must_use]
    pub fn script(&self) -> Vec<ScriptedRead> {
        let mut rng = Rng::new(self.seed);
        let mut script = Vec::new();

        // Crossings are expanded in a fixed order — sorted by instant, then chip — so the
        // script does not depend on the order they were added.
        let mut ordered = self.crossings.clone();
        ordered.sort_by(|left, right| left.at.cmp(&right.at).then(left.chip.cmp(&right.chip)));

        for crossing in &ordered {
            script.extend(self.burst(crossing, &mut rng));
        }
        script.sort_by(|left, right| {
            left.at
                .cmp(&right.at)
                .then(left.message.chip.cmp(&right.message.chip))
        });
        script
    }

    /// Expands one crossing into the stream of reads it produces.
    fn burst(&self, crossing: &Crossing, rng: &mut Rng) -> Vec<ScriptedRead> {
        let count = rng.range(
            i64::from(self.profile.min_reads.max(1)),
            i64::from(self.profile.max_reads.max(1)),
        );
        let count = usize::try_from(count).unwrap_or(1).max(1);

        // Offsets first, so the burst can be centred on the crossing instant afterwards.
        let mut offsets_ms = Vec::with_capacity(count);
        let mut elapsed = 0_i64;
        for _ in 0..count {
            offsets_ms.push(elapsed);
            elapsed = elapsed.saturating_add(
                rng.range(self.profile.min_spacing_ms, self.profile.max_spacing_ms),
            );
        }
        // `Crossing::at` is closest approach, which is the peak of the burst rather than
        // its first read. A mat that reads two seconds early would otherwise shift every
        // recorded time earlier by a second, and the fixture's expected times would be
        // describing the antenna's reach rather than the runner.
        let peak_index = (count - 1) / 2;
        let centre = offsets_ms[peak_index];

        let span = i64::try_from(count - 1).unwrap_or(1).max(1);
        offsets_ms
            .iter()
            .enumerate()
            .map(|(index, offset_ms)| {
                let position = i64::try_from(index).unwrap_or(0);
                // Signal ramps to the peak at closest approach and falls away again.
                let from_centre = (2 * position - span).abs();
                let falloff = i64::from(self.profile.rssi_spread_db) * from_centre / span;
                let jitter = rng.range(
                    -i64::from(self.profile.rssi_jitter_db),
                    i64::from(self.profile.rssi_jitter_db),
                );
                let rssi = i64::from(self.profile.peak_rssi_dbm) - falloff + jitter;
                let rssi_dbm = i16::try_from(rssi).unwrap_or(i16::MIN);

                let at = crossing.at + Duration::milliseconds(offset_ms - centre);
                ScriptedRead {
                    at,
                    message: ReaderMessage {
                        source: self.reader.clone(),
                        antenna: crossing.antenna,
                        chip: crossing.chip.clone(),
                        timestamp: ReaderTimestamp::Utc { at },
                        rssi_dbm: Some(rssi_dbm),
                        raw_payload: synthetic_frame(
                            &crossing.chip,
                            crossing.antenna,
                            rssi_dbm,
                            at,
                        ),
                    },
                }
            })
            .collect()
    }
}

/// Builds the bytes a synthetic reader "sent".
///
/// Deliberately **not** an LLRP frame. This project does not ship vendor protocol code
/// until a physical device has been tested against it, and a plausible-looking fake would
/// be worse than an obvious one. The payload exists so that the journal's payload hash and
/// forensic-retention path are exercised with real, varying bytes.
fn synthetic_frame(
    chip: &ChipId,
    antenna: Option<u16>,
    rssi_dbm: i16,
    at: OffsetDateTime,
) -> Vec<u8> {
    let micros = at.unix_timestamp_nanos().div_euclid(1_000);
    let micros = i64::try_from(micros).unwrap_or(i64::MAX);

    let mut frame = Vec::with_capacity(48);
    frame.extend_from_slice(b"SFSIM\x01");
    frame.extend_from_slice(&antenna.unwrap_or(0).to_be_bytes());
    frame.extend_from_slice(&rssi_dbm.to_be_bytes());
    frame.extend_from_slice(&micros.to_be_bytes());
    let chip = chip.as_str().as_bytes();
    frame.extend_from_slice(&u16::try_from(chip.len()).unwrap_or(u16::MAX).to_be_bytes());
    frame.extend_from_slice(chip);
    frame
}

/// How fast a scenario plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Speed {
    /// Emit every read as fast as the consumer accepts it.
    ///
    /// The default, and what tests use: a four-lap criterium finishes in milliseconds, and
    /// the reads still carry their real scenario timestamps, so nothing downstream can tell.
    #[default]
    Immediate,
    /// Emit in elapsed time, `factor` times faster than the scenario's own clock.
    ///
    /// `factor: 1` is wall-clock speed. Used for demos and for the abrupt-shutdown tests,
    /// which need a process that is genuinely mid-write when it is killed.
    Accelerated {
        /// Speed multiplier. Zero is treated as one.
        factor: u32,
    },
}

/// A reader that produces a scripted race instead of listening to hardware.
#[derive(Debug)]
pub struct SimulatedReader {
    reader: ReaderId,
    script: Vec<ScriptedRead>,
    speed: Speed,
    buffer: usize,
}

impl SimulatedReader {
    /// Default channel depth.
    ///
    /// Small on purpose. A deep buffer would hide back-pressure, and back-pressure between
    /// a reader and a journal doing `synchronous = FULL` writes is exactly the behavior
    /// worth surfacing early.
    pub const DEFAULT_BUFFER: usize = 64;

    /// Builds a reader that will play `scenario`.
    #[must_use]
    pub fn new(scenario: &Scenario, speed: Speed) -> Self {
        Self {
            reader: scenario.reader().clone(),
            script: scenario.script(),
            speed,
            buffer: Self::DEFAULT_BUFFER,
        }
    }

    /// Overrides the channel depth.
    #[must_use]
    pub const fn with_buffer(mut self, buffer: usize) -> Self {
        self.buffer = buffer;
        self
    }

    /// How many reads the reader will emit.
    #[must_use]
    pub fn scripted_reads(&self) -> usize {
        self.script.len()
    }

    /// The script this reader will play.
    #[must_use]
    pub fn script(&self) -> &[ScriptedRead] {
        &self.script
    }
}

impl ReaderProvider for SimulatedReader {
    fn reader_id(&self) -> ReaderId {
        self.reader.clone()
    }

    fn start(self: Box<Self>) -> mpsc::Receiver<ReaderMessage> {
        let (sender, receiver) = mpsc::channel(self.buffer.max(1));
        tokio::spawn(async move {
            let mut previous: Option<OffsetDateTime> = None;
            for scripted in self.script {
                if let Speed::Accelerated { factor } = self.speed {
                    let factor = i32::try_from(factor.max(1)).unwrap_or(i32::MAX);
                    let gap = previous.map_or(Duration::ZERO, |prev| scripted.at - prev);
                    let scaled = gap / factor;
                    if scaled > Duration::ZERO {
                        tokio::time::sleep(scaled.unsigned_abs()).await;
                    }
                    previous = Some(scripted.at);
                }
                // A closed channel means the consumer is gone. Stopping is correct: a real
                // adapter with nowhere to deliver reads should stop reading, not buffer
                // the race into memory.
                if sender.send(scripted.message).await.is_err() {
                    break;
                }
            }
        });
        receiver
    }
}

/// The instant a reader claims it detected a chip, when it claims one at all.
#[must_use]
pub fn detection_time(message: &ReaderMessage) -> Option<OffsetDateTime> {
    match message.timestamp {
        ReaderTimestamp::Utc { at } => Some(at),
        ReaderTimestamp::Uptime { .. } | ReaderTimestamp::Absent => None,
    }
}

/// When the device received a message, and what its monotonic clock read at that moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Receipt {
    /// Device wall-clock time.
    pub received_at: OffsetDateTime,
    /// Device monotonic time, in nanoseconds since a simulated boot.
    pub monotonic_ns: u64,
}

/// The delay between a reader detecting a chip and this process receiving the report.
///
/// Simulated rather than measured, because a test that stamps `received_at` from the real
/// wall clock is not reproducible — and the gap between reader time and receipt time is
/// precisely what `docs/clock-and-time-discipline.md` is about. Keeping it deterministic is
/// what lets a restart test compare two derivations byte for byte.
#[derive(Debug, Clone)]
pub struct Transport {
    latency_ms: i64,
    jitter_ms: i64,
    rng: Rng,
    boot: Option<OffsetDateTime>,
}

impl Transport {
    /// Typical LAN round trip for a reader report: a few milliseconds.
    ///
    /// Two orders of magnitude below [`splitforge_reader::DEFAULT_MAX_OFFSET_MS`], so
    /// simulated reader timestamps stay trusted — which is the case worth exercising, since
    /// it is the one a correctly configured deployment is in.
    pub const DEFAULT_LATENCY_MS: i64 = 4;
    /// Variation around that latency.
    pub const DEFAULT_JITTER_MS: i64 = 3;

    /// Builds a transport with the default latency profile.
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self {
            latency_ms: Self::DEFAULT_LATENCY_MS,
            jitter_ms: Self::DEFAULT_JITTER_MS,
            rng: Rng::new(seed),
            boot: None,
        }
    }

    /// Overrides the latency profile.
    #[must_use]
    pub const fn with_latency(mut self, latency_ms: i64, jitter_ms: i64) -> Self {
        self.latency_ms = latency_ms;
        self.jitter_ms = jitter_ms;
        self
    }

    /// Computes when a message sent at `sent_at` arrives.
    pub fn receipt(&mut self, sent_at: OffsetDateTime) -> Receipt {
        // The simulated device booted an hour before the first read it ever saw, so
        // monotonic values are plausible and always positive.
        let boot = *self.boot.get_or_insert(sent_at - Duration::hours(1));
        let delay = self.latency_ms + self.rng.range(-self.jitter_ms, self.jitter_ms);
        let received_at = sent_at + Duration::milliseconds(delay.max(0));
        let monotonic_ns = u64::try_from((received_at - boot).whole_nanoseconds()).unwrap_or(0);
        Receipt {
            received_at,
            monotonic_ns,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    const START: OffsetDateTime = datetime!(2026-04-11 08:00:00 UTC);

    fn scenario(seed: u64) -> Scenario {
        Scenario::new(ReaderId::new("mat"), BurstProfile::default(), seed)
            .crossing(ChipId::new("AAAA"), Some(1), START)
            .crossing(ChipId::new("BBBB"), Some(1), START + Duration::seconds(3))
            .crossing(ChipId::new("AAAA"), Some(2), START + Duration::minutes(18))
    }

    #[test]
    fn the_same_seed_produces_the_same_script() {
        assert_eq!(scenario(7).script(), scenario(7).script());
    }

    #[test]
    fn a_different_seed_produces_a_different_script() {
        assert_ne!(scenario(7).script(), scenario(8).script());
    }

    #[test]
    fn the_order_crossings_are_added_does_not_change_the_script() {
        let forward = scenario(11).script();
        let backward = Scenario::new(ReaderId::new("mat"), BurstProfile::default(), 11)
            .crossing(ChipId::new("AAAA"), Some(2), START + Duration::minutes(18))
            .crossing(ChipId::new("BBBB"), Some(1), START + Duration::seconds(3))
            .crossing(ChipId::new("AAAA"), Some(1), START)
            .script();
        assert_eq!(forward, backward);
    }

    #[test]
    fn every_crossing_produces_many_reads() {
        let profile = BurstProfile::default();
        let script = scenario(3).script();
        assert!(
            script.len() >= 3 * profile.min_reads as usize,
            "three crossings produced only {} reads",
            script.len()
        );
        assert!(script.len() <= 3 * profile.max_reads as usize);
    }

    #[test]
    fn the_script_is_chronological() {
        let script = scenario(5).script();
        assert!(script.windows(2).all(|pair| pair[0].at <= pair[1].at));
    }

    #[test]
    fn the_strongest_read_of_a_burst_lands_on_the_crossing_instant() {
        // Otherwise the recorded time describes the antenna's reach rather than the runner.
        let script = Scenario::new(ReaderId::new("mat"), BurstProfile::default(), 42)
            .crossing(ChipId::new("AAAA"), Some(1), START)
            .script();

        let peak = script
            .iter()
            .max_by_key(|read| read.message.rssi_dbm.unwrap_or(i16::MIN))
            .expect("a burst is never empty");
        let error = (peak.at - START).abs();
        assert!(
            error <= Duration::milliseconds(120),
            "peak landed {error} from the crossing"
        );
    }

    #[test]
    fn signal_strength_rises_to_the_middle_of_a_burst_and_falls_away() {
        let script = Scenario::new(ReaderId::new("mat"), BurstProfile::default(), 19)
            .crossing(ChipId::new("AAAA"), Some(1), START)
            .script();
        let rssi = |index: usize| script[index].message.rssi_dbm.expect("rssi present");
        let middle = script.len() / 2;

        assert!(rssi(0) < rssi(middle), "the approach must be weaker");
        assert!(
            rssi(script.len() - 1) < rssi(middle),
            "the departure must be weaker"
        );
    }

    #[test]
    fn the_reader_timestamp_matches_the_scripted_instant() {
        for read in scenario(2).script() {
            assert_eq!(read.message.timestamp, ReaderTimestamp::Utc { at: read.at });
        }
    }

    #[test]
    fn each_read_carries_distinct_bytes() {
        // A constant payload would make the journal's payload hash meaningless.
        let script = scenario(4).script();
        let mut payloads: Vec<&[u8]> = script
            .iter()
            .map(|read| read.message.raw_payload.as_slice())
            .collect();
        payloads.sort_unstable();
        let total = payloads.len();
        payloads.dedup();
        assert_eq!(payloads.len(), total, "payloads must not repeat");
    }

    #[test]
    fn transport_delay_is_deterministic_and_never_negative() {
        let mut left = Transport::new(1);
        let mut right = Transport::new(1);
        for step in 0..50 {
            let sent = START + Duration::seconds(step);
            let first = left.receipt(sent);
            assert_eq!(first, right.receipt(sent));
            assert!(first.received_at >= sent, "a report cannot arrive early");
            assert!(first.monotonic_ns > 0);
        }
    }

    #[test]
    fn transport_delay_stays_far_inside_the_trust_threshold() {
        let mut transport = Transport::new(99);
        for step in 0..200 {
            let sent = START + Duration::seconds(step);
            let delay = (transport.receipt(sent).received_at - sent).whole_milliseconds();
            assert!(
                delay < i128::from(splitforge_reader::DEFAULT_MAX_OFFSET_MS),
                "a simulated LAN must not look like a broken reader clock"
            );
        }
    }

    #[tokio::test]
    async fn a_simulated_reader_delivers_its_whole_script() {
        let scenario = scenario(13);
        let reader = SimulatedReader::new(&scenario, Speed::Immediate);
        let expected = reader.scripted_reads();

        let mut receiver = Box::new(reader).start();
        let mut received = 0_usize;
        while receiver.recv().await.is_some() {
            received += 1;
        }
        assert_eq!(received, expected);
    }

    #[tokio::test]
    async fn dropping_the_consumer_stops_the_reader() {
        let scenario = scenario(17);
        let receiver = Box::new(SimulatedReader::new(&scenario, Speed::Immediate)).start();
        drop(receiver);
        // Nothing to assert beyond "this returns": the task must not panic or wedge when
        // it finds nowhere to deliver.
        tokio::task::yield_now().await;
    }

    #[test]
    fn detection_time_is_absent_when_the_reader_supplied_none() {
        let mut message = scenario(1).script()[0].message.clone();
        assert!(detection_time(&message).is_some());
        message.timestamp = ReaderTimestamp::Absent;
        assert!(detection_time(&message).is_none());
        message.timestamp = ReaderTimestamp::Uptime { micros: 42 };
        assert!(detection_time(&message).is_none());
    }
}
