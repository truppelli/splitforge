# ThingMagic M7e-Pico

- **Vendor:** JADAK / Novanta (ThingMagic)
- **Protocol:** ThingMagic serial (Mercury API framing) over USB-UART
- **Adapter crate:** `crates/splitforge-thingmagic/` — not yet written
- **Status:** **experimental — under evaluation**
- **In the support matrix:** **no**, and see [§ Why this cannot become "supported"](#why-this-cannot-become-supported)

> Nothing on this page has been observed on a physical device. This file exists because
> [ADR-0024](../adr/0024-serial-reader-adapter-before-llrp.md) makes this module the first
> physical adapter, and [hardware-support.md](../hardware-support.md) requires a per-model
> notes file. Every claim below is **expected** behavior taken from vendor documentation and
> [hardware-plan.md](../hardware-plan.md), and each is a thing to verify rather than a thing
> that is known.
>
> When a module arrives, this page gets rewritten in the past tense. Anything still in the
> future tense afterwards was not tested.

## Why this module

The full argument is in [ADR-0024](../adr/0024-serial-reader-adapter-before-llrp.md). The
short version: it is a **current** part with an FCC modular grant, a US distributor, and a
documented serial protocol — which is what a manufactured product needs and what a scavenged
out-of-production LLRP reader can never be. What it gives up is precisely the set of things
Milestone 3 was written to test, which is why M3 split rather than weakened.

## The support checklist, scored

The nine rows are [hardware-support.md](../hardware-support.md#what-supported-requires)'s,
unaltered. "Expected" means the module should be able to close it and nobody has checked.

| # | Criterion | This module |
|---|---|---|
| 1 | Connects, and reconnects after reboot, cable pull, network interruption | **Partial** — USB re-enumeration, not a network path. The failure mode is real but it is a different one |
| 2 | Delivers reads under sustained load without dropping the connection | Expected |
| 3 | Timestamp type identified and handled correctly | **Recast** — see [Timestamps](#timestamps) |
| 4 | Clock offset and skew measured over a multi-hour session | **Cannot** — there is no reader clock to measure against |
| 5 | Antenna identity reported and correctly mapped | **Cannot** — one RF port until a second module is fitted |
| 6 | RSSI reported, or its absence documented | Expected — reported, and to be characterized empirically |
| 7 | Malformed/truncated frames handled without process exit | Expected — the parser is written to this before hardware arrives |
| 8 | Read counts reconciled against the journal | Expected |
| 9 | Behavior documented under `docs/readers/` | This file |

## Why this cannot become "supported"

Rows 4 and 5 are not "not yet." They are structural.

**Row 4** asks for clock offset and skew measured against the reader's clock. This module has
no UTC clock at all, so there is no offset to measure — not a small one, not a zero one.
Nothing a longer test run produces will close it.

**Row 5** asks for per-antenna identity mapped to checkpoints. The Pico has one RF port. A
second antenna means a second module, at which point each module presents as its own
`ReaderProvider` with its own reader and antenna identity — which is a valid deployment and
still does not make *this* row true of *one* module.

So this page stays here and the support matrix stays empty. If that reads as pedantic after
the module has run clean for eight hours, that is the moment the rule is doing its job:
[hardware-support.md](../hardware-support.md)'s claim is that an entry in that table is
something an organizer can stake an event on.

## Timestamps

**The module's timestamp is evidence, and it is not authoritative.** Expected behavior:

- The M7e reports a **relative millisecond** timestamp within a continuous-read session.
  It is **not** microseconds since boot, and it is **not** since power-on — it is since the
  read session started. *Verify this first;* the whole mapping below rests on it.
- It maps onto `ReaderTimestamp::Uptime { micros }`. That is a small, deliberate inaccuracy:
  the variant is named for uptime and this is not uptime. The alternative — inventing a
  variant, or worse, letting a relative value reach `ReaderTimestamp::Utc` — is how an uptime
  value becomes a date in 1970, which
  [clock discipline § 6](../clock-and-time-discipline.md#6-llrp-timestamp-specifics) exists
  to prevent.
- Every connect captures a **session anchor** `(received_at_utc, module_relative_us)`. Without
  it the relative value is a number nobody can interpret later; with it, it is a
  high-resolution interval anchored to a known instant.

Through the existing `Ingest::normalize` that produces:

```text
reader_timestamp  = None
reader_uptime_us  = Some(module_relative_us)
timestamp_source  = DeviceReceipt { reason: ReaderUptimeOnly }
```

— so the **Pi's receipt time is authoritative** and the module's finer-grained value is
preserved beside it. That is the conservative default for a first adapter, and because the
journal records both, revisiting the decision later happens at derivation time and never by
touching stored evidence
([clock discipline § 9](../clock-and-time-discipline.md#9-correction-happens-at-derivation-never-in-the-journal)).

### The accuracy sentence that belongs in any claim about this hardware

> On this hardware, timing accuracy is bounded by the **Pi's receive-time jitter** — USB
> serial latency plus scheduler jitter — not by the module's specifications.

The module's "300 tags/sec" is a throughput figure and says nothing about when a read is
stamped. Measuring the jitter is a deliverable of bench validation, not an assumption.

## Wire protocol

Framing is `0xFF` / length / opcode / payload / CRC-16.

[ADR-0004](../adr/0004-llrp-first-reader-adapter.md)'s parser rules apply here unchanged,
because they were never LLRP-specific:

- Parsing returns errors; it never panics. Malformed input is **expected**, not exceptional.
- Bounded frame sizes and allocation limits — a broken module must not exhaust memory. A
  frame claiming 64 KB of payload is a test case, not a hypothetical.
- The parser is a pure function over `&[u8]` with no I/O anywhere near it, so all of the
  above is testable before a module exists.

## Known unknowns

Four of these can turn a purchase into a box that cannot be used on arrival, and all four are
answerable before ordering:

| # | Question | Why it bites |
|---|---|---|
| 1 | What RF connector is on the carrier board — U.FL, MMCX, or SMA? | Determines the coax and whether a pigtail is needed. A CB-radio RG8X jumper terminates in PL-259, which is neither constant-impedance nor appropriate at 915 MHz and matches nothing here |
| 2 | Does the carrier board ship with a power supply? | The developer kit lists a 9 V supply; the board sold alone may not |
| 3 | What is the USB connector — micro-B, USB-C, or a bare header needing a USB-UART bridge? | A bare header is a second part nobody budgeted |
| 4 | Is the unit factory-set to the NA/FCC region, or must the region be commanded at boot? | Changes the adapter's startup sequence, and it is the compliance story rather than a detail |

Beyond those, and only answerable with hardware in hand:

- Read range and detection rate across a real lane at 24 dBm or below.
- RSSI distribution, which is what calibrates `--selection-rule first-above-rssi:` instead of
  guessing the threshold.
- Behavior on unplug mid-read, and whether module-reported counts reconcile with the journal.
- Whether the relative timestamp resets, wraps, or drifts across a long session.

## Deployment notes

Both are consequences of the module being a USB serial device, and neither exists yet.

**`PrivateDevices=yes`** in [`deploy/splitforge-edge.service`](../../deploy/splitforge-edge.service)
gives the service a private `/dev` containing only pseudo-devices. **`/dev/ttyUSB0` is not in
it** — as the unit stands today, the service cannot see this module at all. The fix grants the
tty class and nothing wider:

```ini
PrivateDevices=no
DevicePolicy=closed
DeviceAllow=char-ttyUSB rw
```

**`RestrictAddressFamilies=AF_UNIX` stays.** A serial adapter opens a file, not a socket, so
this widens no network surface whatsoever. It is M3b's LLRP reader that has to add `AF_INET`
deliberately and fail `apps/splitforge-edge/tests/unit_file.rs` until it does.

**`ttyUSB0` renumbers on re-enumeration**, so a udev rule gives it a stable name and the right
group — which also means the service account never needs adding to `dialout`:

```text
SUBSYSTEM=="tty", ATTRS{idVendor}=="XXXX", ATTRS{idProduct}=="XXXX", \
  SYMLINK+="splitforge-reader", GROUP="splitforge", MODE="0660"
```

The vendor and product IDs are `XXXX` because nobody has plugged one in. That is the honest
placeholder; filling it in with a guess would produce a rule that silently matches nothing.

## Validation plan

Do not invent one. [hardware-support.md](../hardware-support.md#what-supported-requires) has
the checklist and [hardware-plan.md § 7](../hardware-plan.md#7-software-plan) has the order —
static range map, motion trials, crowding, boundary, timed trials, failure trials, then a
full-duration rehearsal. Each stage's failures are cheaper to diagnose than the next's, which
is the whole reason for the order.

## What this page does not claim

- That the module can time a real event. Bench validation is a narrow, supervised, single-lane
  platform with manual backup. A wide finish chute, pack finishes, and unsupervised operation
  are all outside what one antenna at 24 dBm should be asked to do.
- That any of this closes **M3b**. M3b needs an LLRP reader, and
  [Milestone 5](../roadmap.md#milestone-5--field-reliability) depends on M3b.
