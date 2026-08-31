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
  read session started. **Verified**, against user guide § 8.8.3: *"the time the tag was read,
  relative to the time the command to read was issued, in milliseconds."* The anchor is the read
  command, which is more specific than "session" — and carries a consequence the last bullet
  below picks up.
- It maps onto `ReaderTimestamp::Uptime { micros }`. That is a small, deliberate inaccuracy:
  the variant is named for uptime and this is not uptime. The alternative — inventing a
  variant, or worse, letting a relative value reach `ReaderTimestamp::Utc` — is how an uptime
  value becomes a date in 1970, which
  [clock discipline § 6](../clock-and-time-discipline.md#6-llrp-timestamp-specifics) exists
  to prevent.
- Every connect captures a **session anchor** `(received_at_utc, module_relative_us)`. Without
  it the relative value is a number nobody can interpret later; with it, it is a
  high-resolution interval anchored to a known instant.
- **And every read command starts a new epoch**, so one anchor per connect is not enough.
  § 8.8.3 again: *"If the Tag Read Meta Data is not retrieved from the Tag Buffer between read
  commands, there will be no way to distinguish order of tags read with different read command
  invocations."* Two reads from either side of a read command are not comparable as intervals —
  so an event wants as few read commands in it as it can be run with, and a fresh anchor at
  every one of them.

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

**And framing is all the user guide documents.** § 7 is two diagrams and the CRC's covered
range; there is no opcode table, no command list, and no tag-report layout in its 61 pages,
because the vendor's position is that *"ThingMagic does not support bypassing the MercuryAPI to
send commands to the ThingMagic module directly."* The framing above could be written from the
document. Everything above the framing cannot, and has to come from the MercuryAPI SDK — code
rather than a specification, with a licensing question attached
([vendor-documents.md](vendor-documents.md#the-command-set-is-not-in-this-document)).

## Known unknowns

### The four pre-order questions — answered from documentation

All four were answerable before ordering, and all four have now been asked. **None of this is
observed**; it is vendor documentation and distributor listings, which is exactly the standard
the rest of this page holds itself to. Confidence is stated per row because it genuinely
differs, and two of the four changed the bill of materials.

| # | Question | Answer | Confidence |
|---|---|---|---|
| 1 | RF connector on the carrier board | **U.FL (I-PEX compatible)** — and there are **four** of them, with RF switching on the board | Good — [DigiKey TechForum][q1], a moderator relaying manufacturer schematics and mechanical drawings. Not vendor-published |
| 2 | Does the carrier board ship with a power supply? | **No.** The DEVKIT lists "Board(s), Cable(s), Power Supply, Accessories" at $741.40; the bare `M7E-PICO-CB` at $345.00 lists none | Moderate — inferred from distributor packaging fields, not a vendor statement |
| 3 | USB connector | **There is no USB.** The module is `UART; 3.3V logic levels 9.6 to 921.6 kbps` and the carrier board brings power and control out on a 15-pin **Molex 532611571** (1.25 mm pin centers) | High for "UART only" — [module spec sheet][spec]. Good for the Molex part number — user guide |
| 4 | Factory-set to NA/FCC? | **Neither, quite.** It is a *"Single SKU for Global Use"*, **pre-configured** for FCC (NA, SA) 902–928 MHz alongside ETSI, TRAI, KCC, ACMA, SRRC-MII, MIC and `Open` | High that the SKU is global — [module spec sheet][spec]. **Open:** whether a region selection persists across a power cycle |

[q1]: https://forum.digikey.com/t/m7e-pico-cb-rf-connector-and-antenna-port-clarification/70439
[spec]: https://mm.digikey.com/Volume0/opasdata/d220001/medias/docus/5735/M7E-PICO-Spec%20Sheet_06262023.pdf

**Question 3 is the expensive one, and it is the answer nobody wanted.** The plan called a
bare header *"a second part nobody budgeted"*, and a bare header is what it is. The $345 board
cannot be plugged into a Pi — it needs a USB-UART bridge and a Molex 1.25 mm cable to reach it.
That has a consequence beyond the ~$15: **the `/dev/ttyUSB0` that appears belongs to the
bridge, not to the module.** So the udev rule below matches an FTDI or CP210x vendor and
product ID, and two identical bridges are indistinguishable by VID/PID alone — telling them
apart needs `ATTRS{serial}`. That is a real constraint on any two-antenna deployment, and it
was invisible while the interface was assumed to be USB.

**Question 1 confirms the coax line was wrong**, which [hardware-plan § 3](../hardware-plan.md#3-phase-0--bench-validation-500-now)
already suspected in writing. U.FL to the antenna's connector needs a pigtail in LMR-195 or
RG316; the RG8X/PL-259 jumper from a CB-radio supplier matches nothing on either end.

**Question 4 does not close, and the open half is the half that matters.** A global SKU means
the region is selectable rather than fixed, so the adapter must set it explicitly at startup
rather than assume it — which is the right behavior whether or not the setting persists, and
is the compliance story rather than a detail.

### Question 1 also challenges row 5 of the checklist above

The scoring above says row 5 **cannot** be closed because *"the Pico has one RF port. A second
antenna means a second module."* The first half is true — the module's antenna connector is a
`Single 50 Ω connection (board-edge)`. **The second half may not be.** The carrier board
appears to carry four U.FL ports and the switching to drive them, and the module supports up to
16 *logical* antennas through `PortSwitchGPO` on its four GPIO lines. If that holds, per-antenna
identity is reachable on one module and row 5 is not structural at all.

It is deliberately **not** rewritten above, for two reasons. The source is a forum answer rather
than a published datasheet, and the vendor's own documentation portal is currently unreachable
(see below). And even if it holds, only one antenna is active at a time — so two checkpoints on
one module time-share the radio, and a runner crossing while the switch is on the other port is
a missed read. That is a real trade-off for a finish line, not a free second antenna.

**Row 4 is untouched by any of this.** There is still no reader clock, and nothing here changes
that.

### The vendor's documentation has moved

`jadaktech.com/documents-downloads/…` now 302-redirects to `novanta.com/precision-medicine/`,
and the user-guide PDFs under `jadaktech.com/wp-content/uploads/…` return 404. The module
specification sheet survives on DigiKey's CDN, which is why it carries the load above.

**Both documents have since been located** on DigiKey's CDN, including the user guide the
protocol assumptions came from — `875-0093-01 Rev 2.3`, which `jadaktech.com` no longer
serves. Their URLs, retrieval dates, SHA-256 hashes, and every statement the codec depends on
are recorded in [vendor-documents.md](vendor-documents.md). The PDFs themselves are not in the
repository: the user guide's § 1 forbids reproduction without written authorization, and this
repository is public.

Retrieving them settled two of the open items above and put a third in a different light — the
CRC coverage assumption is confirmed exactly, `MAX_DATA_LEN` turned out to be wider than the
protocol allows and has since been corrected, and § 8.7 answers the antenna-port question from
the user guide rather than from a forum. All three are written up in
[vendor-documents.md](vendor-documents.md#what-retrieving-these-already-settled); none is acted
on there.

### Still unknown, and only answerable with hardware in hand

- Read range and detection rate across a real lane at 24 dBm or below.
- RSSI distribution, which is what calibrates `--selection-rule first-above-rssi:` instead of
  guessing the threshold.
- Behavior on unplug mid-read, and whether module-reported counts reconcile with the journal.
- Whether the relative timestamp wraps or drifts across a long session. *Resets* is answered:
  it restarts at every read command (§ 8.8.3). Wrap width and drift rate are not in the guide.

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

**And they will not be ThingMagic's.** Question 3 above settles what this device is: the
carrier board speaks 3.3 V UART on a Molex header and has no USB at all, so the `ttyUSB`
node belongs to whatever USB-UART bridge is wired to it — FTDI, Silicon Labs, WCH. The rule
above will match *the bridge*, which has two consequences worth knowing before two of them
are on one Pi:

- Two identical bridges are **indistinguishable** by `idVendor`/`idProduct`. Telling them
  apart needs `ATTRS{serial}`, and not every bridge ships with a unique one — the cheapest
  CH340 boards frequently do not.
- The rule identifies a *cable*, not a reader. Moving a bridge between two modules moves the
  name with it, so `splitforge-reader` names a port and never a reader identity. Reader
  identity stays where it already is: configuration, in the database.

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
