//! The ThingMagic serial protocol's CRC-16.
//!
//! Kept in its own module for one reason: of everything in this crate, this is the part most
//! likely to be *wrong in a way no test here can detect*. It was, and this module is the
//! correction — see [Why this is not CCITT-FALSE](#why-this-is-not-ccitt-false).
//!
//! # The algorithm
//!
//! A 16-bit register seeded with `0xFFFF`, fed four bits at a time, most significant nibble
//! first:
//!
//! ```text
//! crc = ((crc << 4) | nibble) ^ table[crc >> 12]
//! ```
//!
//! where `table[i]` is the carry-less product `i * 0x1021`. That product is computed here from
//! [`POLYNOMIAL`] rather than stored as sixteen constants. The two agree for every `i` in
//! `0..16` and a test asserts it; computing is preferred because it derives the values from the
//! polynomial instead of restating them, so a change to one cannot leave a stale table behind.
//!
//! # Why this is not CCITT-FALSE
//!
//! Because the data nibble enters at the **bottom** of the register instead of being folded
//! into the table index at the top. Written out, with `T[i] = i * 0x1021`:
//!
//! ```text
//! CCITT-FALSE  crc = (crc << 4) ^ T[(crc >> 12) ^ nibble]  =  (crc << 4) ^ T[crc >> 12] ^ T[nibble]
//! ThingMagic   crc = ((crc << 4) | nibble) ^ T[crc >> 12]  =  (crc << 4) ^ T[crc >> 12] ^ nibble
//! ```
//!
//! `T` is linear, which is what lets the first line be rearranged; the low four bits of
//! `crc << 4` are zero, which is what makes the `|` in the second line an exclusive-or. So the
//! two differ in exactly one term — the standard mixes in `T[nibble]`, this one mixes in
//! `nibble`. That is a small difference in a diagram and a total difference in the output.
//!
//! It is deliberate on the vendor's part rather than a defect. MercuryAPI's own
//! `serial_reader_l3.c` labels it *"ThingMagic-mutated CRC used for messages. Notably, not a
//! CCITT CRC-16, though it looks close."* The user guide's § 7.3 calls it "CCITT CRC-16" and
//! gives no polynomial, no seed, and no worked example — which is how this crate came to
//! implement the checksum the name describes rather than the one the module computes.
//!
//! See `docs/readers/vendor-documents.md` for the sources and the whole account.
//!
//! # What anchors it
//!
//! A real frame, captured from real hardware, carrying its own CRC: [`CAPTURED_FRAME`]. Every
//! other assertion in this crate is self-consistent — the frame tests build a frame with
//! [`crc16`] and then check it with [`crc16`], so they pass just as cleanly with the wrong
//! function, and for one release they did.

/// The generator polynomial, `x^16 + x^12 + x^5 + 1` in its normal (non-reversed) form.
///
/// The same polynomial CCITT-FALSE uses. Sharing it is why the two functions look alike, and
/// why the vendor's documentation calls this one by the other's name.
const POLYNOMIAL: u16 = 0x1021;

/// The initial register value: all ones.
const INIT: u16 = 0xFFFF;

/// One entry of the vendor's lookup table, computed rather than stored.
///
/// `table(i)` is the carry-less product of `i` and [`POLYNOMIAL`] — the polynomial shifted
/// once for each set bit of `i`, exclusive-ored together. `i` never exceeds 15, so the largest
/// shift is three places and the product cannot overflow 16 bits.
#[must_use]
const fn table(index: u16) -> u16 {
    let mut product = 0;
    let mut bit = 0;

    while bit < 4 {
        if index & (1 << bit) != 0 {
            product ^= POLYNOMIAL << bit;
        }
        bit += 1;
    }

    product
}

/// Folds one four-bit nibble into the register.
#[must_use]
const fn feed(crc: u16, nibble: u8) -> u16 {
    // `crc << 4` leaves its low four bits clear, so this `|` is an exclusive-or and the whole
    // function is linear over GF(2). That is what lets the comparison with CCITT-FALSE in the
    // module documentation be rearranged the way it is.
    ((crc << 4) | nibble as u16) ^ table(crc >> 12)
}

/// Computes the ThingMagic serial protocol's CRC-16 over `bytes`.
///
/// Nibble at a time, high nibble first, seeded with [`INIT`]. Not table-driven: at the frame
/// sizes this protocol allows — 255 bytes at the absolute maximum — a lookup table buys nothing
/// worth the extra thing to keep in step with [`POLYNOMIAL`].
///
/// **This is not CRC-16/CCITT-FALSE**, whatever the user guide's § 7.3 calls it. See the
/// [module documentation](self) for the one term that differs.
#[must_use]
pub const fn crc16(bytes: &[u8]) -> u16 {
    let mut crc = INIT;
    let mut index = 0;

    while index < bytes.len() {
        crc = feed(crc, bytes[index] >> 4);
        crc = feed(crc, bytes[index] & 0x0F);
        index += 1;
    }

    crc
}

/// A complete reader-to-host frame captured from a physical ThingMagic module, carrying the
/// CRC that module computed.
///
/// This is the only thing in this crate anchored outside it, and it is what made the
/// correction in this module possible without hardware. It is a `READ_TAG_ID_MULTIPLE`
/// (opcode `0x22`) response published as a worked example in SparkFun's
/// `SparkFun_Simultaneous_RFID_Tag_Reader_Library`, which annotates its final two bytes,
/// `56 1D`, as the message CRC.
///
/// **It is an M6e frame, not an M7e-Pico one.** The modules differ; the serial protocol in
/// user guide § 7.1–7.3 does not, and the framing this crate implements decodes it. So it is
/// evidence about the protocol rather than about the Pico, and it is recorded as such.
pub const CAPTURED_FRAME: [u8; 47] = [
    0xFF, 0x28, 0x22, 0x00, 0x00, 0x10, 0x00, 0x1B, 0x01, 0xFF, 0x01, 0x01, 0xC4, 0x11, 0x0E, 0x16,
    0x40, 0x00, 0x00, 0x01, 0x27, 0x00, 0x00, 0x05, 0x00, 0x00, 0x0F, 0x00, 0x80, 0x30, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x15, 0x45, 0xE9, 0x4A, 0x56, 0x1D,
];

#[cfg(test)]
mod tests {
    use super::*;

    /// The test that would have caught the original defect, and the only one here that could
    /// have.
    ///
    /// A module computed these bytes. Nothing in this repository participated in producing
    /// them, so agreeing with them is a claim about the protocol rather than a claim about
    /// this crate's internal consistency.
    #[test]
    fn the_captured_frame_matches_the_crc_the_module_computed() {
        let frame = &CAPTURED_FRAME;
        let total = frame[1] as usize + 7;
        assert_eq!(total, frame.len(), "the length field describes the frame");

        // Covered: everything between the header and the CRC — length, opcode, status, data.
        let covered = &frame[1..total - 2];
        let expected = u16::from_be_bytes([frame[total - 2], frame[total - 1]]);

        assert_eq!(expected, 0x561D);
        assert_eq!(crc16(covered), expected);
    }

    /// CRC-16/CCITT-FALSE answers `0xF542` for the frame above; the module said `0x561D`.
    ///
    /// Pinned with a name that says what it is for. The point is not that `0xF542` is
    /// interesting, but that anyone reintroducing the standard algorithm — because § 7.3 calls
    /// it "CCITT CRC-16", which is exactly how it got here the first time — fails this
    /// immediately rather than at a reader.
    #[test]
    fn the_ccitt_false_answer_is_not_this_functions_answer() {
        fn ccitt_false(bytes: &[u8]) -> u16 {
            let mut crc: u16 = 0xFFFF;
            for &byte in bytes {
                crc ^= (byte as u16) << 8;
                for _ in 0..8 {
                    crc = if crc & 0x8000 == 0 {
                        crc << 1
                    } else {
                        (crc << 1) ^ POLYNOMIAL
                    };
                }
            }
            crc
        }

        let total = CAPTURED_FRAME[1] as usize + 7;
        let covered = &CAPTURED_FRAME[1..total - 2];

        assert_eq!(ccitt_false(covered), 0xF542);
        assert_eq!(crc16(covered), 0x561D);
        assert_eq!(
            ccitt_false(b"123456789"),
            0x29B1,
            "still the catalogue's function"
        );
        assert_ne!(crc16(b"123456789"), 0x29B1, "and still not ours");
    }

    /// The value MercuryAPI's `tm_crc` produces for the catalogue's usual input.
    ///
    /// Not a published check vector — no catalogue lists this function — so it is recorded for
    /// what it is: the answer the vendor's own implementation gives, transcribed from
    /// `serial_reader_l3.c` and computed independently of the code above.
    #[test]
    fn the_vendor_implementation_agrees_on_the_usual_input() {
        assert_eq!(crc16(b"123456789"), 0xA69D);
    }

    #[test]
    fn the_closed_form_reproduces_the_vendors_table() {
        // The sixteen constants MercuryAPI and SparkFun both carry, written here as the
        // assertion's expected side only — the implementation derives them from POLYNOMIAL.
        const VENDOR_TABLE: [u16; 16] = [
            0x0000, 0x1021, 0x2042, 0x3063, 0x4084, 0x50A5, 0x60C6, 0x70E7, 0x8108, 0x9129, 0xA14A,
            0xB16B, 0xC18C, 0xD1AD, 0xE1CE, 0xF1EF,
        ];

        for (index, &expected) in VENDOR_TABLE.iter().enumerate() {
            assert_eq!(table(index as u16), expected, "table[{index}]");
        }
    }

    #[test]
    fn the_empty_input_returns_the_seed() {
        // Not a triviality: a seed of zero is the single most common way to implement the
        // wrong CRC-16, and it is invisible on every input except this one.
        assert_eq!(crc16(&[]), 0xFFFF);
    }

    #[test]
    fn every_single_bit_error_is_detected() {
        // The function is linear, but linearity does not by itself promise this — and this
        // crate no longer has a catalogue's guarantee to borrow, so the property is checked
        // rather than asserted. Exhaustive over every bit of every length up to 40.
        for len in 1..=40usize {
            let message: Vec<u8> = (0..len)
                .map(|i| (i as u8).wrapping_mul(37).wrapping_add(11))
                .collect();
            let baseline = crc16(&message);

            for byte in 0..len {
                for bit in 0..8 {
                    let mut corrupted = message.clone();
                    corrupted[byte] ^= 1 << bit;
                    assert_ne!(
                        crc16(&corrupted),
                        baseline,
                        "flipping bit {bit} of byte {byte} in a {len}-byte message went unnoticed"
                    );
                }
            }
        }
    }

    #[test]
    fn leading_zeroes_are_not_transparent() {
        // A CRC seeded with zero cannot tell these apart. This one can, and that is the
        // property that makes a truncated frame's leading padding detectable.
        assert_ne!(crc16(&[0x00, 0x01]), crc16(&[0x01]));
    }

    #[test]
    fn it_is_usable_in_a_const_context() {
        // Proves the function stays `const`, which is what lets test frames be built at
        // compile time and keeps it callable from a static table later.
        const CHECK: u16 = crc16(b"123456789");
        assert_eq!(CHECK, 0xA69D);
    }
}
