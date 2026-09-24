# ADR-0035: The first physical adapter is the M7E Hecto, on SparkFun's USB board

- **Status:** Accepted
- **Date:** 2026-09-24
- **Supersedes:** [ADR-0024](0024-serial-reader-adapter-before-llrp.md)'s choice of module, and
  nothing else in it

## Context

[ADR-0024](0024-serial-reader-adapter-before-llrp.md) made a serial module the first physical
adapter and chose the ThingMagic M7e-Pico, on its carrier board `M7E-PICO-CB`, at $345. The
order was costed against a $500 cap on the assumption that a Raspberry Pi 3 was already on
hand. It was not. A computer now has to come out of the same $500.

That broke the Pico's budget. The pre-order questions had already found that the carrier board
has **no USB**: its only interface is 3.3 V UART on a 15-pin Molex header, so it needs a
USB-UART bridge and a cable that nobody had costed. With a Pi added, the order came to about
$537 before shipping ([`Materials-and-Cost-Table.xlsx`](../Materials-and-Cost-Table.xlsx),
commit `1a87ed8`).

What ADR-0024 actually needs from the module is narrower than a part number. It needs a
current part with a modular grant and a US distributor, speaking the ThingMagic serial protocol
that `crates/splitforge-thingmagic/` was written against. The Pico was one such part. The
question is whether a cheaper one exists that does not reopen anything else ADR-0024 decided.

## Decision

**The first physical adapter is the ThingMagic M7E-HECTO module, on SparkFun's Simultaneous
RFID Reader board (SKU WRL-24738, $309.95).** It replaces the M7e-Pico.

**Everything else in ADR-0024 stands, unamended:**

- the M3a / M3b split, and that neither exit criterion is weakened;
- Milestone 5 depends on M3b;
- [ADR-0004](0004-llrp-first-reader-adapter.md), unamended;
- the `crates/splitforge-thingmagic/` boundary and `serialport` with `default-features = false`;
- the module's timestamp as evidence and not authority, with a per-connection session anchor;
- the module is *experimental — under evaluation* and does not enter the support matrix.

**No new crate and no new protocol.** The Hecto is an M7E-family module speaking the same
serial protocol, so it is served by the adapter that already exists.

### What the sources say, and how far they go

These are claims from documents and source code, not from a device, and the adapter holds them
to the same standard as everything else in `splitforge-thingmagic`: a claim is believed when a
capture agrees with it. Retrieved 2026-09-24:

| Source | SHA-256 |
|---|---|
| *ThingMagic M7E-HECTO User Guide*, 875-0106-01 Rev 1.4, `cdn.sparkfun.com/assets/8/5/2/4/d/M7E_HECTO_User_Guide.pdf` | `3bfc8d92933418c38fb3b6af314071dfbe6f7f5842aa440fd491b7b489882774` |
| *M7E-HECTO Spec Sheet*, 06/26/2023, DigiKey CDN | `b303026d8f15c220c31175c74cca9889271e67f83429a1a054a09206e3eae709` |
| SparkFun `SparkFun_UHF_RFID_Reader.cpp`, `master` | `3269d53c3156abb7a7af3c9960a186eace8c4e2b0bfa41b2f39ba72a2d107f18`, identical to the copy [vendor-documents.md](../readers/vendor-documents.md#sparkfun-simultaneous-rfid-tag-reader-library) archived on 2026-08-30 |

**The guide states the three limits the read path is built on.** Each one is the premise of an
accepted ADR, so each has been checked against this module's guide rather than assumed to carry
over from the Pico's:

- **§ 5.1.4.1:** *"The connected host processor's receiver must have the capability to receive
  up to 255 bytes of data at a time without overflowing. Flow control is not supported."*
  That is the premise of [ADR-0025](0025-m3a-proves-durability-above-the-transport.md)'s
  restated exit criterion and of `MAX_FRAME_LEN`.
- **§ 8.9.2:** the UART *"does not support control lines, so it is not possible for the module
  to detect a broken communications interface connection and stop streaming the tag
  results."* That is why [ADR-0033](0033-each-connection-starts-the-stream.md) stops and
  restarts the stream on every connection.
- **§ 8.9.3:** the timestamp is *"relative to the time the command to read was issued, in
  milliseconds"*. That is why ADR-0033 takes the session anchor when the start is sent. It is
  the same wording as the Pico's guide, so the disagreement with SparkFun recorded in
  [finding 18](../readers/vendor-documents.md#18-sparkfun-and-the-user-guide-disagree-on-what-the-tag-timestamp-counts-from)
  is unchanged rather than resolved.

§ 5.1.4.2 gives the default baud rate as 115200, which is what the adapter opens at. The
guide's section numbers differ from the Pico guide's, which put tag streaming at § 8.8; the
existing citations in the code and roadmap refer to the Pico guide.

**SparkFun's library knows this module by name.** `SparkFun_UHF_RFID_Reader.h` declares
`ThingMagic_M7E_HECTO` beside `ThingMagic_M6E_NANO`. On the path the adapter copied, the start
sequence ([ADR-0033](0033-each-connection-starts-the-stream.md)), the library branches on
module type in one place: `setRegion` remaps North America to `NA2` for the M6E Nano only, and
sends `NA` (`0x01`) for the Hecto. `Region::Na` already sends `0x01`. So the start sequence
needs no change for this module.

**The guide cannot be trusted on its regulatory identity.** Its labeling text, in the
regulatory section, gives the FCC ID as `QV5MERCURY7EP` and the ISED ID as `5407A-MERCURY7EP`,
and those are the Pico's. The FCC's own record lists the Hecto as **`QV5MERCURY7EH`**. That
section was carried over from the Pico's guide without being corrected. It is a reason to treat
this guide as a transcription that may be wrong in other places, as the Pico's CRC section was.

### What the board changes

**USB, through a CH340C.** The `ttyUSB` node belongs to the CH340C on SparkFun's board, driven
by the kernel's `ch341` driver. That is a `usbserial` driver, so
[ADR-0034](0034-the-service-opens-the-readers-port-and-nothing-else.md)'s
`DeviceAllow=char-ttyUSB rw` and the module that `deploy/splitforge.modules-load.conf` loads
still hold. The board has a switch labeled **UART** that selects between USB-C and its serial
header. It must be set to **USB**.

**The external antenna needs rework before first use.** The board ships with its PCB trace
antenna connected, which SparkFun gives a read range of one to two feet. To use the U.FL
connector, and so the 6 dBi panel antenna in the order, the **0 Ω resistor labeled RF has to be
moved** to the U.FL position with a soldering iron or hot air. No command selects between them.
This is a bench step, and it happens before any read range is measured.

**One RF path.** The board connects either the trace antenna or the U.FL, never both. So
per-antenna identity, row 5 of the support checklist, is structural again, as ADR-0024
originally scored it. The four switched U.FL ports on the Pico's carrier board, which had put
that row in doubt, belong to a board this project is no longer buying.

**The antenna in the order is within the grant.** § 5.7 lists authorized antennas up to
8.15 dBiL, and circular ones up to 11.15 dBiC. It lists a circularly polarized patch among them
(MTI-242043). § 5.8 allows an antenna of the same type with equal or lower gain without further
testing. The 6 dBi circularly polarized patch in the order is that case. This rests on the
guide's antenna table rather than the FCC filing, and the guide has already been wrong about
its own FCC ID. Check the filing before any claim goes on a label.

**More power, and more current.** Transmit power runs from 0 to +27 dBm, against the Pico's
+24. SparkFun gives the board's draw as over 700 mA at +27 dBm, with a 1 A limit on the board,
and warns that a 500 mA USB port may brown out above +22 dBm. The start sequence does not set
transmit power, so the module runs at whatever it defaults to. Whether to set it on every
connection, as the region is set, is a separate decision.

**A thermal cutoff the host cannot see.** SparkFun says the module disables RF above +60 °C.
When it does, it stops producing reads without a disconnection. The adapter would record that
as a *suspected* gap after the silence threshold, which is the same thing it records for an
empty field. The enclosure has a fan and heatsinks. Neither is a measurement.

## Consequences

### What this makes easy

- **The order fits the $500 with a computer in it.** The subtotal is $480.33.
- **No bridge to choose.** The udev rule can be narrowed to the CH340's vendor and product IDs,
  `1a86:7523`, from documentation, instead of waiting for a part choice.
- **The adapter is closer to its best evidence.** The start sequence was cross-checked against a
  library that names this module and branches for it, rather than one written for a different
  module in the family.

### What this makes hard

- **The code and docs name the Pico.** `crates/splitforge-thingmagic/` doc comments,
  `docs/readers/thingmagic-m7e-pico.md`, `hardware-plan.md` § 3–4 and the M3a section of the
  roadmap all describe the old part. A notes page for this module,
  `docs/readers/thingmagic-m7e-hecto.md`, replaces the Pico's, and the Hecto's documents join
  [vendor-documents.md](../readers/vendor-documents.md) with the hashes above. Both follow in
  their own changes.
- **A soldering step sits between delivery and the first real read.** An unmodified board reads
  a foot or two from its own trace antenna and nothing from the panel.
- **`command.rs` says `SetReadTxPower` is *"Capped at 24 dBm on this module"*.** That was the
  Pico.

### What we accept

**That two of these readers cannot be told apart by USB attributes.** CH340-family bridges are
not expected to report a USB serial number, so a udev rule cannot give two identical boards
stable names by `ATTRS{serial}`. It would have to match the physical USB port instead. That
constrains any two-reader deployment. Confirm it on arrival rather than taking it as given.

**That the guide is a weaker document than the Pico's was.** It is Rev 1.4. A later Rev 1.8 is
listed on `jadaktech.com`, which returned an empty body when fetched, the same way the Pico's
documents disappeared from that site. Its FCC ID is wrong. Everything above that comes from it
is held as a claim until a capture from this module agrees.

**That this is a development board.** $309.95 is a retail price for SparkFun's board, not a BOM
cost for a product. Whether the product's radio is a replaceable subassembly is
[hardware-plan.md](../hardware-plan.md#10-what-this-asks-someone-to-decide) decision 5, and it stays
open.

**That the one captured frame is still from an M6e.** `CAPTURED_FRAME` anchored the CRC and the
decoder. Changing modules does not make it better or worse evidence: it was never a capture from
the module being bought.

## Alternatives considered

| Alternative | Why not |
|---|---|
| Keep the M7e-Pico, with a bridge and a Molex cable | About $537 before shipping once a computer is in the order. Over the cap, and it adds a bridge, a cable and a wiring step to a part that was chosen for being simple to source |
| The Pico's `DEVKIT` | $741.40. It solves the power supply, not the budget |
| Drop the computer and wait for a Pi 3 | That repeats the pattern ADR-0024 was written to end: a milestone gated on hardware nobody has scheduled |
| Keep the Pico and cut the antenna or the RTC | The hardware plan already ruled out cutting the RTC or the tags. Cutting the antenna leaves a bench rig that cannot measure a read zone, which is half of what Phase 0 is for |
| An older SparkFun board built on the M6E Nano | ADR-0024 chose a *current* part so that the bill of materials is repeatable. An older module weakens that, and the Hecto is the board SparkFun sells now |

## References

- [ADR-0024](0024-serial-reader-adapter-before-llrp.md): superseded for its choice of module
  only
- [ADR-0025](0025-m3a-proves-durability-above-the-transport.md),
  [ADR-0030](0030-the-serial-adapter-waits-for-proof.md),
  [ADR-0033](0033-each-connection-starts-the-stream.md),
  [ADR-0034](0034-the-service-opens-the-readers-port-and-nothing-else.md): checked against
  this module's documents above, and unchanged
- [ADR-0036](0036-raspberry-pi-4-is-the-edge-target.md): the computer that made this
  necessary
- [`Materials-and-Cost-Table.xlsx`](../Materials-and-Cost-Table.xlsx): the order, from commit
  `1a87ed8`
- SparkFun, [product page](https://www.sparkfun.com/sparkfun-simultaneous-rfid-reader-m7e-hecto.html),
  [hardware overview](https://docs.sparkfun.com/SparkFun_Simultaneous_RFID_Reader_M7E/hardware_overview/)
  and [external antenna](https://docs.sparkfun.com/SparkFun_Simultaneous_RFID_Reader_M7E/external_antenna/)
  pages, retrieved 2026-09-24
- FCC ID [QV5MERCURY7EH](https://fccid.io/QV5MERCURY7EH)
- [Q9a](../open-questions.md#q9a-first-serial-module): re-resolved here
