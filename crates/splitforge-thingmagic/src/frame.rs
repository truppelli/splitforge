//! The wire framing, and nothing above it.
//!
//! Pure functions over `&[u8]`. No I/O, no timers, no reader identity, no chip
//! identifiers — this module turns bytes into frames and refuses to guess what they mean.
//! Semantics land on top of it later, which is the order
//! [ADR-0004](../../../docs/adr/0004-llrp-first-reader-adapter.md) argues for and the
//! reason this is buildable before any module has been bought.
//!
//! # Layout
//!
//! A **response** — what the module sends us, and therefore the untrusted direction:
//!
//! ```text
//! +------+------+--------+---------------+------------------+-----------+
//! | 0xFF | len  | opcode | status (u16)  | data (len bytes) | crc (u16) |
//! +------+------+--------+---------------+------------------+-----------+
//!    0      1       2          3..5            5..5+len       5+len..7+len
//! ```
//!
//! A **command** — what we send, which carries no status word:
//!
//! ```text
//! +------+------+--------+------------------+-----------+
//! | 0xFF | len  | opcode | data (len bytes) | crc (u16) |
//! +------+------+--------+------------------+-----------+
//! ```
//!
//! Both integers are big-endian. `len` counts **data bytes only** — not the header, not
//! the status word, not the CRC.
//!
//! # Two assumptions, stated rather than buried
//!
//! Neither has been checked against a physical module, because nobody has one
//! ([the reader notes](../../../docs/readers/thingmagic-m7e-pico.md)). Both are one-line
//! changes if a capture disagrees, and both are written down here so that the person
//! holding the capture knows where to look.
//!
//! 1. **`len` is one byte.** This is load-bearing in a good way: it means a frame can
//!    never claim more than 255 data bytes, so the "hostile reader announces a 64 KB
//!    payload" attack is not mitigated here — it is unrepresentable. [`MAX_FRAME_LEN`] is
//!    262 bytes and that is a property of the protocol, not a limit this crate imposes.
//! 2. **The CRC covers `len`, `opcode`, `status`, and `data`** — everything between the
//!    `0xFF` and the CRC itself, excluding both. If the module disagrees, it disagrees
//!    here and only here: see [`crc_covered_range`].
//!
//! # What this module promises
//!
//! - **It never panics.** Not on truncation, not on garbage, not on any byte sequence.
//! - **It never allocates.** [`Response::data`] borrows from the caller's buffer, so
//!   decoding a frame on the read path costs no allocator traffic at all.
//! - **It never reports a frame it has not verified**, and a partial buffer is reported as
//!   [`Decoded::Incomplete`] rather than as an error — the difference between "read more"
//!   and "something is wrong" is the whole reason a serial reader can be resilient.

use crate::crc::crc16;

/// Start of header. Every frame in both directions begins with this byte.
///
/// It is **not** a unique marker: `0xFF` occurs freely inside payloads, so nothing here
/// treats it as a delimiter. It is a synchronization hint, and the CRC is what actually
/// decides whether a candidate frame is real. See [`resynchronize`].
pub const SOH: u8 = 0xFF;

/// Bytes before a response's payload: `SOH`, `len`, `opcode`, and the two status bytes.
pub const RESPONSE_HEADER_LEN: usize = 5;

/// Bytes before a command's payload: `SOH`, `len`, and `opcode`.
pub const COMMAND_HEADER_LEN: usize = 3;

/// The trailing CRC, in bytes.
pub const CRC_LEN: usize = 2;

/// The most data any frame can carry, because `len` is a single byte.
pub const MAX_DATA_LEN: usize = u8::MAX as usize;

/// The largest frame this protocol can express, in bytes.
///
/// A ceiling that comes from the wire format rather than from a policy decision, which is
/// why a caller can size a fixed buffer to it and be finished with the question.
pub const MAX_FRAME_LEN: usize = RESPONSE_HEADER_LEN + MAX_DATA_LEN + CRC_LEN;

/// One verified response, borrowing its payload from the buffer it was decoded out of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Response<'a> {
    /// Which command this is answering.
    pub opcode: u8,
    /// The module's status word. Zero is success; this module does not interpret the rest,
    /// because mapping status codes to meanings is semantics and belongs above framing.
    pub status: u16,
    /// The payload, borrowed. Empty is normal and not an error.
    pub data: &'a [u8],
}

impl Response<'_> {
    /// Whether the status word reports success.
    #[must_use]
    pub const fn is_ok(&self) -> bool {
        self.status == 0
    }
}

/// The result of looking at a buffer that may or may not hold a whole frame yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decoded<'a> {
    /// A complete, CRC-verified frame.
    Frame {
        /// The frame.
        response: Response<'a>,
        /// How many bytes it occupied. The caller advances by exactly this much; anything
        /// after it is the next frame's problem.
        consumed: usize,
    },
    /// The buffer holds the start of a frame but not all of it.
    ///
    /// Not an error. A serial port delivers whatever happened to be in the kernel buffer,
    /// so this is the ordinary case, and treating it as a failure is how an adapter ends
    /// up discarding good reads under load.
    Incomplete {
        /// How many bytes the frame needs in total, when that is already known — the
        /// length byte has arrived — or the smallest number that would reveal it when it
        /// has not. Always greater than the buffer's current length.
        need_at_least: usize,
    },
}

/// Why a buffer could not be decoded.
///
/// Both variants mean *this buffer is not a frame*, which is recoverable: the caller
/// resynchronizes and carries on. Neither means the process should stop, because a timer
/// that exits on a corrupt frame has turned a glitch into an outage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
    /// The buffer does not begin with [`SOH`].
    #[error("expected a frame to start with 0x{SOH:02X}, found 0x{found:02X}")]
    NotSynchronized {
        /// The byte that was there instead.
        found: u8,
    },
    /// The frame's own CRC does not match the bytes it covers.
    ///
    /// Either the line corrupted it or the frame was never a frame — a false `0xFF` inside
    /// a previous payload lands here, which is exactly how [`resynchronize`] is meant to
    /// be filtered.
    #[error(
        "frame CRC mismatch: the frame declares 0x{declared:04X}, its bytes compute 0x{computed:04X}"
    )]
    BadCrc {
        /// What the frame's trailing bytes claimed.
        declared: u16,
        /// What its covered bytes actually compute to.
        computed: u16,
    },
}

/// Why a command could not be encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum EncodeError {
    /// More payload than the one-byte length field can describe.
    #[error("a command carries at most {MAX_DATA_LEN} data bytes; {requested} were supplied")]
    DataTooLong {
        /// How many bytes the caller offered.
        requested: usize,
    },
    /// The caller's output buffer is smaller than the finished frame.
    #[error("encoding this command needs {needed} bytes; the buffer holds {available}")]
    BufferTooSmall {
        /// Bytes the frame requires.
        needed: usize,
        /// Bytes the buffer has.
        available: usize,
    },
}

/// The byte range a frame's CRC is computed over, given the whole frame.
///
/// Split out and public so that the assumption is checkable rather than implied by an
/// index buried in a loop. If a capture shows the module covering different bytes, this is
/// the function that changes, and every test that builds a frame follows it automatically.
#[must_use]
pub const fn crc_covered_range(frame_len: usize) -> (usize, usize) {
    // From just after SOH, up to just before the trailing CRC.
    (1, frame_len - CRC_LEN)
}

/// Reads a big-endian `u16`, which both the status word and the CRC are.
const fn be_u16(high: u8, low: u8) -> u16 {
    ((high as u16) << 8) | (low as u16)
}

/// Decodes one response from the front of `input`.
///
/// Returns [`Decoded::Incomplete`] when the buffer is short, which is the normal state of
/// affairs on a serial port and not a failure. Never panics, never allocates, and never
/// reads past the end of a frame it has verified.
///
/// # Errors
///
/// [`FrameError::NotSynchronized`] if the buffer does not start with [`SOH`], and
/// [`FrameError::BadCrc`] if the frame's checksum does not match its contents.
pub fn decode(input: &[u8]) -> Result<Decoded<'_>, FrameError> {
    // The length byte is at index 1, so two bytes is the least that reveals a frame's
    // size. Anything shorter cannot even be measured yet.
    let Some(&first) = input.first() else {
        return Ok(Decoded::Incomplete { need_at_least: 2 });
    };

    if first != SOH {
        return Err(FrameError::NotSynchronized { found: first });
    }

    let Some(&data_len) = input.get(1) else {
        return Ok(Decoded::Incomplete { need_at_least: 2 });
    };
    let data_len = data_len as usize;

    // Cannot overflow: data_len is at most 255 and the constants are single digits.
    let frame_len = RESPONSE_HEADER_LEN + data_len + CRC_LEN;
    if input.len() < frame_len {
        return Ok(Decoded::Incomplete {
            need_at_least: frame_len,
        });
    }

    // Every index below is now known to be in bounds, which is why they are computed
    // before any of them is used rather than checked one at a time.
    let (covered_from, covered_to) = crc_covered_range(frame_len);
    let computed = crc16(&input[covered_from..covered_to]);
    let declared = be_u16(input[frame_len - 2], input[frame_len - 1]);

    if declared != computed {
        return Err(FrameError::BadCrc { declared, computed });
    }

    Ok(Decoded::Frame {
        response: Response {
            opcode: input[2],
            status: be_u16(input[3], input[4]),
            data: &input[RESPONSE_HEADER_LEN..RESPONSE_HEADER_LEN + data_len],
        },
        consumed: frame_len,
    })
}

/// Finds where the next frame might start, after `input[0]` has been ruled out.
///
/// Returns the offset of the next [`SOH`], or `None` when the buffer holds no candidate
/// and can be discarded entirely.
///
/// **This only proposes a position.** `0xFF` appears inside payloads all the time, so a
/// hit here is a guess that [`decode`] then accepts or rejects on the CRC. Treating the
/// first `0xFF` as authoritative is the classic way to turn one corrupt byte into a
/// permanently desynchronized stream.
#[must_use]
pub fn resynchronize(input: &[u8]) -> Option<usize> {
    input
        .iter()
        .skip(1)
        .position(|&byte| byte == SOH)
        .map(|offset| offset + 1)
}

/// Encodes a command into `out`, returning the finished frame as a slice of it.
///
/// Takes a caller-supplied buffer rather than returning a `Vec` so that the whole crate
/// stays allocation-free. [`MAX_FRAME_LEN`] is always large enough.
///
/// # Errors
///
/// [`EncodeError::DataTooLong`] if `data` exceeds [`MAX_DATA_LEN`], and
/// [`EncodeError::BufferTooSmall`] if `out` cannot hold the finished frame.
pub fn encode_command<'a>(
    opcode: u8,
    data: &[u8],
    out: &'a mut [u8],
) -> Result<&'a [u8], EncodeError> {
    if data.len() > MAX_DATA_LEN {
        return Err(EncodeError::DataTooLong {
            requested: data.len(),
        });
    }

    let frame_len = COMMAND_HEADER_LEN + data.len() + CRC_LEN;
    if out.len() < frame_len {
        return Err(EncodeError::BufferTooSmall {
            needed: frame_len,
            available: out.len(),
        });
    }

    out[0] = SOH;
    // Narrowing is checked above: data.len() <= MAX_DATA_LEN == u8::MAX.
    out[1] = data.len() as u8;
    out[2] = opcode;
    out[COMMAND_HEADER_LEN..COMMAND_HEADER_LEN + data.len()].copy_from_slice(data);

    let (covered_from, covered_to) = crc_covered_range(frame_len);
    let crc = crc16(&out[covered_from..covered_to]);
    out[frame_len - 2] = (crc >> 8) as u8;
    out[frame_len - 1] = (crc & 0xFF) as u8;

    Ok(&out[..frame_len])
}
