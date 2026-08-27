# ADR-0024: A serial module is the first physical adapter; LLRP stays the first networked one

- **Status:** Accepted
- **Date:** 2026-08-26

## Context

[Milestone 3](../roadmap.md#milestone-3--one-physical-reader) has been gated on hardware since
Milestone 0, and [Q9](../open-questions.md#q9-first-reader-model) has had no owner for that
entire time. Milestones 4 and 5 were built *around* the gate rather than through it, which
worked — and has now run out of road. Every remaining item in M5, and therefore all of M6,
needs a Pi with real reads flowing into it.

The gate exists for a good reason. [hardware-support.md](../hardware-support.md) says a
support claim is a claim an organizer can stake an event on, and
[ADR-0004](0004-llrp-first-reader-adapter.md) hard-gated M3 on a physical LLRP reader
deliberately. Nothing about that reasoning has weakened.

What has become clear is that the gate was written as a single door when it is really two.

A used Impinj Speedway or Zebra FX7500 satisfies every line of the support checklist today.
It is also out of production, priced by whatever a liquidator lists this month, and
impossible to build a repeatable bill of materials around — you cannot certify, manufacture,
or support a product whose central component is scavenged. Meanwhile a current serial UHF
module with a US distributor and an FCC modular grant can be bought this week, and can close
**six** of the nine support criteria, recast a seventh honestly, and close neither of the
remaining two.

[hardware-plan.md](../hardware-plan.md) § 2 scores both against the checklist without
softening a line. The two it cannot close are the two that matter most to LLRP specifically:

- **Clock offset and skew measured over a multi-hour session** — there is no reader clock on
  the module to measure against.
- **Antenna identity reported and correctly mapped** — one RF port until a second module is
  fitted.

So the choice is not "which reader is better." It is whether a milestone that has blocked
three others for its entire life should keep blocking them when most of what it was
protecting can be retired now, and the rest can stay gated exactly as it is.

## Decision

**Milestone 3 splits into M3a and M3b. Neither exit criterion is weakened.**

- **M3a — one serial reader.** Takes the six criteria a serial module can close plus the one
  it recasts, and states the two it cannot. Completing M3a does **not** put any device in the
  support matrix.
- **M3b — one networked LLRP reader.** Keeps all nine criteria verbatim and stays gated
  exactly as M3 is today, on [Q9b](../open-questions.md#q9b-first-llrp-reader-model).
- **Milestone 5 depends on M3b, not M3a.** M5's exit criterion is unchanged.

**[ADR-0004](0004-llrp-first-reader-adapter.md) stands, unamended.** LLRP remains the first
*networked* reader protocol and the one the support checklist was written against. The serial
module becomes the first *physical* adapter. Those are different claims and this ADR does not
let one stand in for the other.

**The ThingMagic M7e-Pico is the first physical adapter**, resolving
[Q9a](../open-questions.md#q9a-first-serial-module). It lands in a new
`crates/splitforge-thingmagic/` behind the existing `ReaderProvider` port, permitted to depend
on `splitforge-domain` and `splitforge-reader` and nothing else — the boundary
`splitforge-llrp` already declares. The `ALLOWED` table in
`crates/splitforge-testkit/tests/dependency_rules.rs` and the dependency table in
[architecture.md § 2](../architecture.md#dependency-rules) both gain a row, by hand, because
that table is listed exhaustively so that adding a crate forces a deliberate answer
([ADR-0012](0012-architecture-rules-enforced-by-tests.md)).

**`serialport` joins the read path**, which `deny.toml`'s `[bans]` section says warrants an
ADR — *"every crate on the read path is a crate that can lose a read."* This is that ADR. It
is declared as:

```toml
serialport = { version = "4", default-features = false }
```

**`default-features = false` is required, not stylistic.** The default feature set pulls in
`libudev`, which needs `libudev-dev` *for the target*; CI's cross gate
(`.github/workflows/ci.yml`) installs only `gcc-aarch64-linux-gnu`, so leaving defaults on
breaks the Raspberry Pi build that [ADR-0002](0002-raspberry-pi-target.md) makes mandatory.
MPL-2.0 is already in `deny.toml`'s allow list, so the licence check passes unchanged.

**The module's timestamp is evidence and is not authoritative.** The M7e reports a relative
millisecond timestamp within a continuous-read session — *not* microseconds since boot. It
maps onto `ReaderTimestamp::Uptime { micros }`, and every connect captures a session anchor
`(received_at_utc, module_relative_us)`, which is the anchoring
[clock discipline § 6](../clock-and-time-discipline.md#6-llrp-timestamp-specifics) already
prescribes. Through the existing `Ingest::normalize` that yields `reader_timestamp = None`,
`timestamp_source = DeviceReceipt { ReaderUptimeOnly }`, and the Pi's receipt time as
authoritative — with the module's higher-resolution value preserved as evidence. That is the
conservative default for a first adapter, and it is revisitable at derivation time without
ever touching the journal
([clock discipline § 9](../clock-and-time-discipline.md#9-correction-happens-at-derivation-never-in-the-journal)).

**The module is `experimental — under evaluation`, not supported**, and does not enter the
support matrix. Its gaps are named in
[`docs/readers/thingmagic-m7e-pico.md`](../readers/thingmagic-m7e-pico.md). It moves into the
matrix only when the full checklist has been observed, which by the two rows above it cannot
be — so on current hardware it never does.

## Consequences

### What this makes easy

- **A milestone that has blocked three others can start this month**, under a budget that
  exists, rather than waiting on a purchase nobody has scheduled.
- **The four Pi-side reliability questions [M5](../roadmap.md#still-open--and-nearly-every-item-of-it-needs-hardware)
  could not retire become answerable**, none of which need LLRP — only a real stream of real
  reads: whether the SD card honors `fsync` at all, what the second sync per reader report
  ([ADR-0018](0018-write-ahead-sidecar-journal.md)) costs on real flash, what a full day's
  journal actually weighs, and what happens to a write in flight when the power goes.
- **`ReaderProvider` faces its first genuine adversary.** Today the port's central claim —
  that the engine cannot tell one reader from another — rests on a single implementation, the
  simulator, written by the same people who wrote the port to fit the port. An LLRP reader
  would not have tested it either, because LLRP is what the port was designed around. A serial
  module with no reader clock, one antenna, and completely different framing is the first
  thing that can falsify it. If the port survives unchanged that is evidence; if it needs
  changing, finding out now costs a crate rather than a rewrite.
- **This adapter is *less* privileged than the one it precedes.** A serial adapter opens a
  file, not a socket, so `RestrictAddressFamilies=AF_UNIX` stays in
  [`deploy/splitforge-edge.service`](../../deploy/splitforge-edge.service) untouched. M3b's
  LLRP reader is what has to widen it deliberately, and fail
  `apps/splitforge-edge/tests/unit_file.rs` until it does.

### What this makes hard

- **Two adapters to maintain before either is proven.** `splitforge-llrp` stays an empty
  scaffold while a second protocol crate grows beside it.
- **The device tree gets wider before it gets deeper.** `PrivateDevices=yes` in the unit file
  gives the service a private `/dev` containing only pseudo-devices; `/dev/ttyUSB0` is not in
  it, so as the unit stands the service cannot see its reader at all. M3a has to grant the tty
  class (`DevicePolicy=closed`, `DeviceAllow=char-ttyUSB rw`) and add a udev rule for a stable
  name, because `ttyUSB0` renumbers on re-enumeration.
- **Timing accuracy on this hardware is bounded by the Pi's receive-time jitter** — USB serial
  latency plus scheduler jitter — not by the module's specifications. Quoting the module's
  "300 tags/sec" as an accuracy figure would be quoting the wrong number entirely. Measuring
  that jitter becomes a deliverable rather than an assumption.

### What we accept

**That completing M3a leaves the support matrix empty.** It will be tempting, after a serial
module has run reads into the journal for hours, to write something in that table. Two
criteria will still be unclosed and the rule does not bend for effort already spent.

**That M5 stays blocked.** Its exit criterion depends on M3b, which depends on
[Q9b](../open-questions.md#q9b-first-llrp-reader-model), which is exactly as open as Q9 was
before this ADR. Nothing here unblocks a real event being timed. It is honest to say that the
project's headline gate has not moved — only that the work behind it has.

**That the module may go end-of-life before the product does.** RFID modules turn over faster
than a timing product's useful life. The software already answers this — a new protocol is a
new adapter, which is what `ReaderProvider` is for — but it means treating the module as a
replaceable subassembly in hardware too, which is a design constraint this ADR imposes on
work it does not itself cover.

**That one antenna is not a finish line.** A finish chute wants two, the Pico has one port,
and the answer is two modules each presenting as its own `ReaderProvider` with its own reader
and antenna identity — not one module behind a splitter, which would halve transmit power and
destroy the per-antenna identity [timing-model.md](../timing-model.md) depends on and that
`splitforge reader map --antenna` already exposes to the operator.

## Alternatives considered

| Alternative | Why not |
|---|---|
| Keep waiting for an LLRP reader | This is what has happened for three milestones. It has cost the project every hardware-gated item in M5 and produced no reader. Q9b keeps this option open at zero cost — M3b is still there, still gated, still first in the sense that matters |
| Buy a used Impinj or Zebra and call M3 done | Closes all nine criteria and is the right *test instrument*, which is why the hardware plan holds ~$350 for one opportunistically. It is a dead end as product hardware: out of production, liquidator pricing, no repeatable BOM |
| Weaken M3's exit criteria to what a serial module can close | The whole value of an exit criterion is that it is not adjusted to fit what was built. Splitting preserves both claims; softening would destroy the one that matters |
| Write the LLRP adapter from protocol documentation alone | Explicitly forbidden by [hardware-support.md](../hardware-support.md). ADR-0004 permits writing a *parser* against captures, which is different from claiming an adapter works |
| Put the serial module behind `splitforge-llrp` | Two unrelated wire protocols in one crate, and it would make `splitforge-llrp` a lie. The port exists so protocols are peers |
| One module, RF splitter, two antennas | Halves transmit power, complicates matching, and destroys per-antenna identity — for the price of the second module it would have saved |

## References

- [hardware-plan.md](../hardware-plan.md) — the proposal this ADR decides, and the budget
  behind it
- [hardware-support.md](../hardware-support.md) — the checklist § 2 of that plan scores
  against, and the rule that keeps the matrix empty
- [`docs/readers/thingmagic-m7e-pico.md`](../readers/thingmagic-m7e-pico.md) — the per-model
  notes file the checklist requires
- [ADR-0004](0004-llrp-first-reader-adapter.md) — stands unamended; LLRP is still the first
  networked protocol
- [ADR-0002](0002-raspberry-pi-target.md) — the cross-build the `serialport` feature flag
  protects
- [ADR-0012](0012-architecture-rules-enforced-by-tests.md) — why the dependency table is
  edited by hand
- [ADR-0018](0018-write-ahead-sidecar-journal.md) — the second fsync this hardware finally
  lets somebody measure
- [Q9a](../open-questions.md#q9a-first-serial-module) — resolved here.
  [Q9b](../open-questions.md#q9b-first-llrp-reader-model) — still open, still gating M3b
