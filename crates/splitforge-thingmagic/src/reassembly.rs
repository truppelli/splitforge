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
//! # Three properties this has to hold
//!
//! **It always makes progress.** Every path either consumes bytes or asks for more, and no
//! input can leave the cursor where it was. A reassembler that can stall is one that stops
//! recording the race and never says why.
//!
//! **It cannot grow without bound.** A frame is at most [`frame::MAX_FRAME_LEN`] bytes, so a
//! buffer that is waiting on one never needs to hold more than that. Garbage does not
//! accumulate, because a byte that cannot begin a frame is dropped rather than kept.
//!
//! **It never looks inside a frame it is waiting for.** A header with a legal length and too
//! few bytes behind it is either the start of a real frame or a `0xFF` that happened to be
//! followed by a small number. Nothing already in the buffer can tell those apart. Even a
//! CRC-verified frame found inside the waiting one proves nothing, because it lies entirely
//! within the bytes the waiting frame claims, so it could be that frame's payload. A tag's EPC
//! is written by whoever wrote the tag, so that payload can be built on purpose. Only more
//! bytes settle it, or the certainty that no more are coming, which is [`Reassembler::flush`].

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
    /// Partial frames given up on because nothing more was coming for them.
    ///
    /// A connection that ends partway through a frame leaves one of these, and so does a
    /// `0xFF` in noise that happened to be followed by a legal length.
    pub abandoned: u64,
    /// Bytes thrown away while looking for the next plausible frame start.
    pub discarded_bytes: u64,
}

impl Stats {
    /// Every error, however it presented.
    #[must_use]
    pub const fn errors(&self) -> u64 {
        self.not_synchronized + self.bad_crc + self.data_too_long + self.abandoned
    }
}

/// What to do with a candidate frame that has a legal length and too few bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Incomplete {
    /// Hold it, and everything behind it, until more bytes arrive.
    Wait,
    /// Nothing more is coming, so it was never a frame.
    GiveUp,
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
        self.scan(&mut on_frame, Incomplete::Wait);
    }

    /// Settles everything held, because nothing more is coming for it.
    ///
    /// Call it when the line has been quiet for a whole read timeout, and when the connection
    /// ends. A frame in flight does not pause. Its bytes follow one another at the line rate,
    /// about a millisecond apart even at the module's slowest 9 600 baud, and a read timeout is
    /// hundreds of milliseconds with no byte at all. So a partial frame that sat through one is
    /// not waiting for the rest of itself. Whole frames found behind it are handed over, and
    /// the buffer is empty afterwards.
    ///
    /// **This is what bounds the cost of waiting.** A false header claiming 200 bytes holds the
    /// real frames behind it until 200 bytes have arrived, which in a burst is milliseconds. If
    /// the burst ends first, those frames would otherwise sit in the buffer until the next
    /// runner. Their receipt time is the authoritative timestamp for this module, so holding
    /// them that long records them late. With this, the most they can be held is one read
    /// timeout.
    ///
    /// **What it cannot tell apart**, and the choice made about it. A partial frame given up
    /// on here is either a false header or a real frame that was cut off. If it was cut off
    /// just after a frame embedded in its payload, that embedded frame is handed over. Holding
    /// it back instead would lose every real frame behind every false header. The embedded
    /// case needs a crafted EPC and a cut at that exact byte, and nobody can time a pulled
    /// cable to a byte that lasts 87 µs. A real frame corrupted in transit has always been
    /// scanned the same way, because a bad CRC is also a frame that turned out not to be one.
    pub fn flush<F>(&mut self, mut on_frame: F)
    where
        F: FnMut(&Response<'_>),
    {
        self.scan(&mut on_frame, Incomplete::GiveUp);
    }

    fn scan<F>(&mut self, on_frame: &mut F, incomplete: Incomplete)
    where
        F: FnMut(&Response<'_>),
    {
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
                    continue;
                }
                // `decode` has already refused a length no response may declare, so what is
                // waited on here is at most `MAX_FRAME_LEN` bytes away from settling.
                Ok(Decoded::Incomplete { .. }) => match incomplete {
                    Incomplete::Wait => break,
                    Incomplete::GiveUp => stats.abandoned += 1,
                },
                Err(error) => match error {
                    FrameError::NotSynchronized { .. } => stats.not_synchronized += 1,
                    FrameError::BadCrc { .. } => stats.bad_crc += 1,
                    FrameError::DataTooLong { .. } => stats.data_too_long += 1,
                },
            }

            // Progress is guaranteed here, and that is the whole reason this is a separate
            // step rather than `cursor += 1`. `resynchronize` skips index zero, so it returns
            // an offset of at least one or nothing at all, and nothing at all means the rest
            // of the buffer holds no candidate and can go.
            //
            // A header that turned out false is left one byte at a time, so a real frame that
            // starts inside the bytes it claimed is still found.
            let skip = frame::resynchronize(remaining).unwrap_or(remaining.len());
            stats.discarded_bytes += skip as u64;
            cursor += skip;
        }

        self.buffer.drain(..cursor);
        self.stats.frames += stats.frames;
        self.stats.not_synchronized += stats.not_synchronized;
        self.stats.bad_crc += stats.bad_crc;
        self.stats.data_too_long += stats.data_too_long;
        self.stats.abandoned += stats.abandoned;
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
        assert_eq!(
            feed(&mut reassembler, &bytes[bytes.len() - 1..]),
            vec![0x22]
        );
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
        // 249 is one more than a response may declare, so this is not a frame that might yet
        // complete — it is one that never will. Holding it would stall the stream.
        let mut stream = vec![SOH, 249, 0x22, 0x00, 0x00];
        stream.extend(response(0x07, &[0x01]));

        let mut reassembler = Reassembler::new();
        assert_eq!(feed(&mut reassembler, &stream), vec![0x07]);
        assert_eq!(reassembler.stats().data_too_long, 1);
    }

    #[test]
    fn a_length_of_0xff_is_refused_and_is_also_the_start_of_another_header() {
        // What the test above used to assert, with 0xFF as the illegal length. The length is
        // still refused at once. But the refused byte is itself a SOH, and followed by 0x22 it
        // is a legal header claiming 41 bytes. That is a candidate like any other, so the frame
        // behind it waits. The old reassembler found the frame by looking inside the candidate,
        // which is the behavior the 2026-09-13 review found unsafe.
        let mut stream = vec![SOH, 0xFF, 0x22, 0x00, 0x00];
        stream.extend(response(0x07, &[0x01]));

        let mut reassembler = Reassembler::new();
        assert!(feed(&mut reassembler, &stream).is_empty());
        assert_eq!(reassembler.stats().data_too_long, 1);

        let mut seen = Vec::new();
        reassembler.flush(|response| seen.push(response.opcode));
        assert_eq!(seen, vec![0x07], "held, not lost");
    }

    #[test]
    fn a_payload_full_of_start_bytes_is_not_mistaken_for_frames() {
        let bytes = response(0x22, &[SOH; 32]);
        let mut reassembler = Reassembler::new();
        assert_eq!(feed(&mut reassembler, &bytes), vec![0x22]);
        assert_eq!(reassembler.stats().errors(), 0);
    }

    /// A 0x22 response whose payload carries a whole, valid 0x29 frame.
    ///
    /// The bytes of the inner frame are chosen by whoever wrote the tag's EPC, which is what
    /// makes this a case to test rather than a coincidence to hope against.
    fn a_frame_with_a_frame_inside() -> Vec<u8> {
        let inner = response(0x29, &[0x5A]);
        let mut payload = vec![0x00; 20];
        payload[4..4 + inner.len()].copy_from_slice(&inner);
        response(0x22, &payload)
    }

    #[test]
    fn a_frame_inside_a_payload_is_not_emitted_at_any_cut() {
        // The 2026-09-13 review's reproduction, at every cut rather than the one it found. A
        // whole frame arriving in one feed was already covered by
        // `a_payload_full_of_start_bytes_is_not_mistaken_for_frames`; a serial port delivers
        // partial buffers as the ordinary case, and a cut just after the inner frame used to
        // emit it and throw the real one away as noise.
        let outer = a_frame_with_a_frame_inside();

        for cut in 1..outer.len() {
            let mut reassembler = Reassembler::new();
            let mut seen = feed(&mut reassembler, &outer[..cut]);
            seen.extend(feed(&mut reassembler, &outer[cut..]));
            assert_eq!(seen, vec![0x22], "cut at {cut}");
            assert_eq!(reassembler.pending(), 0, "cut at {cut}");
        }

        let mut reassembler = Reassembler::new();
        let mut seen = Vec::new();
        for byte in &outer {
            seen.extend(feed(&mut reassembler, std::slice::from_ref(byte)));
        }
        assert_eq!(seen, vec![0x22], "byte at a time");
    }

    /// `SOH` and a legal length of 200, followed by far fewer than 200 bytes.
    fn a_false_header() -> Vec<u8> {
        vec![SOH, 200]
    }

    #[test]
    fn a_false_header_holds_the_frames_behind_it_until_its_length_has_arrived() {
        // The cost of not looking inside a waiting frame, and the proof that it is a delay
        // rather than a loss. The header claims 207 bytes; once that many are in, its CRC fails
        // and the real frames it was holding come out, in order.
        let mut stream = a_false_header();
        stream.extend(response(0x01, &[0x11]));

        let mut reassembler = Reassembler::new();
        assert!(feed(&mut reassembler, &stream).is_empty());
        assert_eq!(reassembler.pending(), stream.len());

        let rest = response(0x02, &[0x22; 200]);
        assert_eq!(feed(&mut reassembler, &rest), vec![0x01, 0x02]);
        assert_eq!(reassembler.pending(), 0);
        assert_eq!(reassembler.stats().bad_crc, 1);
    }

    #[test]
    fn flushing_releases_the_frames_a_false_header_was_holding() {
        // The burst ended before the header's 207 bytes arrived. Without a flush these two
        // frames would wait for the next runner and be recorded when that runner crossed.
        let mut stream = a_false_header();
        stream.extend(response(0x01, &[0x11]));
        stream.extend(response(0x02, &[0x22]));

        let mut reassembler = Reassembler::new();
        assert!(feed(&mut reassembler, &stream).is_empty());

        let mut seen = Vec::new();
        reassembler.flush(|response| seen.push(response.opcode));
        assert_eq!(seen, vec![0x01, 0x02]);
        assert_eq!(reassembler.pending(), 0);
        assert_eq!(reassembler.stats().abandoned, 1);
    }

    #[test]
    fn flushing_a_real_partial_frame_gives_it_up_and_invents_nothing() {
        // What a connection that ends mid-frame leaves: the bytes are counted and dropped, and
        // the buffer a reconnection starts from is empty.
        let outer = a_frame_with_a_frame_inside();
        let mut reassembler = Reassembler::new();
        assert!(feed(&mut reassembler, &outer[..4]).is_empty());

        let mut seen = Vec::new();
        reassembler.flush(|response| seen.push(response.opcode));
        assert!(seen.is_empty(), "four bytes hold no whole frame: {seen:?}");
        assert_eq!(reassembler.pending(), 0);
        assert_eq!(reassembler.stats().abandoned, 1);
        assert_eq!(reassembler.stats().frames, 0);
    }

    #[test]
    fn flushing_after_a_whole_frame_changes_nothing() {
        let mut reassembler = Reassembler::new();
        assert_eq!(feed(&mut reassembler, &response(0x22, &[1, 2])), vec![0x22]);

        reassembler.flush(|_| panic!("nothing is held, so nothing can come out"));
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
