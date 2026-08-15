//! Raw reads: the evidence.
//!
//! A [`RawRead`] is what a reader told us, normalized. It is written to the journal
//! before deduplication, before chip resolution, and before any check of whether it makes
//! sense. Nothing downstream may modify it.
//!
//! See `docs/timing-model.md` and `docs/clock-and-time-discipline.md`.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::ids::{ChipId, RawReadId, ReaderId};

/// The device clock's synchronization state at the moment a read was received.
///
/// Recorded per read rather than per session: a Pi 3 has no battery-backed clock, so its
/// notion of "now" can change mid-event when a time source appears.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceClockState {
    /// Disciplined by GPS with a pulse-per-second signal.
    GpsLocked,
    /// Set from a battery-backed real-time clock.
    Rtc,
    /// Synchronized by NTP.
    NtpSynced,
    /// Set by hand by an operator.
    Manual,
    /// No known-good time source. The device clock cannot be trusted.
    Unsynced,
}

impl DeviceClockState {
    /// Whether results derived under this clock state can be published without a caveat.
    #[must_use]
    pub const fn is_trustworthy(self) -> bool {
        matches!(self, Self::GpsLocked | Self::Rtc | Self::NtpSynced)
    }
}

/// Why a read fell back to the device clock instead of the reader's timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FallbackReason {
    /// The reader supplied no timestamp at all.
    NoReaderTimestamp,
    /// The reader supplied `Uptime` rather than `UTCTimestamp`, so it has no UTC clock.
    ReaderUptimeOnly,
    /// The reader is configured as untrusted.
    ReaderUntrusted,
    /// Measured offset between reader and device exceeded the configured threshold.
    OffsetExceededThreshold,
}

/// Which clock produced the authoritative timestamp for a read, and why.
///
/// Stored so that a disputed result can be answered honestly. "This time came from a
/// GPS-disciplined reader measured at +3 ms" and "this time came from a Pi that had never
/// synchronized" are both legitimate records — but only if we know which one we wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TimestampSource {
    /// The reader supplied a UTC timestamp and is trusted. This is the preferred case.
    ReaderUtc,
    /// Fell back to the device's receipt time.
    DeviceReceipt {
        /// Why the reader's timestamp was not used.
        reason: FallbackReason,
    },
}

/// A single normalized read, exactly as reported. Immutable evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawRead {
    /// Unique identifier, minted at ingest.
    pub id: RawReadId,
    /// Which reader produced this read.
    pub source: ReaderId,
    /// Antenna number, when the reader reports one.
    pub antenna: Option<u16>,
    /// The chip identifier, normally an EPC.
    pub chip: ChipId,
    /// The reader's own UTC timestamp, when it supplied one.
    #[serde(with = "time::serde::rfc3339::option")]
    pub reader_timestamp: Option<OffsetDateTime>,
    /// Microseconds since the reader booted, when it reported `Uptime` instead of UTC.
    pub reader_uptime_us: Option<u64>,
    /// Device wall-clock time when the read came off the socket. Diagnostic.
    #[serde(with = "time::serde::rfc3339")]
    pub received_at: OffsetDateTime,
    /// Device monotonic time at the same instant. The only safe basis for interval math.
    pub received_at_monotonic_ns: Option<u64>,
    /// Signal strength, when the reader reports it.
    pub rssi_dbm: Option<i16>,
    /// Device clock state at the moment of receipt.
    pub device_clock_state: DeviceClockState,
    /// Which clock produced [`RawRead::authoritative_timestamp`].
    pub timestamp_source: TimestampSource,
    /// `received_at - reader_timestamp`, in milliseconds, when both are known.
    pub clock_offset_ms: Option<i64>,
    /// The bytes the reader sent, retained for forensic diagnosis.
    pub raw_payload: Vec<u8>,
}

impl RawRead {
    /// The timestamp that counts for timing purposes.
    ///
    /// Reader timestamps win when present and trusted, because the reader stamps the read
    /// at detection — before network buffering, adapter scheduling, or a busy Pi can add
    /// jitter. Falls back to receipt time otherwise.
    #[must_use]
    pub fn authoritative_timestamp(&self) -> OffsetDateTime {
        match self.timestamp_source {
            // `unwrap_or` rather than `unwrap`: a malformed adapter must not panic the
            // timer, and receipt time is a defensible answer.
            TimestampSource::ReaderUtc => self.reader_timestamp.unwrap_or(self.received_at),
            TimestampSource::DeviceReceipt { .. } => self.received_at,
        }
    }

    /// Whether this read's timestamp came from the reader rather than the device.
    #[must_use]
    pub const fn is_reader_timed(&self) -> bool {
        matches!(self.timestamp_source, TimestampSource::ReaderUtc)
    }
}

/// A raw read as it exists in the journal, with the fields storage assigns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredRawRead {
    /// Monotonic insertion sequence. Gives a total order independent of any clock, which
    /// is what makes "did we lose a read?" answerable.
    pub seq: u64,
    /// When the row was inserted. Diagnostic — reveals write latency and storage stalls.
    #[serde(with = "time::serde::rfc3339")]
    pub recorded_at: OffsetDateTime,
    /// Lowercase hex SHA-256 of [`RawRead::raw_payload`].
    pub payload_sha256: String,
    /// The evidence itself.
    #[serde(flatten)]
    pub read: RawRead,
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::Duration;

    fn read_at(received: OffsetDateTime, reader: Option<OffsetDateTime>) -> RawRead {
        RawRead {
            id: RawReadId::new(),
            source: ReaderId::new("finish"),
            antenna: Some(1),
            chip: ChipId::new("E280"),
            reader_timestamp: reader,
            reader_uptime_us: None,
            received_at: received,
            received_at_monotonic_ns: None,
            rssi_dbm: Some(-55),
            device_clock_state: DeviceClockState::GpsLocked,
            timestamp_source: if reader.is_some() {
                TimestampSource::ReaderUtc
            } else {
                TimestampSource::DeviceReceipt {
                    reason: FallbackReason::NoReaderTimestamp,
                }
            },
            clock_offset_ms: None,
            raw_payload: vec![0x01, 0x02],
        }
    }

    #[test]
    fn reader_timestamp_is_authoritative_when_present() {
        let received = OffsetDateTime::UNIX_EPOCH + Duration::seconds(100);
        let reader = OffsetDateTime::UNIX_EPOCH + Duration::seconds(97);
        let read = read_at(received, Some(reader));
        assert_eq!(read.authoritative_timestamp(), reader);
        assert!(read.is_reader_timed());
    }

    #[test]
    fn falls_back_to_receipt_when_reader_supplies_nothing() {
        let received = OffsetDateTime::UNIX_EPOCH + Duration::seconds(100);
        let read = read_at(received, None);
        assert_eq!(read.authoritative_timestamp(), received);
        assert!(!read.is_reader_timed());
    }

    #[test]
    fn reader_utc_without_a_timestamp_does_not_panic() {
        // A malformed adapter could claim ReaderUtc yet supply no timestamp. Recording a
        // defensible time beats taking the timer down mid-race.
        let received = OffsetDateTime::UNIX_EPOCH + Duration::seconds(100);
        let mut read = read_at(received, Some(received));
        read.reader_timestamp = None;
        assert_eq!(read.authoritative_timestamp(), received);
    }

    #[test]
    fn unsynced_device_clock_is_not_trustworthy() {
        assert!(!DeviceClockState::Unsynced.is_trustworthy());
        assert!(DeviceClockState::GpsLocked.is_trustworthy());
    }
}
