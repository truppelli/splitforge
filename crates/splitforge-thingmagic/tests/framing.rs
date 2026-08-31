//! The frame codec, attacked.
//!
//! Malformed input is the expected case for this crate, not the exceptional one — the
//! module on the other end of the cable may be broken, the cable may be noisy, and a race
//! is not the moment to discover that a bad checksum panics. So these tests spend most of
//! their effort on input that is *wrong*, and the assertions are mostly about what does
//! **not** happen: no panic, no frame reported that was not verified, no read past the
//! end of a buffer.
//!
//! ## The response builder
//!
//! `response()` below constructs frames independently of `frame::decode`, so the layout is
//! asserted by two pieces of code that were written to the same specification rather than
//! by one piece of code agreeing with itself. It does share `crc16`, deliberately: the CRC
//! algorithm is anchored to a captured frame in the crate, and duplicating a CRC by hand in
//! a test file would test the copy.
//!
//! **Sharing it is also this file's blind spot**, and it is worth naming. Every frame here is
//! built with `crc16` and then checked with `crc16`, so these tests pass whatever that
//! function computes — they did pass while it computed CRC-16/CCITT-FALSE, which is not the
//! checksum the module uses. Only `crc::tests` can catch that, and only because it holds bytes
//! a module produced.

use splitforge_thingmagic::{
    Decoded, EncodeError, FrameError, MAX_COMMAND_DATA_LEN, MAX_FRAME_LEN, MAX_RESPONSE_DATA_LEN,
    RESPONSE_HEADER_LEN, Response, SOH, crc16, decode, encode_command, resynchronize,
};

/// Builds a well-formed response frame.
fn response(opcode: u8, status: u16, data: &[u8]) -> Vec<u8> {
    assert!(
        data.len() <= MAX_RESPONSE_DATA_LEN,
        "test frame is over-long"
    );

    let mut frame = Vec::with_capacity(RESPONSE_HEADER_LEN + data.len() + 2);
    frame.push(SOH);
    frame.push(data.len() as u8);
    frame.push(opcode);
    frame.push((status >> 8) as u8);
    frame.push((status & 0xFF) as u8);
    frame.extend_from_slice(data);

    let crc = crc16(&frame[1..]);
    frame.push((crc >> 8) as u8);
    frame.push((crc & 0xFF) as u8);
    frame
}

/// Unwraps a `Decoded` that must be a frame, with a message that says what arrived.
#[track_caller]
fn expect_frame(bytes: &[u8]) -> (Response<'_>, usize) {
    match decode(bytes) {
        Ok(Decoded::Frame { response, consumed }) => (response, consumed),
        other => panic!("expected a frame, got {other:?}"),
    }
}

// --- The happy path, which is the smaller half -----------------------------------------

#[test]
fn a_well_formed_frame_decodes_to_its_parts() {
    let bytes = response(0x22, 0x0000, &[0xDE, 0xAD, 0xBE, 0xEF]);
    let (response, consumed) = expect_frame(&bytes);

    assert_eq!(response.opcode, 0x22);
    assert_eq!(response.status, 0x0000);
    assert_eq!(response.data, &[0xDE, 0xAD, 0xBE, 0xEF]);
    assert!(response.is_ok());
    assert_eq!(consumed, bytes.len());
}

#[test]
fn a_non_zero_status_is_reported_rather_than_refused() {
    // A module answering "no tags found" is not a framing error, and a codec that treated
    // it as one would turn every ordinary empty poll into a resynchronization.
    let bytes = response(0x22, 0x0400, &[]);
    let (response, _) = expect_frame(&bytes);

    assert_eq!(response.status, 0x0400);
    assert!(!response.is_ok());
    assert!(response.data.is_empty());
}

#[test]
fn every_payload_length_the_protocol_allows_round_trips() {
    // 249 cases, which is all of them: user guide § 7.2 caps a response's data at 248.
    for length in 0..=MAX_RESPONSE_DATA_LEN {
        let data: Vec<u8> = (0..length).map(|index| (index % 251) as u8).collect();
        let bytes = response(0x29, 0x0000, &data);
        let (response, consumed) = expect_frame(&bytes);

        assert_eq!(response.data, data.as_slice(), "payload of {length} bytes");
        assert_eq!(consumed, bytes.len(), "payload of {length} bytes");
    }
}

#[test]
fn the_largest_possible_frame_fits_the_advertised_ceiling() {
    let bytes = response(0x00, 0x0000, &[0xAA; MAX_RESPONSE_DATA_LEN]);
    assert_eq!(bytes.len(), MAX_FRAME_LEN);

    let (response, consumed) = expect_frame(&bytes);
    assert_eq!(response.data.len(), MAX_RESPONSE_DATA_LEN);
    assert_eq!(consumed, MAX_FRAME_LEN);
}

#[test]
fn the_payload_borrows_from_the_caller_rather_than_being_copied() {
    // The zero-allocation claim, checked rather than asserted in prose: the decoded
    // payload must point *into* the input buffer.
    let bytes = response(0x22, 0x0000, &[1, 2, 3]);
    let (response, _) = expect_frame(&bytes);

    let payload_start = bytes.as_ptr().wrapping_add(RESPONSE_HEADER_LEN);
    assert!(
        std::ptr::eq(response.data.as_ptr(), payload_start),
        "decoded payload should borrow the input, not copy it"
    );
}

// --- Truncation, which is the ordinary case on a serial port ---------------------------

#[test]
fn every_truncation_of_a_valid_frame_asks_for_more_rather_than_failing() {
    let bytes = response(0x22, 0x0000, &[0x11, 0x22, 0x33, 0x44, 0x55]);

    for cut in 0..bytes.len() {
        match decode(&bytes[..cut]) {
            Ok(Decoded::Incomplete { need_at_least }) => assert!(
                need_at_least > cut,
                "at {cut} bytes the codec asked for {need_at_least}, which it already has"
            ),
            other => panic!("truncation at {cut} bytes should be incomplete, got {other:?}"),
        }
    }

    // And the whole thing still decodes, so the loop above was not vacuous.
    let (response, _) = expect_frame(&bytes);
    assert_eq!(response.data.len(), 5);
}

#[test]
fn an_empty_buffer_is_incomplete_rather_than_unsynchronized() {
    // Nothing has gone wrong yet — the port simply had nothing to give.
    assert!(matches!(decode(&[]), Ok(Decoded::Incomplete { .. })));
}

#[test]
fn a_lone_start_byte_asks_for_the_length_byte() {
    match decode(&[SOH]) {
        Ok(Decoded::Incomplete { need_at_least }) => assert_eq!(need_at_least, 2),
        other => panic!("expected to be asked for the length byte, got {other:?}"),
    }
}

#[test]
fn a_frame_announcing_more_than_arrived_names_the_full_size() {
    // The length byte declares the largest payload a response may legally carry; only the
    // header is here. That is the biggest frame that can still be honestly incomplete.
    let partial = [SOH, MAX_RESPONSE_DATA_LEN as u8, 0x22, 0x00, 0x00];
    match decode(&partial) {
        Ok(Decoded::Incomplete { need_at_least }) => assert_eq!(need_at_least, MAX_FRAME_LEN),
        other => panic!("expected the full frame size, got {other:?}"),
    }
}

#[test]
fn a_response_declaring_more_than_the_protocol_allows_is_rejected_rather_than_awaited() {
    // The seven length bytes a one-byte field can hold but § 7.2 forbids. Each is a frame
    // that can never be completed, so reporting `Incomplete` would park the reader waiting
    // for bytes that cannot come while the stream stays desynchronized. The right answer is
    // to reject now, so the caller resynchronizes on the next 0xFF.
    for declared in (MAX_RESPONSE_DATA_LEN + 1)..=(u8::MAX as usize) {
        let partial = [SOH, declared as u8, 0x22, 0x00, 0x00];
        match decode(&partial) {
            Err(FrameError::DataTooLong { declared: reported }) => {
                assert_eq!(reported, declared, "length byte {declared}");
            }
            other => panic!("length byte {declared} should be refused, got {other:?}"),
        }
    }
}

#[test]
fn a_full_buffer_declaring_an_illegal_length_is_still_refused() {
    // Not only a short-buffer path: even with every byte the frame claims to need present,
    // and a CRC computed over them, the declared length is not one a response may use.
    let mut frame = vec![SOH, 0xFF, 0x22, 0x00, 0x00];
    frame.extend(std::iter::repeat_n(0xAA, u8::MAX as usize));
    let crc = crc16(&frame[1..]);
    frame.push((crc >> 8) as u8);
    frame.push((crc & 0xFF) as u8);

    match decode(&frame) {
        Err(FrameError::DataTooLong { declared }) => assert_eq!(declared, u8::MAX as usize),
        other => panic!("a valid CRC must not rescue an illegal length, got {other:?}"),
    }
}

// --- Corruption ------------------------------------------------------------------------

#[test]
fn a_buffer_that_does_not_start_with_soh_is_rejected_by_name() {
    match decode(&[0x00, 0x01, 0x02]) {
        Err(FrameError::NotSynchronized { found }) => assert_eq!(found, 0x00),
        other => panic!("expected a synchronization error, got {other:?}"),
    }
}

#[test]
fn every_byte_that_is_not_soh_is_refused_as_a_frame_start() {
    for first in 0..=u8::MAX {
        if first == SOH {
            continue;
        }
        assert!(
            matches!(
                decode(&[first, 0x00, 0x22, 0x00, 0x00, 0x00, 0x00]),
                Err(FrameError::NotSynchronized { .. })
            ),
            "0x{first:02X} should not start a frame"
        );
    }
}

#[test]
fn a_corrupted_crc_is_caught() {
    let mut bytes = response(0x22, 0x0000, &[0x01, 0x02]);
    let last = bytes.len() - 1;
    bytes[last] ^= 0xFF;

    match decode(&bytes) {
        Err(FrameError::BadCrc { declared, computed }) => assert_ne!(declared, computed),
        other => panic!("expected a CRC error, got {other:?}"),
    }
}

#[test]
fn any_single_bit_flipped_in_the_covered_bytes_is_caught() {
    // A total claim rather than a statistical one, and it fails loudly if the CRC's coverage
    // is ever narrowed to exclude a field somebody thought was unimportant. The property is
    // checked exhaustively over every bit here and in `crc::tests` rather than borrowed from a
    // catalogue: this protocol's checksum is not CCITT-FALSE and has no published guarantee.
    //
    // The length byte is excluded deliberately: changing it moves the frame's boundaries,
    // so the outcome is legitimately "incomplete" or "bad CRC" depending on direction.
    // That case is covered separately below.
    let original = response(0x22, 0x1234, &[0xA0, 0xB1, 0xC2, 0xD3]);

    for index in 2..original.len() {
        for bit in 0..8 {
            let mut corrupted = original.clone();
            corrupted[index] ^= 1 << bit;

            match decode(&corrupted) {
                Err(FrameError::BadCrc { .. }) => {}
                other => panic!("bit {bit} of byte {index} flipped undetected: {other:?}"),
            }
        }
    }
}

#[test]
fn corrupting_the_length_byte_never_yields_the_original_frame() {
    // Changing the length moves where the CRC is read from, so the codec may report
    // incomplete or a bad CRC. What it must never do is hand back the frame that was
    // actually sent, because that would mean the length field is not protected at all.
    let original = response(0x22, 0x0000, &[0x10, 0x20, 0x30, 0x40, 0x50, 0x60]);
    let (expected, _) = expect_frame(&original);
    let expected_data = expected.data.to_vec();

    // Every value a byte can hold, not only the legal ones — a corrupted length byte is
    // not obliged to land inside the protocol's range, and the values above it are exactly
    // where `DataTooLong` now answers instead of `Incomplete`.
    for length in 0..=(u8::MAX as usize) {
        if length == 6 {
            continue;
        }
        let mut corrupted = original.clone();
        corrupted[1] = length as u8;

        if let Ok(Decoded::Frame { response, .. }) = decode(&corrupted) {
            assert_ne!(
                response.data, expected_data,
                "length {length} produced the original payload from a corrupted frame"
            );
        }
    }
}

// --- 0xFF is not a delimiter -----------------------------------------------------------

#[test]
fn a_start_byte_inside_the_payload_does_not_confuse_the_decoder() {
    // The single most tempting shortcut in a framing parser is to split on the start byte.
    // This payload is nothing but start bytes.
    let data = [SOH; 16];
    let bytes = response(0x22, 0x0000, &data);
    let (response, consumed) = expect_frame(&bytes);

    assert_eq!(response.data, &data);
    assert_eq!(consumed, bytes.len());
}

#[test]
fn a_payload_that_looks_like_a_whole_frame_is_still_just_a_payload() {
    // The payload here is itself a valid, complete frame. A decoder that scanned for
    // structure instead of trusting the length field would find the inner one.
    let inner = response(0x99, 0xBEEF, &[0x01, 0x02, 0x03]);
    let outer = response(0x22, 0x0000, &inner);

    let (response, consumed) = expect_frame(&outer);
    assert_eq!(response.opcode, 0x22);
    assert_eq!(response.data, inner.as_slice());
    assert_eq!(consumed, outer.len());
}

// --- Streams of frames -----------------------------------------------------------------

#[test]
fn back_to_back_frames_decode_one_at_a_time() {
    let first = response(0x22, 0x0000, &[0xAA]);
    let second = response(0x29, 0x0400, &[0xBB, 0xCC]);
    let third = response(0x2F, 0x0000, &[]);

    let mut stream = Vec::new();
    stream.extend_from_slice(&first);
    stream.extend_from_slice(&second);
    stream.extend_from_slice(&third);

    let mut offset = 0;
    let mut opcodes = Vec::new();
    while offset < stream.len() {
        let (response, consumed) = expect_frame(&stream[offset..]);
        opcodes.push(response.opcode);
        offset += consumed;
    }

    assert_eq!(opcodes, vec![0x22, 0x29, 0x2F]);
    assert_eq!(
        offset,
        stream.len(),
        "the stream should be consumed exactly"
    );
}

#[test]
fn trailing_bytes_are_left_for_the_next_call() {
    let frame = response(0x22, 0x0000, &[0x01]);
    let mut buffer = frame.clone();
    buffer.extend_from_slice(&[0xDE, 0xAD]);

    let (_, consumed) = expect_frame(&buffer);
    assert_eq!(consumed, frame.len());
    assert_eq!(&buffer[consumed..], &[0xDE, 0xAD]);
}

// --- Resynchronization -----------------------------------------------------------------

#[test]
fn resynchronizing_finds_the_next_candidate_start() {
    let mut buffer = vec![SOH, 0x01, 0x02];
    let frame = response(0x22, 0x0000, &[0x07]);
    buffer.extend_from_slice(&frame);

    let offset = resynchronize(&buffer).expect("a candidate start byte follows the garbage");
    assert_eq!(offset, 3);

    let (response, _) = expect_frame(&buffer[offset..]);
    assert_eq!(response.opcode, 0x22);
}

#[test]
fn resynchronizing_reports_nothing_when_no_candidate_remains() {
    assert_eq!(resynchronize(&[SOH, 0x01, 0x02, 0x03]), None);
    assert_eq!(resynchronize(&[]), None);
    assert_eq!(resynchronize(&[SOH]), None);
}

#[test]
fn a_false_start_byte_is_rejected_and_recovered_from() {
    // Garbage containing a 0xFF, then a real frame. Resynchronizing onto the false start
    // must fail on the CRC — not be accepted, and not stop the search.
    let real = response(0x22, 0x0000, &[0x42]);
    let mut buffer = vec![SOH, 0x02, 0xFF, 0xFF];
    buffer.extend_from_slice(&real);

    // The buffer begins with a plausible-looking frame that is not one.
    assert!(matches!(decode(&buffer), Err(FrameError::BadCrc { .. })));

    // Walking candidate start bytes finds the real frame.
    let mut offset = 0;
    let found = loop {
        match resynchronize(&buffer[offset..]) {
            Some(step) => {
                offset += step;
                if let Ok(Decoded::Frame { response, .. }) = decode(&buffer[offset..]) {
                    break Some(response.opcode);
                }
            }
            None => break None,
        }
    };

    assert_eq!(found, Some(0x22));
}

// --- Encoding --------------------------------------------------------------------------

#[test]
fn a_command_encodes_to_the_documented_layout() {
    let mut buffer = [0_u8; MAX_FRAME_LEN];
    let frame = encode_command(0x03, &[0x01, 0x02], &mut buffer).expect("a short command fits");

    assert_eq!(frame[0], SOH);
    assert_eq!(frame[1], 2, "length counts data bytes only");
    assert_eq!(frame[2], 0x03);
    assert_eq!(&frame[3..5], &[0x01, 0x02]);
    assert_eq!(frame.len(), 7, "3 header + 2 data + 2 crc");

    let crc = crc16(&frame[1..frame.len() - 2]);
    assert_eq!(frame[frame.len() - 2], (crc >> 8) as u8);
    assert_eq!(frame[frame.len() - 1], (crc & 0xFF) as u8);
}

#[test]
fn an_empty_command_is_valid() {
    let mut buffer = [0_u8; MAX_FRAME_LEN];
    let frame = encode_command(0x03, &[], &mut buffer).expect("an empty command fits");

    assert_eq!(frame[1], 0);
    assert_eq!(frame.len(), 5);
}

#[test]
fn a_command_longer_than_the_protocol_allows_is_refused() {
    // 251 bytes fits the length field and does not fit § 7.1, which is the whole point:
    // the limit is the documented one, not the one the field could express.
    let mut buffer = [0_u8; 1024];
    let oversized = [0_u8; MAX_COMMAND_DATA_LEN + 1];

    match encode_command(0x03, &oversized, &mut buffer) {
        Err(EncodeError::DataTooLong { requested }) => {
            assert_eq!(requested, MAX_COMMAND_DATA_LEN + 1);
        }
        other => panic!("expected the documented cap to be the limit, got {other:?}"),
    }
}

#[test]
fn a_buffer_too_small_for_the_frame_is_refused_rather_than_truncated() {
    let mut buffer = [0_u8; 4];
    match encode_command(0x03, &[0x01, 0x02], &mut buffer) {
        Err(EncodeError::BufferTooSmall { needed, available }) => {
            assert_eq!(needed, 7);
            assert_eq!(available, 4);
        }
        other => panic!("expected a buffer-size error, got {other:?}"),
    }
}

#[test]
fn the_advertised_ceiling_is_large_enough_for_any_command() {
    let mut buffer = [0_u8; MAX_FRAME_LEN];
    let frame = encode_command(0x03, &[0xAB; MAX_COMMAND_DATA_LEN], &mut buffer)
        .expect("MAX_FRAME_LEN should fit the largest command");
    assert!(frame.len() <= MAX_FRAME_LEN);
}

#[test]
fn both_directions_reach_the_same_ceiling_on_the_wire() {
    // A command carries two more data bytes than a response and spends two fewer on a
    // status word, so the largest legal frame in either direction is the same 255 bytes.
    // That is what lets one buffer size serve both, and the crate asserts it at compile
    // time; this checks the encoder and the frame builder actually produce it.
    let mut buffer = [0_u8; MAX_FRAME_LEN];
    let command = encode_command(0x03, &[0xAB; MAX_COMMAND_DATA_LEN], &mut buffer)
        .expect("the largest legal command must encode");
    let response = response(0x03, 0x0000, &[0xAB; MAX_RESPONSE_DATA_LEN]);

    assert_eq!(command.len(), MAX_FRAME_LEN);
    assert_eq!(response.len(), MAX_FRAME_LEN);
    assert_eq!(MAX_FRAME_LEN, 255);
}

// --- The blunt instrument --------------------------------------------------------------

#[test]
fn no_byte_sequence_makes_the_decoder_panic() {
    // A cheap deterministic sweep rather than a fuzzer: an xorshift PRNG with a fixed
    // seed, every buffer decoded at every truncation. The assertion is that control
    // returns at all — a panic here fails the test by unwinding.
    let mut state = 0x2545_F491_4F6C_DD1D_u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };

    for _ in 0..2_000 {
        let length = (next() % 300) as usize;
        let mut buffer: Vec<u8> = (0..length).map(|_| (next() & 0xFF) as u8).collect();

        // Half the buffers start with SOH, so the interesting paths are actually reached
        // instead of every case bailing out at the first byte.
        if !buffer.is_empty() && next() % 2 == 0 {
            buffer[0] = SOH;
        }

        for cut in 0..=buffer.len() {
            let _ = decode(&buffer[..cut]);
            let _ = resynchronize(&buffer[..cut]);
        }
    }
}

#[test]
fn a_frame_is_never_reported_without_a_matching_crc() {
    // The property that matters most, stated directly: whatever `decode` accepts, the CRC
    // over the covered range equals the CRC in the frame. Checked against random input so
    // that it holds for accidentally-valid frames too, not only constructed ones.
    let mut state = 0x9E37_79B9_7F4A_7C15_u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };

    let mut accepted = 0_u32;
    for _ in 0..20_000 {
        let length = (next() % 40) as usize + 7;
        let mut buffer: Vec<u8> = (0..length).map(|_| (next() & 0xFF) as u8).collect();
        buffer[0] = SOH;
        // Keep the declared length inside the buffer often enough to reach the CRC check.
        buffer[1] = ((length - 7) % 34) as u8;

        if let Ok(Decoded::Frame { consumed, .. }) = decode(&buffer) {
            accepted += 1;
            let covered = crc16(&buffer[1..consumed - 2]);
            let declared = ((buffer[consumed - 2] as u16) << 8) | buffer[consumed - 1] as u16;
            assert_eq!(
                covered, declared,
                "accepted a frame whose CRC does not match"
            );
        }
    }

    // Random bytes almost never form a valid CRC, so this loop is expected to accept
    // nothing. Asserting that keeps the test honest about what it covered: if the codec
    // ever starts accepting garbage, this number moves.
    assert_eq!(
        accepted, 0,
        "random input produced {accepted} frames the codec considered valid"
    );
}
