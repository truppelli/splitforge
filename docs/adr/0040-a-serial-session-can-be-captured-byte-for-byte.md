# ADR-0040: A serial session can be captured byte for byte, beside the evidence and never in it

- **Status:** Accepted
- **Date:** 2026-09-25
- **Extends:** [ADR-0004](0004-llrp-first-reader-adapter.md), [ADR-0033](0033-each-connection-starts-the-stream.md)

## Context

Almost every claim Milestone 3a makes about the M7E-HECTO is held the same way: *believed when a
capture agrees* ([ADR-0004](0004-llrp-first-reader-adapter.md)). The CRC was believed until one
captured frame showed it was wrong. The decoder, the start sequence, the power read-back, the
end-of-cycle frame ([finding 17](../readers/vendor-documents.md#17-an-empty-field-produces-a-frame-at-the-end-of-every-search-cycle)),
and what an overheating module sends ([finding 27](../readers/vendor-documents.md#27-overheating-is-reported-as-0x504))
are all waiting on the first session with a real module.

**Nothing in the service keeps what that session would show.** A frame the decoder turns into a
read reaches the journal, reshaped as a read. A frame it refuses is counted as a decode fault and
its bytes are dropped. The start sequence's answers are judged and then gone. A frame of a kind
nobody expected, which is the one the session is for, leaves a number on `/health`. If the first
session surprises anyone, the bytes that would explain it will not exist.

The roadmap already asks for this, for LLRP: *"Raw protocol captures behind an explicit diagnostic
flag"* ([Milestone 3b](../roadmap.md#milestone-3b--one-networked-llrp-reader)). The serial reader
arrives first and needs it first.

Two constraints decide the design. **The module has no flow control** (§ 5.1.4.1), so the
thread that reads the port must never wait on a slow disk: a capture that stalled it would lose
reads, which is the one thing it must never cost. And **a capture is not evidence**: it is not
fsynced, not append-only in the database's sense, not replayed, and not something a result can
rest on. The journal and its sidecar stay the only record of what was read
([ADR-0005](0005-raw-read-append-only-journal.md), [ADR-0018](0018-write-ahead-sidecar-journal.md)).

## Decision

**1. `splitforge-edge --serial` takes `--capture <PATH>`**, off unless given. With it, every
byte written to the port and every byte read from it goes to that file, with when and in which
direction, as well as the port opening, failing to open, returning an error, and closing.

**2. The capture wraps the port factory, not the reader.** `splitforge_thingmagic::capturing`
takes any `PortFactory` and returns one whose ports copy what they carry. The provider, the start
sequence and the decoder are unchanged, and cannot tell a captured port from any other.

**3. The reading thread never waits for the capture.** Each record is handed to a writer thread
through a bounded queue. When the queue is full the record is dropped and counted, and the next
record written says how many were lost before it. Losing capture is acceptable; losing a read to
keep capture is not. If writing fails, the capture stops, says so once, and reading goes on.

**4. The file is text, one record per line**, for the same reason the sidecar is: it has to be
readable with `grep` and a pager.

```text
2026-09-25T14:03:07.112041Z +0.000ms open
2026-09-25T14:03:07.112310Z +0.269ms > ff032f000002…
2026-09-25T14:03:07.118934Z +6.893ms < ff012f0000024b0e…
2026-09-25T14:03:09.004117Z +1892.076ms error BrokenPipe: …
2026-09-25T14:03:09.004150Z +1892.109ms close
```

The first field is the wall clock when the record was taken, and the second is milliseconds on
the monotonic clock since the capture started, which is what intervals are measured with. `>` is
sent to the module and `<` is received from it, as lowercase hex exactly as the chunk arrived, so
where the port split a frame is kept too. A read that timed out is not recorded: an idle module
times out several times a second, and the absence of anything else says the same thing.

**5. It is created `0640`**, like the sidecar. It holds every chip identifier the module reported,
so it is event data, and it is **not** safe to attach to a public issue the way a diagnostic
bundle is ([ADR-0020](0020-diagnostic-bundles-carry-no-participant-data.md)).

**6. The session's `reader.configured` audit row names the capture file**
([ADR-0038](0038-each-connection-sets-the-read-power-the-operator-chose.md)), so the journal
records that a capture of the session exists and where it was written.

## Consequences

### What this makes easy

- **The first session produces evidence about the module, not only reads from it.** A frame
  that surprises the decoder can be pasted into a test, the way `CAPTURED_FRAME` was, and a
  claim made from documents can be checked against what the module actually sent.
- **The first-session questions get answered in bytes**: what the start sequence's answers
  hold, what the power read-back reports, whether the end-of-cycle frame arrives about once a
  second, and what an unplugged CH340 returns.
- **The same approach carries to M3b.** An LLRP capture wraps a socket instead of a serial port,
  and keeps the same file format and the same rule that capture never delays a read.

### What this makes hard

- **A second thing writing to disk during a session.** At a few hundred bytes a frame it is
  small beside the journal, but it is not nothing on an SD card, and it has no rotation. It is
  a bench flag, and `deployment.md` says so.
- **Two files that look like records.** The capture and the sidecar are both text files full
  of chip reads. Only one of them is evidence, and the capture's first line says it is not.

### What we accept

**A capture can lose records**, when the queue is full or the disk fails, and it says where it
did. It is not fsynced, so a power cut loses its tail. Neither costs a read.

**Receive times are when the reading thread got the bytes**, not when they arrived at the USB
bridge. The capture is for what the module said, not for timing. Timing stays with the journal
and its session anchor.

## Alternatives considered

| Alternative | Why not |
|---|---|
| Keep refused frames in the journal | The journal holds reads. A frame is not a read, and a table of frames next to evidence is one misreading away from being taken for evidence |
| Capture inside the provider | The provider would carry a second responsibility, and the decoder and start-sequence tests would have to account for it. Wrapping the factory leaves them untouched |
| Write the capture from the reading thread | A slow write would stop the port being read, on a link with no flow control. That loses reads to keep a diagnostic |
| An external tool on the tty, such as `interceptty` or `strace` | It needs installing and setting up at the bench, and it records bytes without the opens and closes the service sees. The service already has the bytes |
| A binary format such as pcap | Nothing here needs a dissector, and `grep` works on text |

## References

- [ADR-0004](0004-llrp-first-reader-adapter.md): a parser is believed when a capture agrees
- [ADR-0033](0033-each-connection-starts-the-stream.md): the start sequence whose answers this
  keeps
- [ADR-0018](0018-write-ahead-sidecar-journal.md): the sidecar, which is evidence where this is
  not
- [ADR-0020](0020-diagnostic-bundles-carry-no-participant-data.md): why a capture is not a
  bundle
- [ADR-0038](0038-each-connection-sets-the-read-power-the-operator-chose.md): the audit row that
  names the capture
- [roadmap, Milestone 3b](../roadmap.md#milestone-3b--one-networked-llrp-reader): the capture flag
  asked for there
