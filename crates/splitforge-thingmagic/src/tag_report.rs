//! Turning a streaming `0x22` response into a read.
//!
//! The seam [`crate::provider::TagReportDecoder`] left empty, filled at last. What made it
//! fillable was not new code but a document: a 2023 MercuryAPI was located and archived, and
//! `docs/readers/vendor-documents.md` now records the `TMR_TRD_METADATA_FLAG_*` values, the
//! order the flagged fields appear in, and the byte that separates a tag frame from a status
//! frame.
//!
//! # What anchors this
//!
//! [`crate::crc::CAPTURED_FRAME`] — a real `0x22` response from a real module, the same frame
//! that caught the CRC defect. It decodes here to read count 1, RSSI −60 dBm, antenna tx 1 /
//! rx 1, 923.200 MHz, 295 ms, phase 0, Gen2, no embedded data, GPIO `0x0F`, and a 96-bit EPC —
//! **and it consumes to the payload's last byte exactly**, which is the assertion that would
//! fail first if any width or any order in the table were wrong.
//!
//! That is one frame from an M6e, not a stream from an M7e-Pico, and it is the difference
//! between *written from documentation alone* — which
//! [ADR-0004](../../../docs/adr/0004-llrp-first-reader-adapter.md) forbids and which this
//! crate has already shipped once — and *written from documentation and checked against
//! hardware output*. It is not the same as having run against a module.
//!
//! # What this refuses to do
//!
//! **Guess.** Everything the captured frame does not anchor is an error rather than an
//! assumption, because the failure this crate has already had was silent: a parser that is
//! internally consistent, externally wrong, and fully tested. An error is loud and is a
//! one-line fix for whoever holds the next capture; a wrong parse is a chip identifier nobody
//! can trace.
//!
//! Three things are refused for that reason, and each names itself in the error:
//!
//! - **A metadata flag above `0x0100`.** The 2023 layout has five more fields than the nine
//!   this decoder walks, and a decoder that ignored an unknown high bit would not fail — it
//!   would read the next field's bytes as this one's and return a plausible, wrong EPC.
//! - **A response whose option byte does not set `0x10`.** That selects a different position
//!   for the response-type byte in the vendor's own parser, and no captured frame shows it.
//! - **A truncated record.** Every read is bounds-checked; running out of bytes is a decode
//!   error, never a panic and never a short EPC.

use splitforge_domain::{ChipId, ReaderId};
use splitforge_reader::{ReaderMessage, ReaderTimestamp};

use crate::frame::Response;
use crate::provider::{SessionAnchor, TagReportDecoder};

/// The metadata flags word, from `tmr_tag_data.h`, `enum TMR_TRD_MetadataFlag`.
///
/// Recorded with provenance in
/// [vendor-documents.md](../../../docs/readers/vendor-documents.md#metadata-flags--tmr_tag_datah-enum-tmr_trd_metadataflag).
/// **`ALL` is deliberately absent**: the 2023 header composes it from `#ifdef`s, so its value
/// depends on how the SDK was compiled, and the only flags word that matters here is the one
/// that arrived on the wire.
pub mod flag {
    /// Number of times the tag was seen.
    pub const READCOUNT: u16 = 0x0001;
    /// Receive signal strength, in dBm. Signed.
    pub const RSSI: u16 = 0x0002;
    /// A packed transmit/receive nibble pair, **not** an antenna number.
    pub const ANTENNAID: u16 = 0x0004;
    /// Carrier frequency in kHz.
    pub const FREQUENCY: u16 = 0x0008;
    /// Milliseconds since the read command was issued.
    pub const TIMESTAMP: u16 = 0x0010;
    /// Gen2 phase angle.
    pub const PHASE: u16 = 0x0020;
    /// Air protocol. Gen2 is `5`.
    pub const PROTOCOL: u16 = 0x0040;
    /// Embedded read data, length-prefixed in **bits**.
    pub const DATA: u16 = 0x0080;
    /// One bit per GPIO pin.
    pub const GPIO_STATUS: u16 = 0x0100;

    /// Everything this decoder can walk.
    ///
    /// A flags word with any bit outside this mask is refused rather than approximated — see
    /// the module documentation.
    pub const DECODABLE: u16 = READCOUNT
        | RSSI
        | ANTENNAID
        | FREQUENCY
        | TIMESTAMP
        | PHASE
        | PROTOCOL
        | DATA
        | GPIO_STATUS;
}

/// Offsets into a streaming response's **payload**, not into the whole frame.
///
/// The vendor's parser indexes the whole message, header included; [`Response::data`] begins
/// after the five-byte response header, so every offset here is the SDK's minus
/// [`crate::frame::RESPONSE_HEADER_LEN`]. Getting that subtraction wrong would shift every
/// field by five bytes, which is why the constants are named and the arithmetic appears once.
mod offset {
    /// The option byte. `serial_reader.c` tests `msg[5] & 0x10` to place the next one.
    pub(super) const OPTION: usize = 0;
    /// The metadata flags word, big-endian. The SDK's `GETU16AT(msg, 8)`.
    pub(super) const FLAGS: usize = 3;
    /// The response-type byte, when [`OPTION`] has `0x10` set. The SDK's `msg[10]`.
    pub(super) const RESPONSE_TYPE: usize = 5;
    /// Where the flagged metadata begins. The SDK's `bufPointer = 11`.
    pub(super) const METADATA: usize = 6;
}

/// What a streaming response turned out to be.
///
/// From `serial_reader.c`, `TMR_SR_hasMoreTags`. **Checked before the flags word is read**,
/// which is the ordering that keeps a status frame from being parsed as a tag report and
/// becoming a read about a chip that was never there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamResponse {
    /// A tag read; the stream continues.
    Tag,
    /// Reader statistics, not a tag.
    Status,
    /// The stream ends with this message.
    End,
}

/// Why a tag report could not be decoded.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReportError {
    /// The payload ended in the middle of a field.
    #[error("the report ended after {available} byte(s); {wanted} more were needed for {field}")]
    Truncated {
        /// What was being read.
        field: &'static str,
        /// How many bytes were still needed.
        wanted: usize,
        /// How many the payload had left.
        available: usize,
    },
    /// The flags word named a field this decoder does not know how to walk.
    #[error(
        "metadata flags {flags:#06x} include {unknown:#06x}, which this decoder cannot walk; \
         the 2023 layout has five fields the nine recorded here do not cover"
    )]
    UnknownFlags {
        /// The whole flags word, as it arrived.
        flags: u16,
        /// The bits outside [`flag::DECODABLE`].
        unknown: u16,
    },
    /// The option byte did not set `0x10`.
    #[error(
        "the option byte is {option:#04x}, which does not set 0x10; no captured frame shows \
         where the response type sits in that layout"
    )]
    UnknownLayout {
        /// The byte as it arrived.
        option: u8,
    },
    /// The response-type byte was not one of the three the SDK switches on.
    #[error("the response type is {kind:#04x}, which is not 0x00, 0x01 or 0x02")]
    UnknownResponseType {
        /// The byte as it arrived.
        kind: u8,
    },
    /// The record was walked successfully and did not reach the end of the payload.
    ///
    /// **The quiet failure made loud.** A field read one byte too narrow leaves bytes over
    /// rather than running off the end, and returns an EPC shifted by one — a plausible,
    /// wrong chip id that no test comparing counts would catch.
    #[error("the report ended with {leftover} byte(s) unread, so some field's width is wrong")]
    TrailingBytes {
        /// How many bytes the walk did not account for.
        leftover: usize,
    },
}

/// A bounds-checked walk over a payload.
///
/// Every accessor returns an error rather than panicking, because
/// [`TagReportDecoder::decode`] is on the read path and a corrupt frame must become a counted
/// fault rather than a timer that stops mid-race.
struct Cursor<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    const fn new(data: &'a [u8], at: usize) -> Self {
        Self { data, at }
    }

    fn take(&mut self, wanted: usize, field: &'static str) -> Result<&'a [u8], ReportError> {
        let available = self.data.len().saturating_sub(self.at);
        let end = self.at.checked_add(wanted).ok_or(ReportError::Truncated {
            field,
            wanted,
            available,
        })?;
        let slice = self.data.get(self.at..end).ok_or(ReportError::Truncated {
            field,
            wanted,
            available,
        })?;
        self.at = end;
        Ok(slice)
    }

    fn u8(&mut self, field: &'static str) -> Result<u8, ReportError> {
        Ok(self.take(1, field)?[0])
    }

    /// Big-endian, per `tmr_utils.h`'s accessor macros.
    fn u16(&mut self, field: &'static str) -> Result<u16, ReportError> {
        let bytes = self.take(2, field)?;
        Ok(u16::from(bytes[0]) << 8 | u16::from(bytes[1]))
    }

    fn u24(&mut self, field: &'static str) -> Result<u32, ReportError> {
        let bytes = self.take(3, field)?;
        Ok(u32::from(bytes[0]) << 16 | u32::from(bytes[1]) << 8 | u32::from(bytes[2]))
    }

    fn u32(&mut self, field: &'static str) -> Result<u32, ReportError> {
        let bytes = self.take(4, field)?;
        Ok(u32::from(bytes[0]) << 24
            | u32::from(bytes[1]) << 16
            | u32::from(bytes[2]) << 8
            | u32::from(bytes[3]))
    }

    /// A field whose length arrives as a **bit** count, per `tm_u8s_per_bits`.
    fn bit_counted(&mut self, field: &'static str) -> Result<&'a [u8], ReportError> {
        let bits = self.u16(field)?;
        let bytes = usize::from(bits).div_ceil(8);
        self.take(bytes, field)
    }
}

/// Everything one tag report carried, before it becomes evidence.
#[derive(Debug, Default)]
struct Metadata {
    rssi_dbm: Option<i16>,
    antenna: Option<u16>,
    timestamp_ms: Option<u32>,
    protocol: Option<u8>,
}

/// Decodes the continuous-read stream into reads.
///
/// One response carries at most one tag: `TMR_SR_hasMoreTags` sets `tagsRemainingInBuffer = 1`
/// for a tag frame, so this appends either zero reads or one.
///
/// **Faults are counted rather than returned**, because the trait cannot return them and a
/// read path that stopped on one malformed frame would stop the timer. [`Self::errors`] is the
/// number that matters: a decoder producing no reads and climbing errors is a layout mismatch,
/// and one producing no reads with no errors is a quiet field.
#[derive(Debug)]
pub struct StreamDecoder {
    reader_id: ReaderId,
    frames: u64,
    reads: u64,
    errors: u64,
    reported: bool,
}

impl StreamDecoder {
    /// A decoder that stamps every read it produces with `reader_id`.
    ///
    /// The id comes from the operator's configuration rather than from the module, which has
    /// no name for itself — it is what a gap and a read are both recorded against.
    #[must_use]
    pub const fn new(reader_id: ReaderId) -> Self {
        Self {
            reader_id,
            frames: 0,
            reads: 0,
            errors: 0,
            reported: false,
        }
    }

    /// Frames offered to this decoder, tag reports or not.
    #[must_use]
    pub const fn frames(&self) -> u64 {
        self.frames
    }

    /// Reads produced.
    #[must_use]
    pub const fn reads(&self) -> u64 {
        self.reads
    }

    /// Frames that could not be decoded.
    ///
    /// **Not the same as frames that carried no tag.** A status report and a stream-end are
    /// decoded successfully and produce nothing; this counts only the ones whose layout this
    /// decoder could not walk.
    #[must_use]
    pub const fn errors(&self) -> u64 {
        self.errors
    }

    /// Classifies a streaming response before any of its metadata is read.
    fn classify(data: &[u8]) -> Result<StreamResponse, ReportError> {
        let option = data
            .get(offset::OPTION)
            .copied()
            .ok_or(ReportError::Truncated {
                field: "the option byte",
                wanted: 1,
                available: data.len(),
            })?;

        // The vendor's parser reads the response type from a different offset when this bit is
        // clear. No captured frame shows that layout, so it is refused rather than assumed —
        // the whole record's alignment depends on getting this right.
        if option & 0x10 != 0x10 {
            return Err(ReportError::UnknownLayout { option });
        }

        let kind = data
            .get(offset::RESPONSE_TYPE)
            .copied()
            .ok_or(ReportError::Truncated {
                field: "the response-type byte",
                wanted: 1,
                available: data.len(),
            })?;

        match kind {
            0x00 => Ok(StreamResponse::End),
            0x01 => Ok(StreamResponse::Tag),
            0x02 => Ok(StreamResponse::Status),
            kind => Err(ReportError::UnknownResponseType { kind }),
        }
    }

    /// Walks the flagged fields in flag-bit order, then the EPC.
    fn read_tag(&self, data: &[u8], anchor: &SessionAnchor) -> Result<ReaderMessage, ReportError> {
        let mut flags_cursor = Cursor::new(data, offset::FLAGS);
        let flags = flags_cursor.u16("the metadata flags")?;

        // Refused, not ignored. A decoder that skipped an unknown bit would read the next
        // field's bytes as this one's and return a plausible, wrong EPC.
        let unknown = flags & !flag::DECODABLE;
        if unknown != 0 {
            return Err(ReportError::UnknownFlags { flags, unknown });
        }

        let mut cursor = Cursor::new(data, offset::METADATA);
        let mut meta = Metadata::default();

        // **Ascending bit order is the layout.** The vendor's parser tests each flag in turn
        // from 0x0001 upward and consumes the field if it is set, so walking the bits in order
        // cannot get the sequence wrong.
        if flags & flag::READCOUNT != 0 {
            cursor.u8("the read count")?;
        }
        if flags & flag::RSSI != 0 {
            // Signed, and dBm is negative. Read as `i8` before widening, or −60 becomes 196.
            meta.rssi_dbm = Some(i16::from(cursor.u8("the RSSI")? as i8));
        }
        if flags & flag::ANTENNAID != 0 {
            meta.antenna = logical_antenna(cursor.u8("the antenna byte")?);
        }
        if flags & flag::FREQUENCY != 0 {
            cursor.u24("the frequency")?;
        }
        if flags & flag::TIMESTAMP != 0 {
            meta.timestamp_ms = Some(cursor.u32("the timestamp")?);
        }
        if flags & flag::PHASE != 0 {
            cursor.u16("the phase")?;
        }
        if flags & flag::PROTOCOL != 0 {
            meta.protocol = Some(cursor.u8("the protocol")?);
        }
        if flags & flag::DATA != 0 {
            cursor.bit_counted("the embedded data")?;
        }
        if flags & flag::GPIO_STATUS != 0 {
            cursor.u8("the GPIO status")?;
        }

        let epc_record = cursor.bit_counted("the EPC")?;
        let chip = chip_id(epc_record, meta.protocol);

        // **The record must end where the payload ends**, and this is the check that turns a
        // whole class of silent errors into loud ones. A field read one byte too narrow does
        // not run off the end — it leaves bytes over and returns an EPC shifted by one, which
        // is a plausible, wrong chip id. Landing exactly on the boundary is the property the
        // captured frame demonstrates, and it is worth asserting on every frame rather than
        // only in a test.
        let leftover = data.len().saturating_sub(cursor.at);
        if leftover != 0 {
            return Err(ReportError::TrailingBytes { leftover });
        }

        Ok(ReaderMessage {
            source: self.reader_id.clone(),
            antenna: meta.antenna,
            chip,
            timestamp: reader_timestamp(meta.timestamp_ms, anchor),
            rssi_dbm: meta.rssi_dbm,
            // The record this read came from, not the whole frame. It is the evidence a
            // disputed read is re-examined from, so it is the bytes that produced it.
            raw_payload: data.to_vec(),
        })
    }
}

/// The antenna a packed tx/rx byte names, when it names one.
///
/// **The byte is two nibbles, not a number** — `TMR_SR_postprocessReaderSpecificMetadata`
/// splits it into a transmit port and a receive port, and a `0` nibble means 16 rather than 0
/// ([finding 10](../../../docs/readers/vendor-documents.md#10-the-antenna-byte-is-a-packed-txrx-nibble-pair-not-an-antenna-number)).
/// Passing the raw byte through would report antenna `17` for a tag read on tx 1 / rx 1, and
/// would do it consistently enough to look like a working mapping.
///
/// **Only a monostatic pair becomes a number.** The vendor's own logical-antenna lookup needs
/// a `txRxMap` this project does not have, so the one case that can be resolved without it is
/// the one where transmit and receive are the same port — which the user guide says is the
/// only case this module produces: *"the ThingMagic module does not support bistatic (separate
/// transmit and receive port)"*. Anything else is `None`, because a wrong antenna maps a
/// runner to the wrong checkpoint.
fn logical_antenna(byte: u8) -> Option<u16> {
    let tx = (byte >> 4) & 0x0F;
    let rx = byte & 0x0F;
    // Antenna 16 wraps to 0 for want of a fifth bit, which the SDK undoes the same way.
    let unwrap = |nibble: u8| if nibble == 0 { 16 } else { u16::from(nibble) };
    (tx == rx).then(|| unwrap(rx))
}

/// The chip identifier an EPC record carries.
///
/// For Gen2 the record is a two-byte PC word, then the EPC, then a two-byte tag CRC — so the
/// identifier is the middle, and handing back the whole record would put four bytes that are
/// not the chip into every `ChipId` in the journal. For any other protocol the record is taken
/// whole, because nothing here knows its shape.
fn chip_id(record: &[u8], protocol: Option<u8>) -> ChipId {
    /// `TMR_TAG_PROTOCOL_GEN2`.
    const GEN2: u8 = 5;

    let epc = match protocol {
        Some(GEN2) if record.len() >= 4 => &record[2..record.len() - 2],
        _ => record,
    };

    let mut hex = String::with_capacity(epc.len() * 2);
    for byte in epc {
        use std::fmt::Write as _;
        // Writing to a String cannot fail; the result is discarded rather than unwrapped
        // because this is on the read path.
        let _ = write!(hex, "{byte:02X}");
    }
    ChipId::new(hex)
}

/// What the module said about when, in the shape the port models it.
///
/// **The unit is an assumption, and a named one.** § 8.8.3 calls the field milliseconds; the
/// SDK calls the variable `dspMicros`; the two cannot both be right, and the platform file
/// that would settle it is not in the archived mirror
/// ([finding 11](../../../docs/readers/vendor-documents.md#11-mercuryapi-anchors-the-relative-timestamp-exactly-as-adr-0024-prescribes)).
/// Milliseconds is taken here because the vendor's prose says so, and it is recorded as an
/// assumption because **a 1000× error would look exactly like a working timestamp** on a bench
/// and fail only on the arithmetic.
///
/// Nothing rests on it being right. [`ReaderTimestamp::Uptime`] routes through
/// `FallbackReason::ReaderUptimeOnly`, so the device's receipt time is authoritative and this
/// value is preserved beside it as evidence — which is what
/// [ADR-0024](../../../docs/adr/0024-serial-reader-adapter-before-llrp.md) prescribes and why
/// the anchor exists at all.
fn reader_timestamp(since_command_ms: Option<u32>, _anchor: &SessionAnchor) -> ReaderTimestamp {
    since_command_ms.map_or(ReaderTimestamp::Absent, |ms| ReaderTimestamp::Uptime {
        micros: u64::from(ms) * 1_000,
    })
}

impl TagReportDecoder for StreamDecoder {
    fn decode(
        &mut self,
        response: &Response<'_>,
        anchor: &SessionAnchor,
        out: &mut Vec<ReaderMessage>,
    ) {
        self.frames = self.frames.saturating_add(1);

        let outcome = Self::classify(response.data).and_then(|kind| match kind {
            // Both are successful decodes that carry no tag, and neither is a fault: a status
            // report is reader statistics and a stream-end is the module saying it is done.
            StreamResponse::Status | StreamResponse::End => Ok(None),
            StreamResponse::Tag => self.read_tag(response.data, anchor).map(Some),
        });

        match outcome {
            Ok(Some(message)) => {
                self.reads = self.reads.saturating_add(1);
                out.push(message);
            }
            Ok(None) => {}
            Err(error) => {
                self.errors = self.errors.saturating_add(1);
                // Said once. A layout mismatch is systematic, so logging every frame would
                // fill journald with one sentence and bury the rest of the event; the count
                // is the ongoing signal and `/health` is where it belongs.
                if !self.reported {
                    self.reported = true;
                    eprintln!(
                        "splitforge-thingmagic: a tag report could not be decoded — {error}. \
                         Further failures are counted rather than logged."
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crc::{CAPTURED_FRAME, crc16};
    use crate::frame::{Decoded, RESPONSE_HEADER_LEN, SOH, decode};

    fn anchor() -> SessionAnchor {
        SessionAnchor::now()
    }

    fn decoder() -> StreamDecoder {
        StreamDecoder::new(ReaderId::new("mat"))
    }

    /// Decodes a whole frame the way the reassembler would, and hands over the response.
    fn feed(decoder: &mut StreamDecoder, frame: &[u8]) -> Vec<ReaderMessage> {
        let Decoded::Frame { response, .. } = decode(frame).expect("a well-formed frame") else {
            panic!("the fixture is a complete frame");
        };
        let mut out = Vec::new();
        decoder.decode(&response, &anchor(), &mut out);
        out
    }

    /// Wraps a payload in a valid response frame, so tests vary the payload and nothing else.
    fn framed(payload: &[u8]) -> Vec<u8> {
        let mut frame = Vec::with_capacity(RESPONSE_HEADER_LEN + payload.len() + 2);
        frame.push(SOH);
        frame.push(u8::try_from(payload.len()).expect("payloads here are small"));
        frame.push(0x22);
        frame.push(0);
        frame.push(0);
        frame.extend_from_slice(payload);
        let crc = crc16(&frame[1..]);
        frame.push((crc >> 8) as u8);
        frame.push((crc & 0xFF) as u8);
        frame
    }

    /// The captured frame's payload, which every synthetic case is a mutation of.
    fn captured_payload() -> Vec<u8> {
        CAPTURED_FRAME[RESPONSE_HEADER_LEN..CAPTURED_FRAME.len() - 2].to_vec()
    }

    #[test]
    fn the_captured_frame_decodes_to_the_read_the_module_reported() {
        // **The anchor.** A real `0x22` response from real hardware — the same frame that
        // caught the CRC defect — decoded field by field. Every value below is one this
        // decoder derived, not one restated from the table it was written against, and each
        // is independently plausible: a negative dBm, a frequency inside the US band, a
        // protocol number that means Gen2.
        let mut decoder = decoder();
        let out = feed(&mut decoder, &CAPTURED_FRAME);

        assert_eq!(out.len(), 1, "one streaming response carries one tag");
        let read = &out[0];

        assert_eq!(read.source, ReaderId::new("mat"));
        assert_eq!(read.rssi_dbm, Some(-60), "signed: 0xC4 is -60 dBm, not 196");
        assert_eq!(read.antenna, Some(1), "tx 1 / rx 1 is antenna 1, not 0x11");
        assert_eq!(
            read.timestamp,
            ReaderTimestamp::Uptime {
                micros: 295 * 1_000
            },
            "295 ms since the read command, widened to the port's unit"
        );
        // 12 bytes, with the two-byte PC word and the two-byte tag CRC removed — handing back
        // the whole record would put four bytes that are not the chip into every ChipId.
        assert_eq!(read.chip, ChipId::new("000000000000000000001545"));

        assert_eq!(decoder.frames(), 1);
        assert_eq!(decoder.reads(), 1);
        assert_eq!(decoder.errors(), 0);
    }

    #[test]
    fn the_captured_frame_consumes_to_its_last_byte() {
        // The assertion that would fail first if any width or any order were wrong. A layout
        // error almost never lands exactly on the boundary — it runs off the end, or leaves
        // bytes over. Truncating the payload by one byte must therefore fail.
        let payload = captured_payload();
        let short = framed(&payload[..payload.len() - 1]);

        let mut decoder = decoder();
        let out = feed(&mut decoder, &short);

        assert!(out.is_empty(), "a byte short is not a read");
        assert_eq!(
            decoder.errors(),
            1,
            "the EPC ran past the end, which is a decode error rather than a short chip id"
        );
    }

    #[test]
    fn a_status_frame_is_not_a_read_and_is_not_an_error() {
        // The hazard the response-type byte exists to prevent. Parsed as a tag report this
        // would produce a read with a fabricated EPC — evidence about a chip that was never
        // there, in an append-only table.
        let mut payload = captured_payload();
        payload[offset::RESPONSE_TYPE] = 0x02;

        let mut decoder = decoder();
        let out = feed(&mut decoder, &framed(&payload));

        assert!(out.is_empty(), "a status report carries no tag");
        assert_eq!(
            decoder.errors(),
            0,
            "a status frame is a successful decode of something that is not a read"
        );
        assert_eq!(decoder.frames(), 1);
    }

    #[test]
    fn the_end_of_a_stream_is_not_a_read_and_is_not_an_error() {
        let mut payload = captured_payload();
        payload[offset::RESPONSE_TYPE] = 0x00;

        let mut decoder = decoder();
        assert!(feed(&mut decoder, &framed(&payload)).is_empty());
        assert_eq!(decoder.errors(), 0);
    }

    #[test]
    fn a_flag_this_decoder_cannot_walk_is_refused_rather_than_skipped() {
        // The failure mode finding 13 names: the 2023 layout has five fields above 0x0100,
        // and a decoder that ignored one would read the next field's bytes as this one's and
        // return a plausible, wrong chip id. Refusing is loud; skipping is silent.
        for extra in [0x0200_u16, 0x0400, 0x0800, 0x1000, 0x2000] {
            let mut payload = captured_payload();
            let flags = 0x01FF | extra;
            payload[offset::FLAGS] = (flags >> 8) as u8;
            payload[offset::FLAGS + 1] = (flags & 0xFF) as u8;

            let mut decoder = decoder();
            let out = feed(&mut decoder, &framed(&payload));

            assert!(out.is_empty(), "{extra:#06x} must not produce a read");
            assert_eq!(
                decoder.errors(),
                1,
                "{extra:#06x} must be counted as a fault"
            );
        }
    }

    #[test]
    fn an_unseen_layout_is_refused_rather_than_assumed() {
        // The option byte places the response-type byte. With 0x10 clear the vendor's parser
        // reads it from somewhere else, and no captured frame shows that arrangement — so the
        // whole record's alignment is unknown and a guess would misread every field.
        let mut payload = captured_payload();
        payload[offset::OPTION] &= !0x10;

        let mut decoder = decoder();
        assert!(feed(&mut decoder, &framed(&payload)).is_empty());
        assert_eq!(decoder.errors(), 1);
    }

    #[test]
    fn no_payload_makes_the_decoder_panic() {
        // The trait's one hard rule: a frame has passed its CRC by the time it arrives, which
        // proves it was transmitted intact and not that it means what this decoder expects.
        let mut decoder = decoder();
        for len in 0..64_usize {
            for fill in [0x00_u8, 0x01, 0x10, 0x11, 0xFF] {
                let payload = vec![fill; len];
                let out = feed(&mut decoder, &framed(&payload));
                // Whatever it decides, it must not panic and must not invent a read from a
                // buffer that is plainly not a tag report.
                assert!(out.len() <= 1);
            }
        }
    }

    #[test]
    fn a_bistatic_antenna_byte_is_no_antenna_at_all() {
        // tx and rx differ, so the logical port needs the vendor's txRxMap, which this
        // project does not have. A wrong antenna maps a runner to the wrong checkpoint, so
        // the field is absent rather than approximated.
        assert_eq!(logical_antenna(0x11), Some(1));
        assert_eq!(logical_antenna(0x22), Some(2));
        assert_eq!(logical_antenna(0x12), None, "tx 1 / rx 2 is bistatic");
        // "Due to limited space, Antenna 16 wraps around to 0" — the SDK undoes it the same
        // way, and 0 would otherwise be an antenna number no module has.
        assert_eq!(logical_antenna(0x00), Some(16));
    }

    #[test]
    fn a_non_gen2_record_keeps_its_whole_payload() {
        // The PC word and tag CRC are a Gen2 shape. Stripping four bytes from a protocol
        // whose record shape is unknown would silently shorten the chip id.
        let record = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];
        // Six bytes less the two-byte PC word and the two-byte tag CRC leaves two.
        assert_eq!(chip_id(&record, Some(5)), ChipId::new("CCDD"));
        assert_eq!(chip_id(&record, None), ChipId::new("AABBCCDDEEFF"));
        assert_eq!(chip_id(&record, Some(1)), ChipId::new("AABBCCDDEEFF"));
    }

    #[test]
    fn the_first_failure_is_reported_and_the_rest_are_counted() {
        // A layout mismatch is systematic. Logging every frame would fill journald with one
        // sentence and bury the rest of the event, so the count is the ongoing signal.
        let mut payload = captured_payload();
        payload[offset::OPTION] &= !0x10;
        let frame = framed(&payload);

        let mut decoder = decoder();
        for _ in 0..5 {
            feed(&mut decoder, &frame);
        }

        assert_eq!(decoder.errors(), 5);
        assert_eq!(decoder.reads(), 0);
        assert_eq!(decoder.frames(), 5);
    }

    #[test]
    fn every_error_reads_as_one_sentence() {
        // The defect that has shipped four times in this repository: a message written across
        // two source lines whose trailing backslash was dropped still compiles, and prints
        // the source file's indentation into the middle of the sentence.
        let errors = [
            ReportError::Truncated {
                field: "the EPC",
                wanted: 4,
                available: 1,
            },
            ReportError::UnknownFlags {
                flags: 0x11FF,
                unknown: 0x1000,
            },
            ReportError::UnknownLayout { option: 0x00 },
            ReportError::UnknownResponseType { kind: 0x07 },
            ReportError::TrailingBytes { leftover: 3 },
        ];

        for error in errors {
            let text = format!("{error}");
            assert!(!text.contains("  "), "collapsed continuation: {text:?}");
            assert!(!text.is_empty());
        }
    }

    #[test]
    fn bytes_left_over_are_a_fault_rather_than_a_shorter_read() {
        // The other side of the exact-consumption property. Truncation is caught by running
        // off the end; a field read too narrow is caught here, and it is the one that would
        // otherwise return a plausible chip id shifted by a byte.
        let mut payload = captured_payload();
        payload.push(0x00);

        let mut decoder = decoder();
        let out = feed(&mut decoder, &framed(&payload));

        assert!(
            out.is_empty(),
            "a record that does not fit its payload is not a read"
        );
        assert_eq!(decoder.errors(), 1);
    }
}
