# ADR-0041: A capture is checked against the journal by what each read was decoded from

- **Status:** Accepted
- **Date:** 2026-09-26
- **Extends:** [ADR-0040](0040-a-serial-session-can-be-captured-byte-for-byte.md), [ADR-0025](0025-m3a-proves-durability-above-the-transport.md)

## Context

Milestone 3a's exit criterion, as [ADR-0025](0025-m3a-proves-durability-above-the-transport.md)
restated it, has three clauses. Every read the host receives is preserved; **the journal never
disagrees with what arrived**; every disconnection is a bounded gap. The third is checked with
`splitforge reader gaps`. The first two need to know what arrived, and until
[ADR-0040](0040-a-serial-session-can-be-captured-byte-for-byte.md) nothing kept that. A capture
now holds every byte the service received from the module. Nothing yet compares it with the
journal, so the criterion's middle clause could only be approximated: `reads_received` against
`reads_persisted`, per process, reset by every restart the exit run induces.

Three things decide how to compare them.

**Only the composition root may name the adapter.** `dependency_rules.rs` holds that only the
`splitforge-edge` package depends on `splitforge-thingmagic`
([ADR-0012](0012-architecture-rules-enforced-by-tests.md)). The operator CLI cannot decode a
frame, and should not learn how.

**A read in the journal has no identity the capture shares.** Its id is minted at ingest, and its
receipt time is stamped after the capture recorded the bytes. What they share is the bytes: a
read's `raw_payload` is the record the decoder read it from, byte for byte
(`StreamDecoder::decode`). Each record carries the module's own millisecond timestamp and the
tag's signal strength, so two reads with the same payload are rare, and when they occur they
occur on both sides.

**A capture can be incomplete, and says so.** It drops records rather than make the port wait,
and writes how many it dropped. A comparison against a capture with a hole in it can show reads
missing from the capture, and cannot show that none are missing from the journal.

## Decision

**1. `splitforge-thingmagic` reads its own captures back.** `capture::replay` takes a capture a
line at a time and hands every read in it to a callback, decoded the way the service decoded it:
a fresh reassembler per connection, flushed when the connection ends, and `StreamDecoder` on every
`0x22` response with status `0x0000`. It counts what else the capture held: other answers, the
end-of-cycle frame, tag frames with another status, frames the decoder refused, framing faults,
records the capture dropped, and lines it could not read. The format and the reader of it live in
one module and are tested against each other.

**2. The comparison is a second binary in the `splitforge-edge` package, `splitforge-capture`.**
The package is the composition root, so it may use the adapter and storage together. The service
binary stays a service.

```console
$ splitforge-capture check /var/lib/splitforge/session-7.capture
```

**3. Agreement is payload for payload.** Every read decoded from the capture counts its
`raw_payload` once. Every row in `raw_reads` received from the start of the capture to a minute
after its end counts its `raw_payload` once the other way, as does every read set aside as
`journal.unstorable` in that span, which the audit trail holds with its payload. What is left
over is the disagreement: payloads the capture has and the journal does not, and payloads the
journal has and the capture does not. `--reader` narrows the journal to one reader.

**4. The verdict is one of three**, as a fixed token beside the counts, and the exit status
follows it:

| Verdict | Means | Exit |
|---|---|---|
| `agree` | Nothing left over either way, and the capture dropped nothing and read cleanly | 0 |
| `incomplete` | Nothing missing from the journal, but the capture dropped records, has lines it could not read, or holds nothing, so it cannot show that | 2 |
| `disagree` | A payload is on one side and not the other, in a capture that dropped nothing, or a payload the capture has is missing from the journal, whatever else is true | 1 |

Exit status 3 means the check could not be made: a file that would not open, or a database that would not. A read the capture has and the journal does not is the failure the criterion forbids, so it is
`disagree` even in an incomplete capture. A read the journal has and a complete capture does not
is also `disagree`: something wrote a read that did not arrive on the wire, which is the sidecar
route into the journal the 2026-09-13 review found.

## Consequences

### What this makes easy

- **The exit criterion's middle clause becomes a command with an exit status**, over the whole
  run, restarts and all, rather than a per-process pair of counters.
- **A disagreement names what disagrees.** The report lists the first payloads left over on each
  side, in hex, so the frame can be found in the capture and the read in the journal.
- **The decoder is exercised on real bytes offline.** A capture from the bench can be replayed
  against a changed decoder to see what it would have read.

### What this makes hard

- **Memory proportional to the reads in the window**, as a map of payloads to counts. A four-hour
  bench run with a crossing tag is fine. A capture of millions of reads is the same concern as
  `doctor`'s, and is named here.
- **A third binary to install**, beside the service and the CLI. `deployment.md` lists it.

### What we accept

**The replay does not see the service's read timeouts.** The service flushes its reassembler when
the line goes quiet, and a capture does not record timeouts. The replay flushes only when a
connection ends. The reassembler reaches the same frames once more bytes arrive
([ADR-0030](0030-the-serial-adapter-waits-for-proof.md)), so the reads are the same, and only
when a frame was settled differs, which nothing here compares.

**The replay uses the decoder as it is now**, not as it was when the capture was taken. That is
the point for the second use above, and for the first it means checking a capture with the build
that took it.

**A window, not a session boundary.** Reads journaled in the window from another source are
counted unless `--reader` names the one captured. A capture appended to across restarts spans all
of them, which is what the exit run wants.

## Alternatives considered

| Alternative | Why not |
|---|---|
| A `splitforge` CLI command | The CLI may not depend on the adapter, and learning to decode frames without it would be a second decoder |
| A mode of the service binary | A service that also exits after a check is two programs behind one set of flags, and the flags already mean the service |
| Match by chip and time | Receipt times are stamped after the capture records the bytes, and chip reads repeat many times a second. The payload is the identity both sides already share |
| Record a read's id in the capture | The capture is below the decoder, which is where it has to be to keep what the decoder refused. It never sees a read id |
| Compare only counts | Equal counts can hide a lost read and a stray one. The payloads say which |

## References

- [ADR-0040](0040-a-serial-session-can-be-captured-byte-for-byte.md): the capture this reads
- [ADR-0025](0025-m3a-proves-durability-above-the-transport.md): the exit criterion
- [ADR-0012](0012-architecture-rules-enforced-by-tests.md): why the check is in the composition
  root
- [ADR-0030](0030-the-serial-adapter-waits-for-proof.md): the reassembler's flush
- [the bench runbook](../readers/thingmagic-m7e-hecto-bench.md), session 7
