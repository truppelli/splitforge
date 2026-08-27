//! CRC-16/CCITT-FALSE, as the ThingMagic serial protocol uses it.
//!
//! Kept in its own module for one reason: of everything in this crate, this is the part
//! most likely to be *wrong in a way no test here can detect*. The algorithm below is
//! pinned to a published check vector and is certainly CRC-16/CCITT-FALSE. Whether the
//! module on the end of the cable computes its CRC over exactly the bytes
//! [`crate::frame`] hands to it is a different question, and it is answered by a capture,
//! not by this file. See [`crate::frame`]'s note on coverage.

/// The generator polynomial, `x^16 + x^12 + x^5 + 1` in its normal (non-reversed) form.
const POLYNOMIAL: u16 = 0x1021;

/// The initial register value. `CCITT-FALSE` is the variant that seeds with all ones; the
/// variant seeding with zero is a different checksum that agrees on no useful input.
const INIT: u16 = 0xFFFF;

/// Computes the CRC-16/CCITT-FALSE of `bytes`.
///
/// Bitwise rather than table-driven. The vendor's own implementations use a 16-entry
/// nibble table, which is the same function computed faster; at the frame sizes this
/// protocol allows — 262 bytes at the absolute maximum — the table buys nothing worth the
/// extra thing to get wrong.
///
/// No reflection of input or output, and no final XOR. Those three parameters are what
/// separate this from the half-dozen other checksums that also call themselves "CRC-16",
/// so they are stated here rather than left implicit in the loop.
#[must_use]
pub const fn crc16(bytes: &[u8]) -> u16 {
    let mut crc = INIT;
    let mut index = 0;

    while index < bytes.len() {
        crc ^= (bytes[index] as u16) << 8;

        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 0x8000 == 0 {
                crc << 1
            } else {
                (crc << 1) ^ POLYNOMIAL
            };
            bit += 1;
        }

        index += 1;
    }

    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The check value every CRC catalogue publishes for CRC-16/CCITT-FALSE.
    ///
    /// This is the one test in this crate that is anchored to something outside it. Every
    /// other CRC assertion here is self-consistent — it would still pass if the polynomial
    /// were wrong, because the same wrong function would compute both sides. This one
    /// would not.
    #[test]
    fn the_published_check_vector_matches() {
        assert_eq!(crc16(b"123456789"), 0x29B1);
    }

    #[test]
    fn the_empty_input_returns_the_seed() {
        // Not a triviality: a seed of zero is the single most common way to implement the
        // wrong CRC-16, and it is invisible on every input except this one.
        assert_eq!(crc16(&[]), 0xFFFF);
    }

    #[test]
    fn a_single_bit_changes_the_result() {
        assert_ne!(crc16(&[0x00]), crc16(&[0x01]));
        assert_ne!(crc16(&[0x00, 0x00]), crc16(&[0x00, 0x80]));
    }

    #[test]
    fn leading_zeroes_are_not_transparent() {
        // A CRC seeded with zero cannot tell these apart. CCITT-FALSE can, and this is
        // the property that makes a truncated frame's leading padding detectable.
        assert_ne!(crc16(&[0x00, 0x01]), crc16(&[0x01]));
    }

    #[test]
    fn it_is_usable_in_a_const_context() {
        // Proves the function stays `const`, which is what lets test frames be built at
        // compile time and keeps it callable from a static table later.
        const CHECK: u16 = crc16(b"123456789");
        assert_eq!(CHECK, 0x29B1);
    }
}
