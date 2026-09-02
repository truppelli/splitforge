//! Turning a byte stream back into frames.
//!
//! A serial port delivers whatever happened to be in the kernel buffer: half a frame, three
//! frames, a frame plus the first byte of the next one, or nothing at all. [`frame::decode`]
//! answers questions about one buffer; this is the thing that keeps a buffer to ask about.
//!
//! Still no I/O. [`Reassembler`] is fed `&[u8]` by whatever is doing the reading, which is
//! what lets the connection lifecycle above it be tested against a fake port
//! ([`crate::provider`]).
//!
//! # Two properties this has to hold
//!
//! **It always makes progress.** Every path either consumes bytes or asks for more, and no
//! input can leave the cursor where it was. A reassembler that can stall is one that stops
//! recording the race and never says why.
//!
//! **It cannot grow without bound.** A frame is at most [`frame::MAX_FRAME_LEN`] bytes, so a
//! buffer that is waiting on one never needs to hold more than that. Garbage does not
//! accumulate, because a byte that cannot begin a frame is dropped rather than kept.

use crate::frame::{self, Decoded, FrameError, MAX_FRAME_LEN, Response};

/// What a [`Reassembler`] has seen since it was created.
///
/// Errors are counted rather than propagated: on a serial line a bad frame is a fact about
/// the afternoon, not a reason to stop. The counts are what make "the line is noisy" a
/// measurement instead of an impression, and they are what
/// [architecture § 4](../../../docs/architecture.md#what-survived-means) means by the gap
/// between reads received and reads persisted being monitored.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    /// Frames that decoded and passed their CRC.
    pub frames: u64,
    /// Buffers that began with something other than [`frame::SOH`].
    pub not_synchronized: u64,
    /// Frames whose checksum did not match their contents.
    pub bad_crc: u64,
    /// Length bytes above what a response may declare.
    pub data_too_long: u64,
    /// Bytes thrown away while looking for the next plausible frame start.
    pub discarded_bytes: u64,
}

impl Stats {
    /// Every error, however it presented.
    #[must_use]
    pub const fn errors(&self) -> u64 {
        self.not_synchronized + self.bad_crc + self.data_too_long
    }
}

/// Holds partial frames between reads, and hands over whole ones.
#[derive(Debug, Default)]
pub struct Reassembler {
    buffer: Vec<u8>,
    stats: Stats,
}

impl Reassembler {
    /// A reassembler with room for one whole frame, which is all it can ever need.
    #[must_use]
    pub fn new() -> Self {
        Self {
            buffer: Vec::with_capacity(MAX_FRAME_LEN),
            stats: Stats::default(),
        }
    }

    /// What this reassembler has seen.
    #[must_use]
    pub const fn stats(&self) -> Stats {
        self.stats
    }

    /// Bytes currently held back, waiting for the rest of their frame.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.buffer.len()
    }

    /// Feeds `bytes` in, calling `on_frame` once for every complete frame that results.
    ///
    /// `on_frame` borrows the frame rather than owning it, because the payload borrows this
    /// reassembler's buffer — copying it here would allocate on the read path for every
    /// read, including the ones a caller is going to discard.
    ///
    /// Errors are counted, not returned. A caller that wants to react to them reads
    /// [`Self::stats`] afterwards; a caller that does not is still correct, which is the
    /// point.
    pub fn feed<F>(&mut self, bytes: &[u8], mut on_frame: F)
    where
        F: FnMut(&Response<'_>),
    {
        self.buffer.extend_from_slice(bytes);

        // Counters are accumulated locally and written back at the end. A decoded frame
        // borrows `self.buffer`, so `self.stats` cannot be touched while one is alive.
        let mut cursor = 0_usize;
        let mut stats = Stats::default();

        loop {
            let remaining = &self.buffer[cursor..];
            if remaining.is_empty() {
                break;
            }

            match frame::decode(remaining) {
                Ok(Decoded::Frame { response, consumed }) => {
                    stats.frames += 1;
                    on_frame(&response);
                    cursor += consumed;
                }
                Ok(Decoded::Incomplete { .. }) => break,
                Err(error) => {
                    match error {
                        FrameError::NotSynchronized { .. } => stats.not_synchronized += 1,
                        FrameError::BadCrc { .. } => stats.bad_crc += 1,
                        FrameError::DataTooLong { .. } => stats.data_too_long += 1,
                    }

                    // Progress is guaranteed here, and that is the whole reason this is a
                    // separate step rather than `cursor += 1`. `resynchronize` skips index
                    // zero, so it returns an offset of at least one or nothing at all, and
                    // nothing at all means the rest of the buffer holds no candidate and
                    // can go.
                    let skip = frame::resynchronize(remaining).unwrap_or(remaining.len());
                    stats.discarded_bytes += skip as u64;
                    cursor += skip;
                }
            }
        }

        self.buffer.drain(..cursor);
        self.stats.frames += stats.frames;
        self.stats.not_synchronized += stats.not_synchronized;
        self.stats.bad_crc += stats.bad_crc;
        self.stats.data_too_long += stats.data_too_long;
        self.stats.discarded_bytes += stats.discarded_bytes;
    }

    /// Drops any partial frame, keeping the statistics.
    ///
    /// Called when a port is reopened: the bytes held back belong to a connection that no
    /// longer exists, and prepending them to the new one's first read would manufacture a
    /// frame out of two different sessions.
    pub fn reset(&mut self) {
        self.buffer.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crc::crc16;
    use crate::frame::{RESPONSE_HEADER_LEN, SOH};

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

    /// Collects the opcodes of every frame a feed produces.
    fn feed(reassembler: &mut Reassembler, bytes: &[u8]) -> Vec<u8> {
        let mut seen = Vec::new();
        reassembler.feed(bytes, |response| seen.push(response.opcode));
        seen
    }

    #[test]
    fn a_frame_split_across_reads_is_reassembled() {
        let bytes = response(0x22, &[1, 2, 3, 4]);
        let mut reassembler = Reassembler::new();

        for cut in 1..bytes.len() {
            let mut fresh = Reassembler::new();
            assert!(feed(&mut fresh, &bytes[..cut]).is_empty(), "cut at {cut}");
            assert_eq!(feed(&mut fresh, &bytes[cut..]), vec![0x22], "cut at {cut}");
        }

        // And byte at a time, which is the pathological version of the same thing.
        for index in 0..bytes.len() - 1 {
            assert!(feed(&mut reassembler, &bytes[index..=index]).is_empty());
        }
        assert_eq!(feed(&mut reassembler, &bytes[bytes.len() - 1..]), vec![0x22]);
    }

    #[test]
    fn several_frames_in_one_read_all_come_out() {
        let mut stream = response(0x01, &[]);
        stream.extend(response(0x02, &[0xAA]));
        stream.extend(response(0x03, &[0xBB; 40]));

        let mut reassembler = Reassembler::new();
        assert_eq!(feed(&mut reassembler, &stream), vec![0x01, 0x02, 0x03]);
        assert_eq!(reassembler.pending(), 0);
        assert_eq!(reassembler.stats().frames, 3);
        assert_eq!(reassembler.stats().errors(), 0);
    }

    #[test]
    fn a_trailing_partial_frame_is_held_not_dropped() {
        let mut stream = response(0x01, &[0x11]);
        let next = response(0x02, &[0x22, 0x33]);
        stream.extend_from_slice(&next[..3]);

        let mut reassembler = Reassembler::new();
        assert_eq!(feed(&mut reassembler, &stream), vec![0x01]);
        assert_eq!(reassembler.pending(), 3);
        assert_eq!(feed(&mut reassembler, &next[3..]), vec![0x02]);
        assert_eq!(reassembler.pending(), 0);
    }

    #[test]
    fn garbage_before_a_frame_is_discarded_and_the_frame_still_arrives() {
        let mut stream = vec![0x00, 0x11, 0x22, 0x33];
        stream.extend(response(0x42, &[0x99]));

        let mut reassembler = Reassembler::new();
        assert_eq!(feed(&mut reassembler, &stream), vec![0x42]);
        assert!(reassembler.stats().not_synchronized > 0);
        assert!(reassembler.stats().discarded_bytes >= 4);
    }

    #[test]
    fn a_corrupted_frame_does_not_swallow_the_one_behind_it() {
        // The classic failure: a bad frame consumes its own length, taking the next frame's
        // header with it, and the stream never recovers.
        let mut corrupted = response(0x01, &[0x11, 0x22]);
        let last = corrupted.len() - 1;
        corrupted[last] ^= 0xFF;
        let mut stream = corrupted;
        stream.extend(response(0x02, &[0x33]));

        let mut reassembler = Reassembler::new();
        assert_eq!(feed(&mut reassembler, &stream), vec![0x02]);
        assert_eq!(reassembler.stats().bad_crc, 1);
    }

    #[test]
    fn an_illegal_length_byte_is_not_waited_on() {
        // 0xFF cannot be a response's data length, so this is not a frame that might yet
        // complete — it is one that never will. Holding it would stall the stream.
        let mut stream = vec![SOH, 0xFF, 0x22, 0x00, 0x00];
        stream.extend(response(0x07, &[0x01]));

        let mut reassembler = Reassembler::new();
        assert_eq!(feed(&mut reassembler, &stream), vec![0x07]);
        assert_eq!(reassembler.stats().data_too_long, 1);
    }

    #[test]
    fn a_payload_full_of_start_bytes_is_not_mistaken_for_frames() {
        let bytes = response(0x22, &[SOH; 32]);
        let mut reassembler = Reassembler::new();
        assert_eq!(feed(&mut reassembler, &bytes), vec![0x22]);
        assert_eq!(reassembler.stats().errors(), 0);
    }

    #[test]
    fn pure_noise_never_stalls_and_never_accumulates() {
        // The progress property, stated as a test: whatever goes in, the buffer afterwards
        // is smaller than one frame. A reassembler that can stall would fail this by
        // holding everything it was ever given.
        let mut state = 0x1234_5678_9ABC_DEF0_u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };

        let mut reassembler = Reassembler::new();
        for _ in 0..1_000 {
            let length = (next() % 64) as usize;
            let chunk: Vec<u8> = (0..length).map(|_| (next() & 0xFF) as u8).collect();
            reassembler.feed(&chunk, |_| {});
            assert!(
                reassembler.pending() < MAX_FRAME_LEN,
                "buffer grew to {} bytes",
                reassembler.pending()
            );
        }
    }

    #[test]
    fn reset_drops_a_partial_frame_so_two_sessions_cannot_be_spliced() {
        let bytes = response(0x22, &[1, 2, 3, 4]);
        let mut reassembler = Reassembler::new();

        assert!(feed(&mut reassembler, &bytes[..4]).is_empty());
        assert!(reassembler.pending() > 0);

        reassembler.reset();
        assert_eq!(reassembler.pending(), 0);

        // The tail of the old frame is now just noise, and the next whole frame still lands.
        let seen = feed(&mut reassembler, &bytes[4..]);
        assert!(seen.is_empty());
        assert_eq!(feed(&mut reassembler, &bytes), vec![0x22]);
    }

    #[test]
    fn statistics_accumulate_across_feeds() {
        let mut reassembler = Reassembler::new();
        reassembler.feed(&response(0x01, &[]), |_| {});
        reassembler.feed(&[0x00, 0x00], |_| {});
        reassembler.feed(&response(0x02, &[]), |_| {});

        let stats = reassembler.stats();
        assert_eq!(stats.frames, 2);
        assert_eq!(stats.not_synchronized, 1);
        assert!(stats.discarded_bytes >= 2);
    }
}
