# ADR-0045: A reader that says nothing at all is a different gap from one that reads nothing

- **Status:** Accepted
- **Date:** 2026-09-26
- **Extends:** [ADR-0025](0025-m3a-proves-durability-above-the-transport.md), [ADR-0026](0026-a-reader-gap-is-two-rows.md)

## Context

Milestone 3a's third clause is that every disconnection is detected and recorded as a bounded
gap. A ThingMagic module cannot report a broken connection, so part of that detection is
inference: the silence watchdog opens a *suspected* gap when a running race has gone
`reader_silence_ms` without a read. Its threshold is
[Q14](../open-questions.md#q14-reader-silence-threshold), and it is two minutes because nothing
better was known. A quiet finish line and a dead reader look the same when the only evidence is
reads.

Something better is now very likely known.
[Finding 17](../readers/vendor-documents.md#17-an-empty-field-produces-a-frame-at-the-end-of-every-search-cycle)
found that the module sends an empty end-of-cycle frame after every search cycle, which two
sources put at about once a second, tag or no tag. The decoder accepts and counts them, and
`splitforge-capture check` now times them.

The obvious use would be to count those frames as life for the silence watchdog, so a quiet field
stops looking like a dead reader. **That would hide a real failure.** An end-of-cycle frame
proves the module is there, not that it can read a tag. A module whose antenna cable has worked
loose goes on sending one every second and reads nothing. Today the read-silence watchdog
catches that as a race with no reads. Counting the frames as reads would silence it.

These are two questions. *Is the reader there?* can be answered quickly and without ambiguity
from frames that arrive whether or not a tag is near. *Is it reading?* can only be answered from
reads, stays ambiguous in an empty field, and stays a race policy.

## Decision

**1. A new event, `ReaderEvent::Alive`**: the reader said something that was not a read. The
ThingMagic provider sends it when its decoder has accepted a frame that carried no tag: an
end-of-cycle frame, a status report or a stream-end. It sends at most one per batch of frames
read from the port. An LLRP keepalive is the same event when M3b gets there. A frame the
decoder refused is not counted, so a layout nobody has captured never arms anything.

**2. Two detectors, each answering its own question.**
- **The heartbeat check** asks whether the reader is there. It opens a *suspected* gap when a
  reader that has been heard on this connection has said nothing at all, reads included, for
  longer than `reader_heartbeat_ms`. It closes that gap when the reader is heard again. It is
  **not gated on a race running**, because a reader that was speaking every second and stops
  is not a quiet field, at any hour.
- **The read-silence watchdog** asks whether it is reading. It is unchanged: during a race,
  `reader_silence_ms` without a read opens a *suspected* gap. `Alive` never resets its clock.

**3. The heartbeat is armed by hearing the reader, on each connection.** Until the reader has
said something on this connection, the check does nothing. A module that never sends such
frames, or a decoder that recognises none, therefore behaves exactly as before.

**4. Off by default.** `reader_heartbeat_ms` is set with
`splitforge device set --reader-heartbeat-ms` and defaults to zero. An unparseable value is
also off, where the silence threshold falls back to its default. No value has a measurement
behind it until a bench session has timed the frames, and then the value is a few of their
periods.

**5. Only the heartbeat check closes the gap it opened.** Both detectors write to the same open
gap per reader ([ADR-0026](0026-a-reader-gap-is-two-rows.md)). While a heartbeat gap is open,
the read-silence watchdog stands down, the same guard `transport_down` already gives a
confirmed gap. Otherwise its *not silent long enough yet* would close the gap on the strength
of nothing having been read. A connection, a loss of one, or the check being switched off
clears it.

**6. Both kinds of gap stay *suspected*.** The heartbeat is still an inference, from frames
that stop arriving. Only the transport confirms.

## Consequences

### What this makes easy

- **A module that dies in a quiet field is noticed in seconds, not minutes.** With the
  threshold at a few periods, the gap opens within that plus the watchdog's ten-second tick. It
  is backdated to when the quiet began.
- **A loose antenna is still caught**, by the check that was already catching it, and a test
  holds that `Alive` does not change that.
- **Nothing changes until someone turns it on**, after measuring.

### What this makes hard

- **Two thresholds to explain.** The CLI help for each says which question it answers, and the
  bench runbook says how to choose the heartbeat's.
- **One more piece of state that has to be reset on each connection.** It is reset in the one
  function that records a connection change, which a test covers.

### What we accept

- **The ten-second tick bounds how quickly the heartbeat check notices**, the same as the
  read-silence watchdog. A faster tick for this check alone was not worth a second loop while
  the period itself is unmeasured.
- **A module that keeps sending end-of-cycle frames while wedged in some other way** looks
  alive to this check. The read-silence watchdog is still there for a running race.
- **Health does not report when the reader was last heard.** The gap is the report. Adding it to
  `/health` is a small change once the bench shows whether operators need it.

## Alternatives considered

| Alternative | Why not |
|---|---|
| Count `Alive` as life for the read-silence watchdog | Hides a loose antenna, which is the failure that watchdog catches today |
| Lower `reader_silence_ms` instead | Still one question asked of the wrong evidence. A short threshold manufactures gaps on a quiet finish line |
| A new `GapDetection` value, such as `unresponsive` | The gap is still an inference. The detail beside it says which check opened it, and adding a kind changes the stored vocabulary for no new fact |
| Turn the heartbeat on by default at, say, five seconds | A guess about a frame nobody has timed on this module. Q14 is waiting on exactly that measurement |
| An `Alive` for every frame | The service needs when it last heard, not how often. One per batch keeps the channel for reads |

## References

- [Q14](../open-questions.md#q14-reader-silence-threshold): the silence threshold
- [Finding 17](../readers/vendor-documents.md#17-an-empty-field-produces-a-frame-at-the-end-of-every-search-cycle): the end-of-cycle frame
- [ADR-0025](0025-m3a-proves-durability-above-the-transport.md): detecting disconnection as a deliverable
- [ADR-0026](0026-a-reader-gap-is-two-rows.md): one open gap per reader
- [Bench runbook, session 2](../readers/thingmagic-m7e-hecto-bench.md#session-2-first-contact): where the period is measured
