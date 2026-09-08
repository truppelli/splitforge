# ADR-0027: A reader reports connection events on the same channel as its reads

- **Status:** Accepted
- **Date:** 2026-09-01
- **Deciders:** —

## Context

[ADR-0025](0025-m3a-proves-durability-above-the-transport.md) makes *"every disconnection is
detected and recorded as a bounded gap in the evidence"* a clause of Milestone 3a's exit
criterion, and names two detectable events:

- **The device node goes away.** Unambiguous, and recorded as `confirmed`.
- **The stream goes silent.** Ambiguous, and recorded as `suspected`.

The second is built: a watchdog in `splitforge-edge` compares the wall clock against the last
message and opens a suspected gap. It needs nothing from the transport, which is exactly why it
could be built first.

**The first cannot be built at all as the port currently stands.** `ReaderProvider::start`
returns `mpsc::Receiver<ReaderMessage>`, and a `ReaderMessage` is a read. A provider that has
just failed to open its port has nothing it is allowed to say: the only vocabulary it has is
"here is a tag", and inventing a sentinel read to mean "the cable is out" would put a fabricated
read in an append-only evidence table, which is the one thing that table exists to prevent.

So the port has to grow a way to say it. The question is *where* — and the answer is
constrained by something less obvious than it looks.

### The constraint is ordering, not plumbing

A gap has to start after the last read that preceded it. That sounds automatic and is not.

The reads are already in flight on a bounded channel: `ThingMagicReader` blocks in
`blocking_send` when the journal cannot keep up, which is deliberate back-pressure
([architecture § 4](../architecture.md#4-failure-behavior)). So at the moment a port dies, some
reads it delivered may still be queued ahead of the consumer.

If the disconnection arrives on a *second* channel, the consumer can observe it while those
reads are still queued — and will then open a gap timestamped **before** reads it has not yet
written. The evidence would say the reader was gone during a period it was demonstrably
producing, and the two facts would be irreconcilable after the fact because nothing records
which channel was drained first.

This is not a rare interleaving. It is the *expected* one precisely when it matters most: under
load, on a slow SD card, at the moment the transport fails.

## Decision

**`ReaderProvider::start` returns `mpsc::Receiver<ReaderEvent>`**, and `ReaderEvent` is:

```rust
pub enum ReaderEvent {
    Read(ReaderMessage),
    Disconnected { detail: Option<String> },
    Connected,
}
```

One channel, so the order the provider observed is the order the consumer sees. A
`Disconnected` cannot overtake the reads that preceded it, because it is behind them in the
same queue.

**`Disconnected` and `Connected` are claims about the transport only**, and a provider emits
them only when the transport itself said so — a port that failed to open, a read that returned
an error. A provider must never infer a disconnection from quiet, because that inference is the
watchdog's, it is ambiguous, and it is recorded under a different word.

**A provider that cannot disconnect emits neither.** The simulator's reads are `Read` and
nothing else; there is no cable, so there is no claim to make. `ReaderKind::Simulated` already
tells an operator what they are looking at.

**The consumer maps them to gaps**: `Disconnected` opens a `confirmed` gap, `Connected` closes
whatever is open. Both go through the same `open_reader_gap` / `close_reader_gap` that the
watchdog uses, so ADR-0026's "first detection wins" rule applies unchanged — a confirmed
disconnection arriving while a suspected gap is open does not open a second.

## Consequences

### What this makes easy

- **A confirmed gap cannot precede a read that arrived inside it.** The property that motivated
  the whole decision, and it holds by construction rather than by discipline in the consumer.
- **M3b inherits it.** LLRP has the same two events over TCP, and this is the port both adapters
  implement. Doing it here rather than in the ThingMagic crate is what stops it being done twice
  and differently.
- **The read path stays one path.** `--simulate` is still not a second code path: the consumer
  matches on the same enum whichever provider is composed.

### What this makes hard

- **Every implementor and every consumer changes at once**, because the channel's item type
  changed. There are two of each, and a compiler error at each — which is the reason to do it
  while there are two.
- **Consumers must handle a variant they may not care about.** `splitforge simulate` writes
  reads to a journal and has no gap table in front of it; it now has to say so explicitly rather
  than by the type making the question unaskable.

### What we accept

- **A `Read` variant costs one enum discriminant per message** on the hottest path in the
  project. Measured against what it buys — the ordering guarantee above — this is not close, and
  the channel already boxes nothing.
- **`detail` is free text from the operating system** and nothing parses it. It is recorded on
  the gap for a human reading the evidence later, and no promise is made about its wording.
- **Nothing here is observed against hardware.** A provider emitting `Disconnected` on a real
  unplugged cable is M3a's exit criterion, and it still needs the module.

## Alternatives considered

| Alternative | Why not |
|---|---|
| A second channel for connection events | The ordering hazard above: a disconnection can overtake queued reads and produce a gap that starts before reads taken inside it. Racy exactly under the load that makes the failure likely. |
| A shared status handle the consumer polls | Same ordering problem, plus it turns an event into a level — two disconnections in one polling interval become one, and a gap that closed and reopened between polls is invisible. |
| A sentinel `ReaderMessage` meaning "gone" | Puts a fabricated read into an append-only evidence table. ADR-0005 and ADR-0011 exist to make exactly this impossible. |
| Leave it to the silence watchdog | Records every disconnection as `suspected`, discarding a distinction the transport actually knows. ADR-0025 names both events deliberately, and *"suspected"* on an unplugged cable is under-reporting a fact. |

## References

- [ADR-0025](0025-m3a-proves-durability-above-the-transport.md) — makes detection a deliverable
  and names the two events
- [ADR-0026](0026-a-reader-gap-is-two-rows.md) — how a gap is stored, and the "first detection
  wins" rule this obeys
- [ADR-0004](0004-llrp-first-reader-adapter.md) — the port both adapters implement
- [architecture § 4](../architecture.md#4-failure-behavior) — the back-pressure that makes the
  ordering hazard real
