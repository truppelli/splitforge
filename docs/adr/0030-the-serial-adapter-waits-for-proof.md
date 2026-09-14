# ADR-0030: The serial adapter waits for proof before it acts

- **Status:** Accepted
- **Date:** 2026-09-14
- **Supersedes:** — (refines when [ADR-0027](0027-a-reader-reports-connection-events-on-the-read-channel.md)'s `Connected` is sent; the rest of ADR-0027 stands)

## Context

The [2026-09-13 security review](../roadmap.md#security-review--2026-09-13) found four defects
in `splitforge-thingmagic` and the edge's view of it. Three of them are the same mistake: the
adapter treated a guess as a fact.

- **The reassembler searched inside a frame it was waiting for.** A header with a legal length
  and too few bytes behind it was remembered, and the scan went on. A CRC-verified frame found
  further along was taken as proof the header was false. It is no such proof. Every frame found
  that way lies entirely within the bytes the waiting header claims, so it may be that frame's
  payload. A tag's EPC is written by whoever wrote the tag. The review fed a 20-byte `0x22`
  response with a whole frame inside its payload in two chunks, and the reassembler emitted the
  inner frame and discarded the real one as noise.
- **The provider announced a connection when a port opened.** A wrong device node or a loose
  cable opens and ends at once. `run` reset its backoff on every open, so that loop stayed at
  the shortest delay. The review counted 29 connections and 28 disconnections in two seconds,
  and each pair is two rows in the append-only `reader_gap_events` table, fsynced under the
  lock the read path appends through.
- **Decode faults were counted and never reported.** `StreamDecoder::errors()` and
  `Reassembler::stats()` were read only in tests. Milestone 3a promises that a wrong assumption
  shows up as *"no reads and a climbing error count"*, and nothing an operator could see showed
  the count.

The fourth, a decoder that ignored the frame's opcode and status, is a missing check and needs
no decision.

The obvious fix for the reassembler is to wait, and the original code had rejected it for a
reason: *"a guess that waits is how one corrupt header swallows every frame behind it."* That
overstated it. A false header holds the frames behind it until its claimed length arrives, then
fails its CRC, and the frames are found. They are delayed, not lost. But a delay matters here.
This module's timestamps are relative, so the device's receipt time is authoritative
([ADR-0024](0024-serial-reader-adapter-before-llrp.md)), and a read held until the next runner
crosses is recorded when that runner crosses.

## Decision

**1. The reassembler never looks inside a frame it is waiting for.** An incomplete candidate
with a legal length is held, with everything behind it, until enough bytes arrive to verify or
reject it. `frame::decode` already refuses a length above 248, so the wait is at most 255 bytes.

**2. Silence or a closed connection settles what is held.** `Reassembler::flush` gives up on a
held partial frame and hands over the whole frames behind it. The provider calls it on a read
timeout and when the connection ends. A frame in flight does not pause: its bytes arrive about
a millisecond apart even at 9 600 baud, and a read timeout is 250 ms with no byte at all. So a
read held behind a false header is late by at most one read timeout.

**3. A connection is announced when it proves a module is there.** `Connected` is sent at the
first verified frame, or once the port has stayed up for `Backoff::max`. That replaces
ADR-0027's *"sent on every successful connection"*, where success meant the port opened. A
connection that ends before either is part of the outage it followed. It sends `Disconnected`,
which opens nothing new, and it counts as a failure for the backoff. Only an established
connection resets the backoff.

**4. Fault totals travel on the read channel, and one kind degrades health.**
`ReaderEvent::Faults` carries two cumulative counts, sent when either changes:

- **Framing faults** are bytes that never formed a verified frame. They are reported and never
  degrade health on their own, because a connection that opens partway through a frame costs
  one.
- **Decode faults** are frames that arrived intact and were refused. Health is degraded while
  there are any and no read has decoded since the service started.

**5. A tag report is a successful `0x22`.** The decoder refuses any other opcode, and any
non-zero status, as a counted decode fault.

## Consequences

### What this makes easy

- A crafted EPC cannot put a frame of its choosing into the stream by being cut at a read
  boundary, which is the ordinary case on a serial port.
- A run of ports that open and die is one gap with two rows, and the reconnect delay backs off
  to its cap, between half of `Backoff::max` and all of it.
- A module sending frames the decoder cannot read shows up on `/health` as a degraded status,
  so it no longer looks the same as an empty field.

### What this makes hard

- **A quiet module is reported connected up to `Backoff::max` late**, which is 5 s by default.
  A gap it closes is recorded that much longer than it was. ADR-0027 gave the reason not to do
  this: *"a reader that has come back but has nothing to report yet must not stay in one."*
  Once the adapter starts the stream itself, the module's answer to the start command is a
  verified frame. It will then prove the connection within milliseconds, and this cost will
  mostly go away.
- **A read behind a false header can be late by one read timeout**, which is 250 ms by default.
  This is outside the ±0.1 s design target. It happens only when a `0xFF` followed by a legal
  length lands at the start of the buffer and the burst ends before that length arrives.
- `Backoff::max` now means two things: the longest reconnect delay, and how long a quiet
  connection must last to count. Keeping them equal is what bounds the flap rate, so a change to
  one is a change to both.

### What we accept

- **`flush` cannot tell a false header from a real frame that was cut off.** If a connection
  ends, or the line goes quiet, exactly after a frame embedded in a real frame's payload, that
  embedded frame is handed over. Holding it back instead would lose every real frame behind
  every false header. Causing it takes a crafted EPC and a cut at that exact byte. A frame
  corrupted in transit has always been scanned the same way, and the decoder's opcode, status
  and layout checks still apply to whatever comes out.
- **Decode faults stop degrading health after the first decoded read.** A layout that changes
  mid-event would not degrade it, and would show only as a count. The alternative degrades
  health whenever a refusal follows the last read. If the module sends a frame type nobody has
  captured between runners, that would turn health on and off all day, and an operator would
  learn to ignore it.
- **A non-zero status is a fault until a capture says otherwise.** The module may send one in an
  empty field. If it does, a device that has decoded nothing will report degraded health until
  the first tag. Deciding that status is not a fault is a one-line change, and it should be made
  from a capture, not before one exists.

## Alternatives considered

| Alternative | Why not |
|---|---|
| Keep scanning ahead, and accept an embedded frame only if it is a plausible tag report | The decoder's checks are the second line, not the first. A crafted payload can be a plausible tag report |
| Wait on incomplete headers only at a clean frame boundary, and keep searching after a resynchronization | The first frame after connecting, and every frame after line noise, arrives at a resynchronized position. Those are the cases where the embedded frame is still reachable |
| Wait with no flush | A false header at the end of a burst holds real reads until the next runner, and records them then |
| A separate threshold for an established connection | A second number to justify. Equal to `Backoff::max`, a port that dies just after it still cannot reconnect faster than the capped backoff allows |
| Merge flaps at the edge, by delaying the close of a gap | The provider already knows whether the connection proved anything. The edge would have to guess |
| Carry fault counts through a shared atomic handle instead of the channel | Protocol-specific state in the composition root. ADR-0027 put the provider's knowledge on the channel so it arrives in order with the reads |
| Degrade health on framing faults | A connection that opens partway through a frame costs one, so health would be degraded after most connections |
| Amend ADR-0027 | ADR process: *"ADRs are not edited after acceptance except to change status."* Only when `Connected` is sent changes here |

## References

- [ADR-0024](0024-serial-reader-adapter-before-llrp.md): the device's receipt time is
  authoritative for this module
- [ADR-0026](0026-a-reader-gap-is-two-rows.md): opening a gap that is already open writes nothing
- [ADR-0027](0027-a-reader-reports-connection-events-on-the-read-channel.md): the channel these
  events travel on
- [Roadmap: Security review, 2026-09-13](../roadmap.md#security-review--2026-09-13)
- `crates/splitforge-thingmagic/src/reassembly.rs`: `Reassembler::flush`, and the tests on it
- `crates/splitforge-thingmagic/src/provider.rs`: `ThingMagicReader::established`
