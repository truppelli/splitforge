# ADR-0027: A reader reports connection events on the same channel as its reads

- **Status:** Accepted
- **Date:** 2026-09-02
- **Deciders:** —

## Context

[ADR-0025](0025-m3a-proves-durability-above-the-transport.md) makes *"every disconnection is
detected and recorded as a bounded gap in the evidence"* a clause of Milestone 3a's exit
criterion, and names two detectable events:

- **The stream goes silent.** Ambiguous, and recorded as `suspected`.
- **The transport fails.** Unambiguous, and recorded as `confirmed`.

The first was built first, because it needs nothing from the transport: a watchdog in
`splitforge-edge` compares the clock against the last message and opens a suspected gap
([ADR-0026](0026-a-reader-gap-is-two-rows.md)).

**The second could not be built at all as the port stood.** `ReaderProvider::start` returned
`mpsc::Receiver<ReaderMessage>`, and a `ReaderMessage` is a read. A provider that had just
failed to open its port had nothing it was allowed to say: its only vocabulary was "here is a
tag", and inventing a sentinel read to mean "the cable is out" would put a fabricated read in an
append-only evidence table — which is what [ADR-0005](0005-raw-read-append-only-journal.md)
and [ADR-0011](0011-append-only-enforced-by-triggers.md) exist to prevent.

`GapDetection::Confirmed` had existed since the gap table landed, the journal accepted it, and
health had a string for it. Nothing in the workspace produced one, because the only writer was
the silence watchdog and silence was the only evidence the service had.

So the port has to grow a way to say it. The question is *where* — and the answer is constrained
by something less obvious than it looks.

### The constraint is ordering, not plumbing

A gap has to start after the last read that preceded it. That sounds automatic and is not.

The reads are already in flight on a bounded channel: a provider blocks in `blocking_send` when
the journal cannot keep up, which is deliberate back-pressure
([architecture § 4](../architecture.md#4-failure-behavior)). So at the moment a port dies, some
reads it delivered may still be queued ahead of the consumer.

If the disconnection arrives on a *second* channel, the consumer can observe it while those
reads are still queued — and will then open a gap timestamped **before** reads it has not yet
written. The evidence would say the reader was gone during a period it was demonstrably
producing, and the two facts would be irreconcilable after the fact, because nothing records
which channel was drained first.

This is not a rare interleaving. It is the *expected* one precisely when it matters most: under
load, on a slow SD card, at the moment the transport fails.

## Decision

**`ReaderProvider::start` returns `mpsc::Receiver<ReaderEvent>`**, and `ReaderEvent` is:

```rust
pub enum ReaderEvent {
    Read(ReaderMessage),
    Connected,
    Disconnected { cause: Disconnection },
}
```

One channel, so the order the provider observed is the order the consumer sees. A `Disconnected`
cannot overtake the reads that preceded it, because it is behind them in the same queue.

**The cause is a token, not a sentence.**

```rust
pub enum Disconnection {
    NotOpened,
    Ended,
}
```

For the same reason the clock-source report names its `measurement` as one: a watchdog reading
the evidence should never have to parse prose to tell two situations apart. These are different
diagnoses — `NotOpened` says the reader was never reachable and the ordinary reading is that
nothing was plugged in; `Ended` says something that had been delivering stopped, and it is the
disconnection an observer actually induces. An operator acts on them differently.

`Disconnection::detail()` lives with the enum so the prose recorded beside a gap cannot drift
from the cause it describes, and **both strings are compile-time constants with no
interpolation** — which is the strongest case
[ADR-0020](0020-diagnostic-bundles-carry-no-participant-data.md)'s allowlist can be given if a
bundle ever carries them. Free text from the operating system would not have that property.

**The lifecycle variants carry no timestamp.** *When* a disconnection happened is the receiving
end's observation, taken from the device clock it already trusts for evidence. A provider
reporting its own idea of the time would be a second clock in the record with nothing
disciplining it.

**`Connected` is sent on every successful connection, not only the first**, because after a
reconnection it is what closes the gap the disconnection opened — and a reader that has come
back but has nothing to report yet must not stay in one.

**`Disconnected` is sent on every failed attempt, not only the first.** Opening is idempotent at
the journal — `open_reader_gap` returns the one already open (ADR-0026) — so a stateless report
costs a row nobody writes, and saves the reconnect loop from keeping a duplicate of state the
evidence already holds durably, on the one path that must survive a power cut.

**Both are claims about the transport only.** A provider emits them when the transport itself
said so — a port that failed to open, a read that returned an error. A provider must never infer
a disconnection from quiet: that inference is the watchdog's, it is ambiguous, and it is
recorded under a different word.

**A provider that cannot disconnect emits neither.** The simulator's events are all `Read`;
there is no cable, so there is no claim to make. `ReaderKind::Simulated` already tells an
operator what they are looking at.

**The consumer maps them to gaps.** `Disconnected` opens a `confirmed` gap, `Connected` closes
whatever is open — including a merely suspected one, because a reader that is connected is a
reader that came back, whichever way its absence was noticed. Both go through the same
`open_reader_gap` / `close_reader_gap` the watchdog uses, so ADR-0026's "first detection wins"
rule applies unchanged.

**The confirmed half is not gated on a race running, and the suspected half still is.** The
asymmetry is the ambiguity. Silence on a bench overnight means nothing and recording it would
teach an operator to ignore the signal; a reader that is *not there* is a true statement at any
hour, and it is the statement an operator most wants before the gun, when the reason the port
will not open is usually that nothing has been plugged into it yet.

**A reconnection resets the silence clock.** A connection is evidence the reader is alive
exactly as a read is, and it is the only such evidence a reader in an empty field will ever
produce. Without the reset, silence would still be measured from before the outage and the
watchdog would open a suspected gap seconds after a real reconnection closed a confirmed one.

**While the transport is known down, the watchdog stands down.** The two detectors answer the
same question from different evidence and only one of them is guessing. This is a correctness
requirement rather than a tidiness one: `assess_silence` reads *not silent for long enough yet*
as `Close` — a verdict meaning "reads are arriving" — so a tick landing inside the first
threshold's worth of a real outage reaches it without any read having arrived, and would close
the confirmed gap the disconnection had just opened.

## Consequences

### What this makes easy

- **A confirmed gap cannot precede a read that arrived inside it.** The property that motivated
  the whole decision, and it holds by construction rather than by discipline in the consumer.
- **Q14 stops being in the path of noticing an induced disconnection.** A transport that says
  the port died needs no threshold and no guess, so
  [Q14](../open-questions.md#q14-reader-silence-threshold) constrains only the ambiguous half.
- **M3b inherits it.** LLRP has the same two events over TCP, and this is the port both adapters
  implement. Doing it here rather than in the ThingMagic crate is what stops it being done twice
  and differently.
- **The read path stays one path.** `--simulate` is still not a second code path: the consumer
  matches on the same enum whichever provider is composed.

### What this makes hard

- **Every implementor and every consumer changed at once**, because the channel's item type
  changed. There were two of each, and a compiler error at each — which is the reason to do it
  while there are two.
- **Consumers must handle a variant they may not care about.** `splitforge simulate` writes
  reads to a journal and has no gap table in front of it; it says so explicitly rather than
  having the type make the question unaskable.
- **`ReaderState` gained `Disconnected`**, and every reader now starts in it. The composition
  root was setting `Connected` before any connection existed — harmless for a scripted reader
  and a lie for a serial port that may never open.

### What we accept

- **A `Read` variant costs one enum discriminant per message** on the hottest path in the
  project. Measured against the ordering guarantee above this is not close, and the channel
  already boxes nothing.
- **Nothing here is observed against hardware.** A provider emitting `Disconnected` on a real
  unplugged cable is M3a's exit criterion, and it still needs the module.

## Alternatives considered

| Alternative | Why not |
|---|---|
| A second channel for connection events | The ordering hazard above: a disconnection can overtake queued reads and produce a gap that starts before reads taken inside it. Racy exactly under the load that makes the failure likely. |
| A shared status handle the consumer polls | Same ordering problem, plus it turns an event into a level — two disconnections in one polling interval become one, and a gap that closed and reopened between polls is invisible. |
| A sentinel `ReaderMessage` meaning "gone" | Puts a fabricated read into an append-only evidence table. ADR-0005 and ADR-0011 exist to make exactly this impossible. |
| `Disconnected { detail: String }` from the OS | Uncontrolled text on a path that can reach a diagnostic bundle, and prose a watchdog would have to parse. A token plus a constant `detail()` gives the operator the same information with neither cost. |
| Leave it to the silence watchdog | Records every disconnection as `suspected`, discarding a distinction the transport actually knows. ADR-0025 names both events deliberately, and *"suspected"* on an unplugged cable is under-reporting a fact. |

## What this cost, and what it was worth

[hardware-plan § "It proves the `ReaderProvider` port is real"](../hardware-plan.md#it-proves-the-readerprovider-port-is-real)
asked in advance whether the port would survive the serial adapter unchanged. **It did not**, and
that is the prediction working rather than failing.

Different framing, no reader clock, and one antenna all fit behind the port without moving it.
Liveness did not — because the port was designed around LLRP, and TCP made liveness someone
else's problem so completely that the port never thought to ask for it. The gap was found by the
adapter that has no such transport, rather than at Milestone 6 by the reader that would have
inherited the assumption.

## References

- [ADR-0025](0025-m3a-proves-durability-above-the-transport.md) — makes detection a deliverable
  and names the two events
- [ADR-0026](0026-a-reader-gap-is-two-rows.md) — how a gap is stored, and the "first detection
  wins" rule this obeys
- [ADR-0020](0020-diagnostic-bundles-carry-no-participant-data.md) — why the cause is a token
  with a constant message rather than operating-system text
- [ADR-0004](0004-llrp-first-reader-adapter.md) — the port both adapters implement
- [architecture § 4](../architecture.md#4-failure-behavior) — the back-pressure that makes the
  ordering hazard real
