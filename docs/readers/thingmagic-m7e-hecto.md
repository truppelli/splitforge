# ThingMagic M7E-HECTO, on SparkFun's USB board

- **Module:** ThingMagic M7E-HECTO — JADAK / Novanta
- **Board:** SparkFun Simultaneous RFID Reader, M7E Hecto — SKU WRL-24738
- **Protocol:** ThingMagic serial (Mercury API framing), over the board's CH340C USB-UART bridge
- **Adapter crate:** `crates/splitforge-thingmagic/`, composed by `splitforge-edge --serial`
- **Status:** **experimental — under evaluation**
- **In the support matrix:** **no**, and see [§ Why this cannot become "supported"](#why-this-cannot-become-supported)

> **Nothing on this page has been observed on a physical device.** None has been bought yet.
> This page exists because [ADR-0035](../adr/0035-the-first-module-is-the-m7e-hecto.md) makes
> this module the first physical adapter, and [hardware-support.md](../hardware-support.md)
> requires a per-model notes file. Every claim below comes from the documents listed in
> [vendor-documents.md](vendor-documents.md#m7e-hecto-user-guide) or from SparkFun's pages, and
> each is a thing to verify rather than a thing that is known.
>
> When a board arrives, this page gets rewritten in the past tense. Anything still in the future
> tense afterwards was not tested.

It replaces the [M7e-Pico](thingmagic-m7e-pico.md), which was never bought. Most of what that
page reasoned out, about timestamps, framing and the structural limits of a serial module,
holds here unchanged, and this page says so rather than repeating it.

## Why this module

[ADR-0024](../adr/0024-serial-reader-adapter-before-llrp.md) gives the argument for a current
serial module over a scavenged LLRP reader, and
[ADR-0035](../adr/0035-the-first-module-is-the-m7e-hecto.md) gives the reason for this one: the
Pico needed a USB-UART bridge and a cable, and with a computer in the same $500 it no longer fit.
SparkFun's board puts the same protocol behind a USB-C port.

## The board

The module guide describes the module. What the Pi connects to is SparkFun's board, and these
facts come from SparkFun's pages
([vendor-documents.md](vendor-documents.md#sparkfuns-pages-for-its-m7e-hecto-board)), retrieved
2026-09-24.

**USB, through a CH340C.** *"A built-in USB-C connector (via a CH340C converter) allows you to
plug it directly into a computer."* So the `ttyUSB` node belongs to the CH340C, not to the
module.

**A switch selects the serial interface.** It is labeled **UART** and *"allows the user to
toggle between the two serial interfaces: USB-C (**USB**) and the Serial Header (**SER**)."* It
must be on **USB**. On **SER**, the Pi would see a CH340C with nothing behind it: a port that
opens and a start sequence nobody answers.

**The panel antenna needs a resistor moved.** The board carries a PCB trace antenna and a U.FL
connector, and *"the trace antenna is enabled by default."* SparkFun gives the trace antenna a
read range of *"roughly one to two feet"*. To use the U.FL: *"Carefully reflow the 0kΩ resistor
labeled **RF** to move it to the u.FL position."* No command selects between them. Until that is
done, the 6 dBi panel antenna in the order is connected to nothing.

**Power and current.** Supply 3.3–5 V, with *"an internal current limiting circuit"* at 1 A.
*"With RF power level set to 27dBm the board can draw over 700mA (3.6W @ 5V)"*, and a ~500 mA
USB port *"may cause brownout above 22dBm"*. The Pi 4 gives its USB ports 1.2 A in total from a
3 A supply ([ADR-0036](../adr/0036-raspberry-pi-4-is-the-edge-target.md)).

**Heat.** The board has *"a large ground plane heatsink on the bottom"*, and SparkFun suggests
attaching a heatsink or lowering power or duty cycle to avoid throttling. The module guide says
overheating turns RF off and is reported as `0x504`
([finding 27](vendor-documents.md#27-overheating-is-reported-as-0x504)).

**Solder jumpers.** `PWR` (the power LED), `VIN SEL` (ties USB, VIN and VCC together) and
`SHLD` (USB shield to ground), all closed by default. None needs changing for this use.

**Mounting.** 60.96 × 35.56 mm, with four 4-40 holes.

## The support checklist, scored

The nine rows are [hardware-support.md](../hardware-support.md#what-supported-requires)'s,
unaltered. "Expected" means the module should be able to close it and nobody has checked.

| # | Criterion | This module |
|---|---|---|
| 1 | Connects, and reconnects after reboot, cable pull, network interruption | **Partial** — USB re-enumeration, not a network path. The failure mode is real but it is a different one |
| 2 | Delivers reads under sustained load without dropping the connection | Expected |
| 3 | Timestamp type identified and handled correctly | **Recast** — see [Timestamps](#timestamps) |
| 4 | Clock offset and skew measured over a multi-hour session | **Cannot** — there is no reader clock to measure against |
| 5 | Antenna identity reported and correctly mapped | **Cannot** — one antenna port, and one RF path on the board |
| 6 | RSSI reported, or its absence documented | Expected — reported, and to be characterized empirically |
| 7 | Malformed/truncated frames handled without process exit | Expected — the codec is tested to this without hardware |
| 8 | Read counts reconciled against the journal | Expected, for reads that reached the host. See below |
| 9 | Behavior documented under `docs/readers/` | This file |

## Why this cannot become "supported"

Rows 4 and 5 are structural, for the same reasons the [Pico page](thingmagic-m7e-pico.md#why-this-cannot-become-supported)
gives, and one of them is firmer here than it was there.

**Row 4.** The module has no UTC clock, so there is no offset to measure.

**Row 5.** The module has one antenna port. § 5.1.2 says so, and the fault table says a command
naming *"an antenna value other than 1"* is refused
([finding 26](vendor-documents.md#26-one-antenna-port-stated-twice)). SparkFun's board connects
that port to either the trace antenna or the U.FL, never both. The Pico's carrier board had
appeared to carry four switched U.FL ports, which had put this row in doubt. This board does not.
A second antenna means a second board, each its own `ReaderProvider`.

**And the delivery count is structural.** The module *"does not support control lines"*, cannot
*"detect a broken communications interface connection"*, and keeps streaming into it
([§ 8.9.2](vendor-documents.md#23-the-protocol-sections-are-the-pico-guides-one-section-later-in--8)).
Reads it sends during a disconnection are gone, and nothing on the module counts them.
[ADR-0025](../adr/0025-m3a-proves-durability-above-the-transport.md) restates M3a's exit
criterion around what a serial link can prove for that reason.

So this page stays here and the support matrix stays empty.

## Timestamps

Unchanged from the [Pico page](thingmagic-m7e-pico.md#timestamps), because the guide's wording
is unchanged. § 8.9.3: the timestamp is *"the time the tag was read, relative to the time the
command to read was issued, in milliseconds."*

- It is evidence and not authoritative. It maps onto `ReaderTimestamp::Uptime { micros }`, and
  the Pi's receipt time is what counts.
- Each connection anchors it when the start command is sent
  ([ADR-0033](../adr/0033-each-connection-starts-the-stream.md)).
- SparkFun describes the same field as time since the last keep-alive. That disagreement is
  [finding 18](vendor-documents.md#18-sparkfun-and-the-user-guide-disagree-on-what-the-tag-timestamp-counts-from),
  and changing modules does not settle it.

> On this hardware, timing accuracy is bounded by the **Pi's receive-time jitter** — USB serial
> latency plus scheduler jitter — not by the module's specifications.

## Wire protocol

The framing, the CRC's coverage, the 255-byte ceiling and the default baud rate read the same in
this module's guide as in the Pico's
([finding 23](vendor-documents.md#23-the-protocol-sections-are-the-pico-guides-one-section-later-in--8)).
The codec in `splitforge-thingmagic` needs no change. Its CRC is anchored on a captured frame,
not on § 7.3, which still calls the algorithm CCITT and is still wrong about it.

**The start sequence needs no change either.** SparkFun's library, which the sequence was
cross-checked against, names this module and branches for it once: North America is sent as
`0x01`, which is what `Region::Na` sends
([finding 29](vendor-documents.md#29-sparkfuns-library-branches-for-this-module-once-and-only-for-the-region)).

## Regulatory

- **FCC ID `QV5MERCURY7EH`**, a modular grant. The guide's own labeling text gives the Pico's
  ID, [finding 24](vendor-documents.md#24-the-guides-regulatory-section-names-the-picos-ids),
  so take the ID from the FCC's record, not from the guide.
- **Region is set on every connection** and has no default: `splitforge-edge --serial` requires
  `--region`. The module is pre-configured for eight regions, so it is selectable rather than
  fixed. **EU and EU2 are not supported on this module**; EU3 is
  ([finding 25](vendor-documents.md#25-the-hecto-supports-neither-eu-nor-eu2)).
- **The antenna.** § 5.7 authorizes antennas up to 8.15 dBiL, and circular ones up to
  11.15 dBiC, and lists a circularly polarized patch. § 5.8 allows the same type at equal or
  lower gain without further testing. The 6 dBi circular panel in the order is that case, on the
  guide's reading. Check the FCC filing before relying on it.
- **Read power is set on every connection**, from `--read-power`, which has no default
  ([ADR-0038](../adr/0038-each-connection-sets-the-read-power-the-operator-chose.md)). The
  maximum is +27 dBm ([finding 28](vendor-documents.md#28-27-dbm-and-what-it-takes-to-reach-it)),
  and the module refuses more. After setting it, the service asks what the module applied and
  the range it accepts, and reports both on `/health`.

## Known unknowns

### Answered from documentation before ordering

The same four questions the Pico was asked, answered for this board:

| # | Question | Answer | Confidence |
|---|---|---|---|
| 1 | RF connector | **U.FL**, plus a PCB trace antenna that is connected by default. Moving the `RF` resistor selects the U.FL | High — SparkFun's hardware overview and external antenna pages |
| 2 | Power supply | **USB bus power.** 3.3–5 V, 1 A limit, over 700 mA at +27 dBm | High — SparkFun's hardware overview |
| 3 | USB connector | **USB-C, through a CH340C**, with a switch to select it | High — SparkFun's product page and hardware overview |
| 4 | Region | **Selectable.** Pre-configured for eight regions including FCC. **Open:** whether a region survives a power cycle | High that it is selectable — spec sheet and § 8.1 |

### Only answerable with hardware in hand

Run the first sessions with `--capture` ([ADR-0040](../adr/0040-a-serial-session-can-be-captured-byte-for-byte.md)).
Every item below is answered in bytes, and a frame that surprises the decoder can then be pasted
into a test, as `CAPTURED_FRAME` was.

- Whether the module accepts the start sequence. A command sequence copied from source code is
  believed when a module answers it.
- Whether the empty-field frame, `0x22` with status `0x0400`, arrives about once a second
  ([finding 17](vendor-documents.md#17-an-empty-field-produces-a-frame-at-the-end-of-every-search-cycle)).
  It is the likely liveness signal for [Q14](../open-questions.md#q14-reader-silence-threshold).
- What an idle and an unplugged CH340C `ttyUSB` return. A pseudo-terminal returns `TimedOut` and
  `BrokenPipe` ([finding 20](vendor-documents.md#20-an-idle-port-times-out-and-a-closed-one-does-not)).
  A USB bridge may not.
- Whether a region setting persists. The default read power no longer matters: it is set on
  every connection. Whether the module accepts the power command, and what it reports applying,
  are first-session checks.
- Whether the CH340C reports a USB serial number. CH340-family bridges are not expected to.
- What a streaming module sends when it overheats.
- Read range and detection rate across a real lane, at the power finally chosen.
- The RSSI distribution, which is what calibrates `--selection-rule first-above-rssi:`.
- Whether the relative timestamp wraps or drifts across a long session.

## Deployment notes

The unit already opens this device and nothing else
([ADR-0034](../adr/0034-the-service-opens-the-readers-port-and-nothing-else.md)):

```ini
PrivateDevices=no
DevicePolicy=closed
DeviceAllow=char-ttyUSB rw
```

The CH340C is driven by the kernel's `ch341` driver. That is a `usbserial` driver, so its port
is a `ttyUSB` node and the directives above cover it.
[`deploy/splitforge.modules-load.conf`](../../deploy/splitforge.modules-load.conf) loads
`usbserial` at boot so that `char-ttyUSB` resolves even when the board is unplugged at startup.

**The udev rule** in [`deploy/99-splitforge-reader.rules`](../../deploy/99-splitforge-reader.rules)
gives the port a stable name, `/dev/splitforge-reader`, and the service's group. It matches
the CH340 by vendor and product ID, `1a86:7523`, which is the CH340's entry in the `ch341`
driver's device table. That comes from documentation: confirm it with
`udevadm info /dev/ttyUSB0` when a board is plugged in. `apps/splitforge-edge/tests/unit_file.rs`
fails if the rule's IDs are not the ones this page names. Two consequences follow before two
boards share a Pi:

- **Two identical boards are indistinguishable** by vendor and product ID, and CH340-family
  bridges are not expected to carry a serial number that would separate them. A rule would have
  to match the physical USB port instead.
- **The rule names a port, not a reader.** Reader identity stays in configuration, in the
  database.

## Validation plan

**The first sessions are written down** in [the bench runbook](thingmagic-m7e-hecto-bench.md):
what to do, what to expect, what to keep, and which box each closes.

Do not invent one. [hardware-support.md](../hardware-support.md#what-supported-requires) has the
checklist and [hardware-plan.md § 7](../hardware-plan.md#7-software-plan) has the order. Two
steps come before any of it: set the **UART** switch to **USB**, and move the **RF** resistor to
the U.FL.

## What this page does not claim

- That the module can time a real event. Bench validation is a narrow, supervised, single-lane
  platform with manual backup.
- That any of this closes **M3b**. M3b needs an LLRP reader, and
  [Milestone 5](../roadmap.md#milestone-5--field-reliability) depends on M3b.
