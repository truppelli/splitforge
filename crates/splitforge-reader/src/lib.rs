//! # splitforge-reader
//!
//! Reader provider port and normalized reader messages.
//!
//! ## Boundaries
//!
//! - **May depend on:** splitforge-domain
//! - **Must never depend on:** any specific reader protocol crate
//!
//! These rules come from ADR-0001 and are tabulated in `docs/architecture.md`.
//! They are enforced by `crates/splitforge-testkit/tests/dependency_rules.rs`, not left to
//! review (ADR-0012).
//!
//! ## What this crate is for
//!
//! Everything upstream of the timing engine funnels through [`ReaderMessage`] and
//! [`ReaderProvider`]. An LLRP reader, a serial timing box, a barcode scanner, and the
//! simulator all become the same shape here, which is what lets the engine stay ignorant
//! of protocols (ADR-0004).
//!
//! This crate also owns the **timestamp trust decision** — the guardrails around
//! "reader timestamps are authoritative". See [`Ingest`] and
//! `docs/clock-and-time-discipline.md`.

// No panicking on any path reachable during an event: a corrupt frame, a missing
// field, or an out-of-range value must become an error the caller can act on, never a
// timer that stops mid-race. Test code is exempt, where panicking on the unexpected is
// the point. See CONTRIBUTING.md, "Code standards".
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use serde::{Deserialize, Serialize};
use splitforge_domain::{
    ChipId, DeviceClockState, FallbackReason, RawRead, RawReadId, ReaderId, TimestampSource,
};
use time::OffsetDateTime;
use tokio::sync::mpsc;

/// What a reader said about when a read happened.
///
/// LLRP readers report *either* a UTC timestamp or an uptime, and which one arrives tells
/// you a great deal about the reader's state. Modelling them as distinct cases stops an
/// uptime value from ever being interpreted as a date in 1970.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReaderTimestamp {
    /// The reader believes it knows UTC. It may still be wrong.
    Utc {
        /// The reader's claimed time.
        #[serde(with = "time::serde::rfc3339")]
        at: OffsetDateTime,
    },
    /// The reader has no UTC clock and reported time since boot instead.
    ///
    /// Monotonic and high resolution, so still excellent for *relative* timing within one
    /// reader session — it simply cannot be placed on a calendar without an anchor.
    Uptime {
        /// Microseconds since the reader booted.
        micros: u64,
    },
    /// The reader supplied no timestamp at all.
    Absent,
}

/// One normalized read, before any SplitForge policy has been applied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReaderMessage {
    /// Which reader produced it.
    pub source: ReaderId,
    /// Antenna number, when reported.
    pub antenna: Option<u16>,
    /// The chip identifier.
    pub chip: ChipId,
    /// What the reader said about the time.
    pub timestamp: ReaderTimestamp,
    /// Signal strength, when reported.
    pub rssi_dbm: Option<i16>,
    /// The bytes the reader sent.
    pub raw_payload: Vec<u8>,
}

/// How far a reader's own clock is trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimestampTrust {
    /// Always use the reader's UTC timestamp when it supplies one.
    Trusted,
    /// Never use it. Always fall back to device receipt time.
    Untrusted,
    /// Use it unless the measured offset exceeds the configured threshold.
    #[default]
    Auto,
}

/// Default offset threshold for [`TimestampTrust::Auto`]: 5 s.
///
/// Deliberately loose. It is not trying to detect drift — that is what `clock_samples` is
/// for — it is trying to catch a reader whose clock is *categorically* wrong: never set,
/// stuck at boot, or years out. A correctly configured reader on a LAN sits within a few
/// milliseconds; one that has never been synchronized is usually wrong by hours.
pub const DEFAULT_MAX_OFFSET_MS: i64 = 5_000;

/// Turns [`ReaderMessage`] values into [`RawRead`] evidence.
///
/// This is where "reader timestamps are authoritative when present" is actually
/// implemented — with the guardrails that keep it safe. Whatever it decides, it records
/// *what* it decided and *why* in [`RawRead::timestamp_source`], so a disputed result can
/// be answered from the database alone.
#[derive(Debug, Clone, Copy)]
pub struct Ingest {
    /// How far this reader's clock is trusted.
    pub trust: TimestampTrust,
    /// Offset beyond which [`TimestampTrust::Auto`] stops believing the reader.
    pub max_offset_ms: i64,
}

impl Default for Ingest {
    fn default() -> Self {
        Self {
            trust: TimestampTrust::default(),
            max_offset_ms: DEFAULT_MAX_OFFSET_MS,
        }
    }
}

impl Ingest {
    /// Normalizes a reader message into immutable evidence.
    ///
    /// `received_at` is the device wall clock when the message came off the socket;
    /// `monotonic_ns` is the device monotonic clock at the same instant, which is the only
    /// safe basis for later interval arithmetic.
    #[must_use]
    pub fn normalize(
        &self,
        message: ReaderMessage,
        received_at: OffsetDateTime,
        monotonic_ns: Option<u64>,
        device_clock_state: DeviceClockState,
    ) -> RawRead {
        let (reader_timestamp, reader_uptime_us, clock_offset_ms) = match message.timestamp {
            ReaderTimestamp::Utc { at } => {
                let offset = (received_at - at).whole_milliseconds();
                let offset = i64::try_from(offset).unwrap_or(i64::MAX);
                (Some(at), None, Some(offset))
            }
            ReaderTimestamp::Uptime { micros } => (None, Some(micros), None),
            ReaderTimestamp::Absent => (None, None, None),
        };

        let timestamp_source = match (reader_timestamp, reader_uptime_us) {
            (Some(_), _) => match self.trust {
                TimestampTrust::Trusted => TimestampSource::ReaderUtc,
                TimestampTrust::Untrusted => TimestampSource::DeviceReceipt {
                    reason: FallbackReason::ReaderUntrusted,
                },
                TimestampTrust::Auto => {
                    let within_threshold =
                        clock_offset_ms.is_some_and(|ms| ms.abs() <= self.max_offset_ms);
                    if within_threshold {
                        TimestampSource::ReaderUtc
                    } else {
                        TimestampSource::DeviceReceipt {
                            reason: FallbackReason::OffsetExceededThreshold,
                        }
                    }
                }
            },
            (None, Some(_)) => TimestampSource::DeviceReceipt {
                reason: FallbackReason::ReaderUptimeOnly,
            },
            (None, None) => TimestampSource::DeviceReceipt {
                reason: FallbackReason::NoReaderTimestamp,
            },
        };

        RawRead {
            id: RawReadId::new(),
            source: message.source,
            antenna: message.antenna,
            chip: message.chip,
            reader_timestamp,
            reader_uptime_us,
            received_at,
            received_at_monotonic_ns: monotonic_ns,
            rssi_dbm: message.rssi_dbm,
            device_clock_state,
            timestamp_source,
            clock_offset_ms,
            raw_payload: message.raw_payload,
        }
    }
}

/// A source of reads.
///
/// Channel-based rather than `async fn` in a trait, so it stays dyn-compatible: the edge
/// service holds `Box<dyn ReaderProvider>` and cannot tell a simulator from a reader.
pub trait ReaderProvider: Send {
    /// Which reader this provider represents.
    fn reader_id(&self) -> ReaderId;

    /// Starts producing reads, returning the channel they arrive on.
    ///
    /// The provider owns its own connection lifecycle — including reconnect and backoff —
    /// and keeps producing until the channel is dropped.
    fn start(self: Box<Self>) -> mpsc::Receiver<ReaderMessage>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::Duration;

    fn message(timestamp: ReaderTimestamp) -> ReaderMessage {
        ReaderMessage {
            source: ReaderId::new("finish"),
            antenna: Some(1),
            chip: ChipId::new("E280"),
            timestamp,
            rssi_dbm: Some(-52),
            raw_payload: vec![0xAA],
        }
    }

    fn received() -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH + Duration::seconds(1_000)
    }

    #[test]
    fn a_trusted_reader_timestamp_becomes_authoritative() {
        let reader_at = received() - Duration::milliseconds(40);
        let read = Ingest::default().normalize(
            message(ReaderTimestamp::Utc { at: reader_at }),
            received(),
            None,
            DeviceClockState::GpsLocked,
        );

        assert_eq!(read.timestamp_source, TimestampSource::ReaderUtc);
        assert_eq!(read.authoritative_timestamp(), reader_at);
        assert_eq!(read.clock_offset_ms, Some(40));
    }

    #[test]
    fn auto_trust_rejects_a_reader_whose_clock_is_categorically_wrong() {
        // A reader that has never been synchronized: claims a time three days off.
        let reader_at = received() - Duration::days(3);
        let read = Ingest::default().normalize(
            message(ReaderTimestamp::Utc { at: reader_at }),
            received(),
            None,
            DeviceClockState::GpsLocked,
        );

        assert_eq!(
            read.timestamp_source,
            TimestampSource::DeviceReceipt {
                reason: FallbackReason::OffsetExceededThreshold
            }
        );
        assert_eq!(read.authoritative_timestamp(), received());
        // The offset is still recorded — that is the evidence the reader is broken.
        assert_eq!(read.clock_offset_ms, Some(3 * 24 * 60 * 60 * 1_000));
    }

    #[test]
    fn an_uptime_only_reader_never_produces_a_1970_date() {
        let read = Ingest::default().normalize(
            message(ReaderTimestamp::Uptime { micros: 42_000_000 }),
            received(),
            None,
            DeviceClockState::Rtc,
        );

        assert_eq!(read.reader_timestamp, None);
        assert_eq!(read.reader_uptime_us, Some(42_000_000));
        assert_eq!(
            read.timestamp_source,
            TimestampSource::DeviceReceipt {
                reason: FallbackReason::ReaderUptimeOnly
            }
        );
        assert_eq!(read.authoritative_timestamp(), received());
    }

    #[test]
    fn an_untrusted_reader_is_ignored_even_when_its_clock_looks_fine() {
        let reader_at = received() - Duration::milliseconds(5);
        let ingest = Ingest {
            trust: TimestampTrust::Untrusted,
            ..Ingest::default()
        };
        let read = ingest.normalize(
            message(ReaderTimestamp::Utc { at: reader_at }),
            received(),
            None,
            DeviceClockState::GpsLocked,
        );

        assert_eq!(
            read.timestamp_source,
            TimestampSource::DeviceReceipt {
                reason: FallbackReason::ReaderUntrusted
            }
        );
    }

    #[test]
    fn a_missing_timestamp_falls_back_and_says_so() {
        let read = Ingest::default().normalize(
            message(ReaderTimestamp::Absent),
            received(),
            Some(123),
            DeviceClockState::Unsynced,
        );

        assert_eq!(
            read.timestamp_source,
            TimestampSource::DeviceReceipt {
                reason: FallbackReason::NoReaderTimestamp
            }
        );
        assert_eq!(read.received_at_monotonic_ns, Some(123));
        assert_eq!(read.device_clock_state, DeviceClockState::Unsynced);
    }

    #[test]
    fn a_reader_running_fast_is_still_trusted_within_the_threshold() {
        // Offset can be negative: the reader's clock is ahead of the device's.
        let reader_at = received() + Duration::milliseconds(120);
        let read = Ingest::default().normalize(
            message(ReaderTimestamp::Utc { at: reader_at }),
            received(),
            None,
            DeviceClockState::GpsLocked,
        );

        assert_eq!(read.clock_offset_ms, Some(-120));
        assert_eq!(read.timestamp_source, TimestampSource::ReaderUtc);
    }
}
