# Hardware Plan

> Status: **partly adopted.** Decisions 1, 2, and 3 of
> [§ 10](#10-what-this-asks-someone-to-decide) are now recorded in
> [ADR-0024](adr/0024-serial-reader-adapter-before-llrp.md): M3 has split into M3a/M3b, a
> serial module is the first physical adapter, and `serialport` may join the read path. The
> module is now the M7E-HECTO on SparkFun's USB board
> ([ADR-0035](adr/0035-the-first-module-is-the-m7e-hecto.md)), which replaced the M7e-Pico, and
> the computer is a Raspberry Pi 4 ([ADR-0036](adr/0036-raspberry-pi-4-is-the-edge-target.md)).
> **Decision 4 is half decided, and 5 and 6 are still open and bind nothing.** Everything below
> about the shipped compute platform, the radio subassembly, and Phase 2 remains a proposal.
>
> **No hardware has been ordered.** The pre-order questions in
> [§ 3](#four-questions-to-answer-before-ordering--asked-twice) have been answered twice from
> documentation rather than from a device. The first time, they found that the Pico's $345
> carrier board had no USB port at all. That, and a computer the budget had not expected to
> buy, is why the order changed.
> Companion to [hardware-support.md](hardware-support.md) and [roadmap.md](roadmap.md).
> The parts themselves are in [`Materials-and-Cost-Table.xlsx`](Materials-and-Cost-Table.xlsx),
> which is the submission template rather than a file this repository designed.

## 1. What this is for

[Milestone 3](roadmap.md#milestone-3--one-physical-reader) is gated on hardware nobody has
bought, and [Q9](open-questions.md#q9-first-reader-model) has had no owner since Milestone 0.
Milestones 4 and 5 were built around the gate rather than through it, which worked, and has
now run out of road: every remaining item in M5 needs a Pi with reads flowing into it.

This is a plan to unblock that under a real budget — **$500 now, $2,000 if the first phase
succeeds** — and to spend it so that it ends at a unit somebody could buy rather than at a
bench demo.

[`Materials-and-Cost-Table.xlsx`](Materials-and-Cost-Table.xlsx) is the file a purchasing
process receives. This document is why each line is in it. The spreadsheet's schema is fixed
by the funder, so it is the one file here that is not written to this repository's
conventions — it carries real vendors, live links, and quoted prices, and it is authoritative
over any target price quoted below.

## 2. The decision: an embedded module, not a fixed LLRP reader

Choosing an embedded UHF module over a networked LLRP reader is right for a product and
wrong for Milestone 3 as written. Both halves are true, and the plan is more useful for
saying so.

A used Impinj Speedway or Zebra FX7500 satisfies every line of
[hardware-support.md](hardware-support.md) today. It is also a dead end: out of production,
priced by whatever a liquidator lists this month, and impossible to build a repeatable bill
of materials around. **You cannot certify, manufacture, or support a product whose central
component is scavenged.**

A ThingMagic M7E-family module is the opposite trade. It is a current part with an FCC
modular grant, a US distributor, and a documented serial protocol — what a manufactured
product needs. What it gives up is precisely the set of things Milestone 3 exists to test.
This section was written for the M7e-Pico. The scoring holds for the M7E-HECTO that replaced
it ([ADR-0035](adr/0035-the-first-module-is-the-m7e-hecto.md)), and the table below is scored
for the Hecto.

### Milestone 3's checklist, scored against both

The nine rows are copied from [hardware-support.md](hardware-support.md#what-supported-requires).
Nothing has been softened.

| Criterion | M7E-HECTO | Fixed LLRP reader |
|---|---|---|
| Connects, and reconnects after reboot, cable pull, network interruption | **Partial** — USB re-enumeration, not a network path | Closes |
| Delivers reads under sustained load without dropping the connection | Closes | Closes |
| Timestamp type identified and handled correctly | **Recast** — no UTC clock exists; relative-ms maps to `Uptime` with a session anchor | Closes |
| Clock offset and skew measured over a multi-hour session | **Cannot** — there is no reader clock to measure against | Closes |
| Antenna identity reported and correctly mapped | **Cannot** — one antenna port until a second module is fitted | Closes |
| RSSI reported, or its absence documented | Closes | Closes |
| Malformed/truncated frames handled without process exit | Closes | Closes |
| Read counts reconciled against the journal | Closes | Closes |
| Behavior documented under `docs/readers/` | Closes | Closes |

Six close, one is recast into something narrower and honest, and two cannot be closed at
all on one single-port module with no clock.

> **The antenna-identity row was briefly in question, and is not any more.** The Pico's
> carrier board appeared to carry four switched U.FL ports, which would have made per-antenna
> identity reachable on one module
> ([the Pico's notes](readers/thingmagic-m7e-pico.md#question-1-also-challenges-row-5-of-the-checklist-above)).
> SparkFun's Hecto board has one RF path, and the Hecto guide says the module accepts no
> antenna but 1, so the row is a "cannot" again.

### The roadmap consequence

**M3 splits rather than weakens.**

- **M3a — Serial reader.** Takes the six criteria the module can close, plus the two it
  recasts, and states the two it cannot.
- **M3b — Networked LLRP reader.** Keeps all nine verbatim and stays gated exactly as M3 is
  today. [ADR-0004](adr/0004-llrp-first-reader-adapter.md) stands unchanged: LLRP remains the
  first *networked* protocol. The module is the first *physical* adapter.
- **M5 depends on M3b, not M3a.**

Nothing gets marked complete on weaker evidence than it asked for. That property is the
reason the rest of this document is worth reading, and it is not a rhetorical flourish —
[hardware-support.md](hardware-support.md)'s rule is that a support claim is a claim an
organizer can stake an event on.

### Two RF paths, not a splitter

A finish line wants two antennas. The module has one port. The answer is **two modules**, each
presenting as its own `ReaderProvider` with its own reader and antenna identity — not one
module behind a splitter.

A splitter halves transmit power, complicates matching, and destroys the per-antenna
identity that [timing-model.md](timing-model.md) depends on and that
`splitforge reader map --antenna` already exposes to the operator. Two modules preserve it,
and cost one module.

**A switched carrier board is a third option, and it is not a splitter.** SparkFun's Hecto
board is not one, but the Pico's appeared to be, and a product carrier could be. With switched
ports, one module can address two antennas *sequentially* with
full power into each and a logical antenna number on every read — which keeps the identity a
splitter destroys. What it does not keep is simultaneity: one antenna is live at a time, so a
runner crossing while the switch is on the other port is a read that never happens. For a
finish line that is a worse trade than it sounds, and it is the measurement that decides
between the two — not an argument to be settled on paper.

## 3. Phase 0 — bench validation ($500, now)

Buys the answer to one question: **does a serial UHF module, driven by SplitForge's own
adapter, put real reads in the journal and keep them there?**

The order is [`Materials-and-Cost-Table.xlsx`](Materials-and-Cost-Table.xlsx), which is
authoritative over this table. As of commit `73a75cd`:

| Line | Vendor | |
|---|---|---|
| SparkFun Simultaneous RFID Reader, M7E Hecto (WRL-24738) | SparkFun | $309.95 |
| UHF antenna, 902–928 MHz, circular, 6 dBi, IP65 (L-com LCANFP1031) | L-com | $68.99 |
| U.FL to SMA-male pigtail, RG178, 15 cm | Amazon | $9.99 |
| DS3231 RTC | Adafruit 3013 | $17.50 |
| CR1220 coin cell for the RTC | Adafruit 380 | $0.95 |
| Raspberry Pi 4 Model B, 2 GB | PiShop | $55.00 |
| Antenna stand and lane materials | Home Depot | $15.00 |
| Raspberry Pi 4 heatsink set | PiShop | $2.95 |
| **Subtotal** | | **$480.33** |
| Shipping allowance | | $19.67 |
| **Total against the cap** | | **$500.00** |

Carried **in kind**, at no cost to the budget: the Pi's 5.1 V 3 A USB-C supply, a 64 GB
high-endurance microSD, a USB-C to USB-A data cable, M2.5 mounting hardware for both boards,
the 3D-printed enclosure, a 5 V fan, four jumper wires for the RTC, and 100 borrowed EPC Gen2
inlays.

**This is not the order this section first described**, and each change has a reason:

- **The radio is SparkFun's M7E Hecto board, not the M7E-PICO-CB.** The Pico's carrier board
  turned out to have no USB, and with a computer to buy as well it no longer fit
  ([ADR-0035](adr/0035-the-first-module-is-the-m7e-hecto.md)).
- **A Pi 4 is bought, where a Pi 3 was carried in kind.** The Pi 3 was not on hand after all
  ([ADR-0036](adr/0036-raspberry-pi-4-is-the-edge-target.md)).
- **The powered USB hub is gone.** A Pi 4 with its 3 A supply gives its USB ports 1.2 A, which
  covers the reader's 700 mA or more at +27 dBm.
- **The RG8X coax is now a U.FL pigtail.** RG8X terminates in PL-259, which matched neither the
  board nor the antenna.
- **The CR1220 is its own line**, because the DS3231 breakout does not include one.

Cautions the spreadsheet cannot express, which belong in the submission's notes rather than
being discovered later:

- **The shipping figure is a plug, not an estimate.** It is `500 − subtotal`, so the total lands
  on exactly $500 by construction. The order spans five shippers — SparkFun, L-com, Amazon,
  Adafruit and PiShop — and five shipments will cost more than $19.67. The subtotal needs
  headroom, or a line has to move in kind.
- **Two prices are not firm.** The pigtail's is an estimate, and the CR1220 was out of stock at
  Adafruit when checked. Any CR1220 works.
- **Borrowed tags are a validation risk, not just a cost saving.** The read-zone
  characterization in step 7 depends on knowing the tags' band and inlay class, and on being
  able to mount them on real bib material. Confirm the borrowed stock is 902–928 MHz EPC Gen2
  before relying on it, and keep a small quantity of owned tags if the loan is short-term.
- **Two bench steps come before the first read, and neither is a part.** The board's **UART**
  switch must be set to **USB**. The board ships wired to its PCB trace antenna, and the 0 Ω
  resistor labeled **RF** has to be moved to the U.FL position with a soldering iron or hot air
  before the panel antenna is connected to anything
  ([the reader notes](readers/thingmagic-m7e-hecto.md#the-board)).
- **The pigtail must be SMA male, standard polarity**, to meet the antenna's SMA female. An
  RP-SMA pigtail looks the same and does not mate.

**If the order lands above the cap, the antenna is the line to trim.** A cheaper panel works for
a deliberately narrow lane, and RSSI is being measured empirically regardless. A replacement
must be of a type and gain the Hecto's grant already covers
([ADR-0035](adr/0035-the-first-module-is-the-m7e-hecto.md#what-the-board-changes)). The RTC and
the tags are not trimmable: the RTC removes a whole class of silent wrongness for the price of
a coin cell, and without tags the reader has nothing to read.

### Four questions to answer before ordering — **asked**, twice

Each could turn an order into a box that cannot be used on arrival.

**Asked first of the M7e-Pico**, whose answers are in
[its notes](readers/thingmagic-m7e-pico.md#the-four-pre-order-questions--answered-from-documentation).
The third found that its carrier board had no USB at all, which is half of why the order
changed.

**Asked again of SparkFun's Hecto board**, from documentation rather than a device. The answers,
their sources, and how far each can be trusted are in
[the Hecto's notes](readers/thingmagic-m7e-hecto.md#answered-from-documentation-before-ordering):

1. **RF connector: U.FL**, plus a PCB trace antenna that is connected by default. Using the
   U.FL means moving a resistor.
2. **Power: USB bus power.** 3.3–5 V, a 1 A limit, and over 700 mA at +27 dBm.
3. **USB: yes**, through a CH340C, with a switch to select it.
4. **Region: selectable**, and set by the adapter on every connection. **Still open:** whether a
   region survives a power cycle.

**The user guide is archived**, as this section asked before the first order. The Hecto's guide,
Rev 1.4, is on SparkFun's CDN and hashed in
[vendor-documents.md](readers/vendor-documents.md#m7e-hecto-user-guide). The vendor lists a later
Rev 1.8 that returned nothing when fetched.

## 4. Phase 1 — field unit, and finding the real BOM ($2,000)

Contingent on Phase 0. Two jobs: build something that survives an actual race day, and
discover what the product costs to build — which is not what Phase 0 cost.

> **$309.95 is a retail price, not a BOM price.** SparkFun's board is a development board
> sold in ones. The bare module in production quantities is a fraction of it, and quoting the
> dev-board price as cost of goods makes the product look unviable when it is not.
> Requesting a qty-100 and qty-500 quote costs an email and has no hardware dependency —
> do it the week Phase 0 arrives.

| | |
|---|---|
| Core | $1,355 |
| Support | $305 |
| **Allocated** | **$1,660** |
| Unallocated — hold against quotes coming in high | $340 |

### Optional: buy an LLRP reader anyway, if one appears cheap

$350 buys a used FCC-band Impinj R220/R420 or Zebra FX7500 **as a test instrument**, not as
product hardware. It is the only way to close M3b, and M5 depends on M3b. It also gives a
reference to measure the unit against: a known-good reader clock, real `UTCTimestamp`
behavior, and a second antenna identity to validate the mapping code against.

Buy it if the price is right. Do not block on it — that is what blocked M3 for three
milestones.

## 5. The product architecture

Three decisions that determine whether Phase 1's field unit can become a manufactured one.
All three are cheaper now than as a retrofit.

### Compute: Compute Module for the product, the Pi 4 until then

The Pi 4 Model B is a development board: no availability commitment suited to a product, no
eMMC, and no RTC. The Compute Module 4 or 5 is the part Raspberry Pi sells for embedding —
guaranteed availability into the 2030s, onboard eMMC, and a carrier board designed once.

**This section first proposed keeping the Pi 3 as the minimum supported platform**, on
[ADR-0002](adr/0002-raspberry-pi-target.md)'s argument that it is the constrained case. That
did not survive the Pi 3 not being on hand.
[ADR-0036](adr/0036-raspberry-pi-4-is-the-edge-target.md) makes the Pi 4 the target and claims
no floor below it, because a platform nobody tests on is a claim nothing checks. The half of
the argument worth keeping, that the software stays honest about memory and I/O, is now kept
by streaming rather than by a small board.

- The Pi 4 is the **target**, and the only board tested.
- Whether CM4/CM5 becomes the **shipped platform** is still open, as
  [§ 10](#10-what-this-asks-someone-to-decide) decision 4.

### Radio: the module is a replaceable subassembly

RFID modules go end-of-life on a cycle shorter than a timing product's useful life. The
software already has the answer — `ReaderProvider` exists so a new protocol is a new adapter
rather than a rewrite ([architecture.md § 2](architecture.md#dependency-rules)).

**Build the same seam into the hardware.** Put the module on a socketed or castellated
daughtercard with a defined UART/USB and RF interface, so an EOL becomes a new subassembly
plus a new adapter crate — not a new product and a new certification campaign.

That symmetry is also the clearest sentence available for a proposal: the architecture's
central claim is that the timing engine cannot tell one reader from another, and the hardware
is built to the same claim.

## 6. Phase 2 — certification and manufacture

Separately funded, and the phase hardware proposals most often underestimate by an order of
magnitude. Budget it explicitly even as a range: a funder who discovers it later will assume
it was not known.

### What a modular grant does and does not cover

| Obligation | Who carries it | In practice |
|---|---|---|
| Intentional radiator (Part 15.247) | **Inherited** | Covered by the module's grant — only while using an antenna on its approved list, at or below approved gain, with no changes to the RF section |
| Antenna restriction | **Conditional** | Exceeding listed gain voids the grant. 6 dBi against a 6 dBi listing is fine; an 8 dBi "upgrade" is a new certification |
| Unintentional radiator (Part 15B) | **Yours** | The CM4, eMMC, Ethernet, SSD, and switching supplies are a digital device. Lab testing on the finished product. The largest single line |
| RF exposure (MPE) | **Yours** | An evaluation and a documented minimum separation distance |
| Labeling | **Yours** | "Contains FCC ID: (module)" plus your own Part 15B statement |
| ISED (Canada), CE-RED (EU) | **Yours** | Separate. The EU is 865–868 MHz — a different SKU and a different antenna. Ship US-only first |

Estimated envelope: **$16k–50k**, wide on purpose. Phase 1's quotes and the pre-scan narrow
it before anyone is asked to fund it.

### The GPL constraint, which ADR-0007 already recorded

[ADR-0007](adr/0007-license-selection.md) names the product implication in its consequences:
*"anyone shipping preloaded SplitForge SD cards or turnkey Pi units must let the recipient
install modified versions."*

Selling hardware is fine. **Locking it is not** — no signed-firmware-only boot, no measure
preventing a buyer from installing their own build. Plan the business model around assembled
units, service, support, calibration, and tags, not around software lock-in, because the
license forecloses that route deliberately.

For this market that is closer to an asset than a cost. "You can read the code that produced
this result" is the product's actual argument, and it is the same argument
[ADR-0005](adr/0005-raw-read-append-only-journal.md) makes about the data.

## 7. Software plan

Seven steps. The first three need no hardware and should be underway before the order ships —
the same argument [ADR-0004](adr/0004-llrp-first-reader-adapter.md) makes for writing a
parser against captures.

### Step 0 — write the decisions down first *(no hardware)* — **done**

Done for the M7e-Pico, and done again when it changed:
[ADR-0035](adr/0035-the-first-module-is-the-m7e-hecto.md) replaced it with the M7E-HECTO and
[ADR-0036](adr/0036-raspberry-pi-4-is-the-edge-target.md) made the Pi 4 the target. The Hecto's
notes are [`docs/readers/thingmagic-m7e-hecto.md`](readers/thingmagic-m7e-hecto.md). The list
below is the first round, as it was done. Its § numbers are the Pico guide's; the Hecto guide
puts § 8.8.x at § 8.9.x
([finding 23](readers/vendor-documents.md#23-the-protocol-sections-are-the-pico-guides-one-section-later-in--8)).

- [x] [ADR-0024](adr/0024-serial-reader-adapter-before-llrp.md) — records that ADR-0004
      **stands** while the M7e-Pico becomes the first physical adapter, and names what it
      cannot exercise.
- [x] The same ADR covers the new dependency. `deny.toml`'s `[bans]` section says it outright:
      *"every crate on the read path is a crate that can lose a read. Additions to the read
      path warrant an ADR."*
- [x] Split [Q9](open-questions.md#q9-first-reader-model):
      [Q9a](open-questions.md#q9a-first-serial-module) closes as the M7e-Pico;
      [Q9b](open-questions.md#q9b-first-llrp-reader-model) stays open and keeps blocking M3b.
- [x] Amend [roadmap.md](roadmap.md) to [M3a](roadmap.md#milestone-3a--one-serial-reader) /
      [M3b](roadmap.md#milestone-3b--one-networked-llrp-reader), M3b's exit criteria
      unchanged.
- [x] Create [`docs/readers/thingmagic-m7e-pico.md`](readers/thingmagic-m7e-pico.md) — the
      per-model notes file the checklist requires — listing the module as **experimental —
      under evaluation** with its gaps named. It does not enter the support table.
- [x] **One thing this plan missed.** `hardware-support.md` listed *"Handheld/USB readers —
      different integration model, revisit after LLRP"* under **Deliberately unsupported**,
      which decisions 1 and 2 directly contradict. Amending that file was not in this step and
      had to be added to it — otherwise the document whose entire job is being the project's
      honest state would have contradicted the ADR beside it.

### Step 1 — the adapter crate *(no hardware)* — **done**

New crate at `crates/splitforge-thingmagic/`, permitted to depend on `splitforge-domain` and
`splitforge-reader` and nothing else — the boundary `splitforge-llrp` already declares.

Two files must be edited by hand, and both are gates rather than chores. The `ALLOWED` table
in `crates/splitforge-testkit/tests/dependency_rules.rs` is listed exhaustively precisely so
that *"adding a crate to the workspace forces a deliberate answer here"*, and the dependency
table in [architecture.md § 2](architecture.md#dependency-rules) is what that test enforces.

```toml
serialport = { version = "4", default-features = false }
```

**`default-features = false` is required, not stylistic.** The default set pulls in
`libudev`, which needs `libudev-dev` *for the target*; CI's cross gate installs only
`gcc-aarch64-linux-gnu`, so leaving defaults on breaks the Raspberry Pi build. MPL-2.0 is
already in `deny.toml`'s allow list, so the licence check passes unchanged.

### Step 2 — framing before semantics *(no hardware)* — **done**

Parse the ThingMagic serial framing — `0xFF` / length / opcode / payload / CRC-16 — as a pure
function over `&[u8]` returning `Result<Frame, FrameError>`, with no I/O near it. ADR-0004's
rules apply unchanged: bounded frame sizes, allocation limits, and parsing that returns errors
rather than panicking, because malformed input is expected rather than exceptional.

This is the highest-risk code in the project and it is fully testable before a module exists:
hand-built frames, truncated frames, bad CRCs, absurd length fields, and a frame claiming
64 KB of payload.

### Step 3 — the timestamp decision

The M7e reports a relative millisecond timestamp within a continuous-read session. It is
**not** microseconds since boot, so mapping it onto `ReaderTimestamp::Uptime` is a small
inaccuracy with a real downstream effect. Do it anyway, and record the anchor:

- Map to `ReaderTimestamp::Uptime { micros }`, and capture a session anchor
  `(received_at_utc, module_relative_us)` at every connect — exactly the anchoring
  [clock-and-time-discipline.md § 6](clock-and-time-discipline.md#6-llrp-timestamp-specifics)
  prescribes. **Anchor at every read command, not only at every connect:** user guide § 8.8.3
  makes the module's zero the moment the read command was issued, and says outright that reads
  from either side of one cannot be ordered against each other. One connect can contain several
  read commands, and each is a new epoch.
- Document the "since read-start, not since boot" semantic in the reader notes file. That
  sentence is the difference between an anchor someone can use later and a number nobody can
  interpret.

Through the existing `Ingest::normalize`, that produces `reader_timestamp = None`,
`timestamp_source = DeviceReceipt { ReaderUptimeOnly }`, and the Pi's receipt time as
authoritative. **The module's high-resolution timestamp is preserved as evidence and is not
authoritative** — the right conservative default for a first adapter, and revisitable at
derivation time without ever touching the journal
([§ 9](clock-and-time-discipline.md#9-correction-happens-at-derivation-never-in-the-journal)).

> **State this plainly in any proposal.** On Phase 0 hardware, timing accuracy is bounded by
> the Pi's receive-time jitter — USB serial latency plus scheduler jitter — not by the
> module's specifications. Measuring that jitter is a Phase 0 deliverable. Quoting the
> module's "300 tags/sec" as an accuracy figure would be quoting the wrong number entirely.

### Step 4 — give the edge service a read path

`apps/splitforge-edge/src/main.rs` currently serves health and, by its own module
documentation, does not write. M3a changes that. The ordering from
[architecture.md § 3](architecture.md#3-data-flow) is absolute and not negotiable under time
pressure:

```text
ReaderMessage
  -> Ingest::normalize
  -> sidecar append + fsync     <- completes first, always
  -> journal append              <- durable here, and only here
  -> notify engine
```

Two counters, deliberately different numbers: `reads_received` counts frames off the serial
port, `reads_persisted` counts journal appends that returned.
[Architecture § 4](architecture.md#what-survived-means) calls the gap between them a monitored
quantity, and it is also how read-count reconciliation gets measured.

Health gains reader connection state — which the edge module docs already identify as the only
thing that *can* report it, since it lives in the process and in no file.

**Three constraints on this path come from the user guide, and none of them were known when the
ordering above was written** ([vendor-documents.md](readers/vendor-documents.md#what-the-read-path-will-depend-on-quoted)):

- **There is no flow control** (§ 5.1.4.1), and the host *"must have the capability to receive up
  to 255 bytes of data at a time without overflowing."* The read loop cannot apply backpressure;
  if it stalls, bytes are lost in the kernel rather than queued by the module.
- **Streaming cannot be paused, and a broken cable cannot be detected** (§ 8.8.2). The module has
  no control lines, so it goes on streaming into a disconnected host, and the host cannot ask it
  to stop without stopping the reading. This is what puts
  [M3a's exit criterion in doubt](roadmap.md#milestone-3a--one-serial-reader).
- **The alternative is polling the tag buffer** (§ 8.8.1), a FIFO of roughly 52 96-bit EPCs. It
  bounds and counts the loss that streaming cannot, at a throughput cost nobody has measured.
  Which of the two the adapter uses is a decision this plan does not yet make.

### Step 5 — systemd and the device node — **done, but for the bridge's IDs in the rule**

**Done by [ADR-0034](adr/0034-the-service-opens-the-readers-port-and-nothing-else.md)**, and
observed under systemd 252 rather than on a Pi. Three things below turned out to be missing:

- **The driver has to be loaded before the service starts.** `DeviceAllow=char-ttyUSB` is
  looked up in `/proc/devices` at start, and `ttyUSB` is listed only once `usbserial` has
  loaded. Otherwise the filter allows nothing and systemd says so only at debug level.
  `deploy/splitforge.modules-load.conf` loads it at boot.
- **`ProtectClock=yes` does not close the device policy by itself**, so `DevicePolicy=closed`
  is load-bearing rather than belt-and-braces.
- **The service threw away the reason a port would not open.** It now logs it, and
  [deployment.md](deployment.md#connecting-the-reader) maps each message to its fix.

The rule below shipped as `deploy/99-splitforge-reader.rules` matching any USB serial adapter,
because the bridge had not been chosen. It has now: SparkFun's Hecto board carries a CH340C,
which the kernel's `ch341` driver lists as `1a86:7523`, so the `XXXX`s below are known and the
shipped rule can be narrowed to them. CH340-family bridges are not expected to carry a serial
number, so the rule cannot tell two identical boards apart
([the reader notes](readers/thingmagic-m7e-hecto.md#deployment-notes)).

> **`PrivateDevices=yes` in [`deploy/splitforge-edge.service`](../deploy/splitforge-edge.service)
> gives the service a private `/dev` containing only pseudo-devices. `/dev/ttyUSB0` is not in
> it.** As the unit stands, the service cannot see its reader at all. The roadmap anticipated
> widening `RestrictAddressFamilies` for LLRP; this is not written down anywhere.

The fix is narrower than it looks — grant the tty class and nothing else:

```ini
PrivateDevices=no
DevicePolicy=closed
DeviceAllow=char-ttyUSB rw
```

**The network directives stay as they are.** A serial adapter opens a file, not a socket, so
this phase widens no network surface whatsoever. The unit already allows IPv4 to this device
only, for `chronyc` ([ADR-0032](adr/0032-the-service-speaks-ip-to-this-device-only.md)). It is
M3b's LLRP reader that will have to widen `IPAddressAllow` deliberately and fail `unit_file.rs`
until it does. Of the two adapters, the serial one is the *less* privileged.

`ttyUSB0` renumbers on re-enumeration, so add `deploy/99-splitforge-reader.rules` for a stable
name and the right group, which also means the service account never needs adding to
`dialout`:

```text
SUBSYSTEM=="tty", ATTRS{idVendor}=="XXXX", ATTRS{idProduct}=="XXXX", \
  SYMLINK+="splitforge-reader", GROUP="splitforge", MODE="0660"
```

`apps/splitforge-edge/tests/unit_file.rs` needs the new expectations. Keep its discipline —
every assertion compares the unit against a fact taken from somewhere else — by asserting the
udev rule's group against `deploy/splitforge.sysusers.conf` rather than a string restated in
the test.

### Step 6 — clock state without reaching for `unsafe` — **done**

[Milestone 5](roadmap.md#milestone-5--field-reliability) recorded that determining
`DeviceClockState` *"needs syscalls this workspace's `unsafe_code = deny` rules out reaching
for directly."* There is a way around it, and the unit file already points at it: *"the
service reads the clock and never sets it. Clock discipline is the system's NTP or GPS
daemon's job."*

So read the daemon rather than the syscall. `chronyc -c tracking` emits parseable CSV —
reference ID, stratum, leap status, offset — and `ProtectClock=yes` stays untouched, because
reading is all that happens. Parsing and classification are pure and live in
`splitforge-domain`; running the process lives in the CLI. `splitforge doctor` reports the
result unconditionally, and warns without blocking anything.

**This step's original mapping was wrong, and building it is what found out.** It claimed
*"RTC-set-only gives `Rtc`"*. It does not — **`Rtc` and `Manual` are not reachable from
tracking output at all**:

| State | From `chronyc -c tracking`? |
|---|---|
| `GpsLocked` | **Yes** — a local reference with a GPS/PPS refid |
| `NtpSynced` | **Yes** — synchronized to any other source |
| `Unsynced` | **Yes** — leap status says so, or there is no reference |
| `Rtc` | **No** |
| `Manual` | **No** |

A Pi whose clock was set from a DS3231 at boot and has reached no source since reports *"Not
synchronised"*, exactly like a Pi that booted with no clock at all — because from chrony's
point of view they *are* the same situation. Telling them apart means knowing whether an RTC
device exists and was read at boot, which is a different question asked of a different place.

So both report `Unsynced`, which is the safe direction: `is_trustworthy` is false for
`Unsynced` and true for `Rtc`, so the error is toward warning about a clock that was fine
rather than staying quiet about one that was not.

**Phase 0 therefore reports `Unsynced`, not `Rtc`** — even with the DS3231 fitted and
working. Phase 1's GPS/PPS is what makes `GpsLocked` reachable at all, and it is also what
would make a Phase 0 device stop reporting `Unsynced`.

What stays gated is making any of this **blocking**. *Which* states should refuse a race
start is [Q11](open-questions.md#q11-clock-error-budget-enforcement), which has no answer.

```text
# Phase 0 - /boot/firmware/config.txt
dtoverlay=i2c-rtc,ds3231          # then disable fake-hwclock

# Phase 1 - adds PPS
dtoverlay=pps-gpio,gpiopin=18     # gpsd + chrony:
                                  #   refclock PPS /dev/pps0 lock NMEA
                                  #   refclock SHM 0
```

### Step 7 — validate against the checklist that already exists

Do not invent a test plan. [hardware-support.md](hardware-support.md#what-supported-requires)
has one, and § 2 above scores which boxes this phase can tick. Run it in this order, because
each stage's failures are cheaper to diagnose than the next's:

1. **Static range map** — detection rate, RSSI, and first-detection position at 0.5 m
   increments across the lane.
2. **Motion trials** — 100+ walking and running passes across every lane position and
   plausible bib orientation.
3. **Crowding** — pairs and small packs, tags on real bib material, on people in normal
   clothing.
4. **Boundary** — walk near but not through the chute; find where off-course reads begin.
5. **Timed trials** — film crossings against a visible time reference and compare against the
   credited read. This calibrates the `first-above-rssi` threshold instead of guessing it.
6. **Failure trials** — restart the service, unplug the module mid-read, cut power, and
   reconcile module-reported counts against the journal.
7. **Full-duration rehearsal** — the whole anticipated event length, with realistic power and
   repeated passes.

## 8. What $500 buys that no reader purchase could

The strongest argument for this spend is not that the module is cheap.

### It proves the `ReaderProvider` port is real

The architecture's load-bearing claim is that the timing engine cannot tell one reader from
another — it is why `engine` must never depend on `llrp`, and it is the rule
`dependency_rules.rs` exists to protect.

Today that claim rests on a single implementation: the simulator, written by the same people
who wrote the port, to fit the port. **An LLRP reader would not have tested it either**,
because LLRP is what the port was designed around. A serial module with no reader clock, one
antenna, and completely different framing is the first genuine adversary the abstraction has
faced. If `ReaderProvider` survives it unchanged, that is evidence. If it needs changes,
finding out now costs a crate; finding out at M6 costs a rewrite.

**Answered, and it needed a change.** The port survived everything this section expected it
to be tested by: different framing, no reader clock, and one antenna all fit behind it
without moving it. What it did not survive was the thing the module *cannot* do — announce
its own failure. On a link with no flow control the adapter is the only thing that knows the
port died, and `start()` returned a channel of reads, so it had nowhere to say so; a service
downstream could not tell a dead reader from a checkpoint nobody was crossing. `start()` now
returns a channel of `ReaderEvent`, which carries the connection lifecycle beside the reads.

**That is the prediction working rather than failing**, and it is the same shape as the CRC:
the abstraction was written around LLRP, LLRP runs over TCP, and TCP made liveness somebody
else's problem so completely that the port never asked for it. The gap was found by the
adapter that has no such transport — the crate this section argued for — rather than at M6 by
the reader that would have inherited the assumption. The change is small and it is shared
with M3b, which needs the same signal for a socket that closes.

### It retires the Pi-side reliability questions that need real reads

[Milestone 5](roadmap.md#milestone-5--field-reliability) names four things it could not retire
without hardware, none of which require LLRP — only a real stream of real reads:

- Whether the SD card honors `fsync` at all.
- What the second sync per reader report costs on real flash.
- What a full day's journal weighs — the 256 MiB default is *"a judgement against the 5K
  fixture, not a measurement."*
- What happens to a write in flight when the power goes.

Phase 0 answers all four. That is a milestone's worth of retired risk from a $500 order, and
it is what makes the $2,000 ask legible rather than speculative.

## 9. Risks

| Risk | Absorbed by |
|---|---|
| The board arrives needing parts or work nobody planned | The pre-order questions in § 3, asked of both boards. The one piece of work they found, moving the RF resistor to the U.FL, is a bench step listed there |
| The module overheats in a closed enclosure and turns its RF off | Heatsinks and a fan in the order. The module reports overheating as `0x504`, which the adapter currently counts as a decode fault rather than naming ([ADR-0035](adr/0035-the-first-module-is-the-m7e-hecto.md#what-the-board-changes)). Lowering power or duty cycle is the fallback |
| Serial protocol documentation is gated behind vendor registration | It was, and worse: the vendor's own links now 404. The MercuryAPI sources and SparkFun's library, which names the Hecto, are archived with hashes in [vendor-documents.md](readers/vendor-documents.md). The parser is anchored on a capture either way |
| Read range insufficient for a real lane at +27 dBm or below | Phase 0 is scoped as a narrow supervised lane. Range is a measured output of step 7, not an assumption. If short, that is a Phase 1 choice between a higher-power module and a higher-gain approved antenna |
| Volume module pricing comes back too high to be viable | Discovered in Phase 1 for the price of an email, before any PCB or certification spend. This is why the quote request is a Phase 1 deliverable rather than a Phase 2 one |
| Part 15B testing fails and forces a respin | A ~$1k pre-scan before the full test, budgeted in § 6 for exactly this reason |
| No LLRP reader is ever sourced | Q9b stays open and M3b stays gated — precisely as M3 is today. Nothing regresses, and the product path does not depend on it |
| $500 does not cover core plus tax, or shipping | Stated trim order: the antenna first. Never the RTC, never the tags |

## 10. What this asks someone to decide

Each is a decision, not a guess, and belongs in
[open-questions.md](open-questions.md) until it has an owner.

| # | Decision | Needs | Status |
|---|---|---|---|
| 1 | Does M3 split into M3a / M3b as § 2 proposes? | Roadmap amendment | **Decided** — [ADR-0024](adr/0024-serial-reader-adapter-before-llrp.md); [roadmap](roadmap.md#milestone-3--one-physical-reader) amended |
| 2 | Is the M7e-Pico the first physical adapter, with ADR-0004 standing? | New ADR | **Decided, then changed** — ADR-0024 chose the Pico; [ADR-0035](adr/0035-the-first-module-is-the-m7e-hecto.md) replaced it with the M7E Hecto on SparkFun's USB board. ADR-0004 stands unamended, [Q9a](open-questions.md#q9a-first-serial-module) closed, [Q9b](open-questions.md#q9b-first-llrp-reader-model) still gates M3b |
| 3 | Does `serialport` join the read path? | Covered by the same ADR, per `deny.toml` `[bans]` | **Decided** — ADR-0024, with `default-features = false` mandatory for the Pi cross-build |
| 4 | Does CM4/CM5 become the shipped platform with Pi 3 as the support floor? | New ADR, plus one line in ADR-0002 | **Half decided** — [ADR-0036](adr/0036-raspberry-pi-4-is-the-edge-target.md) makes the Pi 4 the target and drops the Pi 3 as a floor. Whether a Compute Module ships is still open |
| 5 | Is the product's radio a replaceable subassembly, or soldered down? | New ADR — it constrains the carrier design | **Open** |
| 6 | Is [Q10](open-questions.md#q10-gps-pps-time-reference) answered as "required for published results"? | Q10 has been open since M0, and Phase 1 is when it becomes answerable | **Open** |

Decisions 4 and 5 are deliberately *not* bundled into ADR-0024. They constrain a product this
project has not committed to building, on a timescale where nothing forces the choice yet —
and ADR-0024 is expensive enough to reverse already. Nothing in M3a depends on either.

## 11. What this plan does not claim

- **That the M7E-HECTO is a supported reader.** It is not, and it will not be until step 7 is
  finished and written up. Until then it is *experimental — under evaluation*, with the two
  criteria it cannot close named in the notes file.
- **That Phase 0 hardware can time a real event.** It is a narrow, supervised, single-lane
  validation platform with manual backup. A wide finish chute, pack finishes, and unsupervised
  operation are all outside what one antenna at +27 dBm should be asked to do.
- **That the Phase 2 numbers are quotes.** They are ranges. Phase 1 exists to replace them.
- **That any of this closes M3b.** It does not. M3b needs an LLRP reader, and M5 needs M3b.
