# Hardware Plan

> Status: **partly adopted.** Decisions 1, 2, and 3 of
> [§ 10](#10-what-this-asks-someone-to-decide) are now recorded in
> [ADR-0024](adr/0024-serial-reader-adapter-before-llrp.md): M3 has split into M3a/M3b, the
> M7e-Pico is the first physical adapter, and `serialport` may join the read path.
> **Decisions 4, 5, and 6 are still open and bind nothing.** Everything below about the
> shipped compute platform, the radio subassembly, and Phase 2 remains a proposal.
>
> **No hardware has been ordered.** The four questions in
> [§ 3](#four-questions-to-answer-before-ordering--asked) have now been answered from
> documentation rather than from a device — and one of them found that the $345 board has no
> USB port at all, so it cannot be plugged into a Pi without a bridge that was not in the
> bill of materials.
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

The ThingMagic M7e-Pico is the opposite trade. It is a current part with an FCC modular
grant, a US distributor, and a documented serial protocol — what a manufactured product
needs. What it gives up is precisely the set of things Milestone 3 exists to test.

### Milestone 3's checklist, scored against both

The nine rows are copied from [hardware-support.md](hardware-support.md#what-supported-requires).
Nothing has been softened.

| Criterion | M7e-Pico | Fixed LLRP reader |
|---|---|---|
| Connects, and reconnects after reboot, cable pull, network interruption | **Partial** — USB re-enumeration, not a network path | Closes |
| Delivers reads under sustained load without dropping the connection | Closes | Closes |
| Timestamp type identified and handled correctly | **Recast** — no UTC clock exists; relative-ms maps to `Uptime` with a session anchor | Closes |
| Clock offset and skew measured over a multi-hour session | **Cannot** — there is no reader clock to measure against | Closes |
| Antenna identity reported and correctly mapped | **Cannot** — one RF port until a second module is fitted | Closes |
| RSSI reported, or its absence documented | Closes | Closes |
| Malformed/truncated frames handled without process exit | Closes | Closes |
| Read counts reconciled against the journal | Closes | Closes |
| Behavior documented under `docs/readers/` | Closes | Closes |

Six close, one is recast into something narrower and honest, and two cannot be closed at
all on one single-port module with no clock.

> **The antenna-identity row is now in question.** Answering
> [§ 3's question 1](#four-questions-to-answer-before-ordering--asked) turned up documentation
> that the *carrier board* carries four switched U.FL ports, which — if it holds — makes
> per-antenna identity reachable on one module and that row not a "cannot" at all. The table
> is left as scored until it is confirmed against something better than a distributor's forum;
> see [the reader notes](readers/thingmagic-m7e-pico.md#question-1-also-challenges-row-5-of-the-checklist-above).
> **The clock row is unaffected** — there is still no reader clock.

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

A finish line wants two antennas. The Pico has one port. The answer is **two modules**, each
presenting as its own `ReaderProvider` with its own reader and antenna identity — not one
module behind a splitter.

A splitter halves transmit power, complicates matching, and destroys the per-antenna
identity that [timing-model.md](timing-model.md) depends on and that
`splitforge reader map --antenna` already exposes to the operator. Two modules preserve it,
and cost one module.

**A switched carrier board is a third option, and it is not a splitter.** If the four U.FL
ports and their RF switch are real, one module can address two antennas *sequentially* with
full power into each and a logical antenna number on every read — which keeps the identity a
splitter destroys. What it does not keep is simultaneity: one antenna is live at a time, so a
runner crossing while the switch is on the other port is a read that never happens. For a
finish line that is a worse trade than it sounds, and it is the measurement that decides
between the two — not an argument to be settled on paper.

## 3. Phase 0 — bench validation ($500, now)

Buys the answer to one question: **does a serial UHF module, driven by SplitForge's own
adapter, put real reads in the journal and keep them there?**

Already on hand: a Raspberry Pi 3 Model B/B+, a weather-resistant box, power cables, and
Ethernet cables. The spreadsheet lists the ones a reviewer needs to see as in-kind rows so
the whole system is visible, and costs them at zero.

As submitted, with real vendors and quoted prices:

| Line | Vendor | |
|---|---|---|
| ThingMagic M7E-PICO-CB | DigiKey | $345.00 |
| UHF antenna, 6 dBi CP IP65 | L-com | $68.99 |
| Coax jumper | Walcott Radio | $15.49 |
| DS3231 RTC | Adafruit 3013 | $17.50 |
| Powered USB 2.0 hub | Adafruit 961 | $32.50 |
| Antenna stand and lane materials | Home Depot | $5.00 |
| **Subtotal** | | **$484.48** |
| Shipping allowance | | $15.52 |
| **Total against the cap** | | **$500.00** |

Three further lines are carried **in kind** and cost the budget nothing: the Raspberry Pi 3
and the microSD are already owned, and the EPC Gen2 tags are borrowed.

Two cautions the spreadsheet cannot express, both of which belong in the submission's notes
rather than being discovered later:

- **The shipping figure is a plug, not an estimate.** It is computed as `500 − subtotal`, so
  the total lands on exactly $500 by construction. The order spans four separate shippers —
  DigiKey, L-com, Walcott, and Adafruit — and four shipments are realistically $25–45, not
  $15.52. The subtotal needs roughly $20 of headroom, or one line has to move in kind.
- **Borrowed tags are a validation risk, not just a cost saving.** The read-zone
  characterization in step 7 depends on knowing the tags' band and inlay class, and on being
  able to mount them on real bib material. Confirm the borrowed stock is 902–928 MHz EPC Gen2
  before relying on it, and keep a small quantity of owned tags if the loan is short-term.
- **The coax line contradicts its own note.** RG8X from a CB-radio supplier terminates in
  PL-259/UHF connectors, which are neither constant-impedance nor appropriate at 915 MHz, and
  which match neither the antenna's SMA-female nor whatever the carrier board turns out to
  carry. The note on that row already says the connector is unconfirmed, which is the argument
  for keeping it as an unspent reserve rather than a specific cable. Order an SMA jumper in
  LMR-195 or RG316 once question 1 below is answered.

**If the carrier board lands above $345, the antenna is the line to trim.** A cheaper 8 dBi
panel works for a deliberately narrow lane at reduced power, and RSSI is being measured
empirically regardless. The RTC and the tags are not trimmable: the RTC removes a whole class
of silent wrongness for the price of a coin cell, and without tags the reader has nothing to
read.

### Four questions to answer before ordering — **asked**

Each could turn a $345 order into a box that cannot be used on arrival. All four have now been
put to the documentation; the answers, their sources, and how far each can be trusted are in
[the reader notes](readers/thingmagic-m7e-pico.md#the-four-pre-order-questions--answered-from-documentation).
Two of them move money, and one moves it in a direction this plan did not anticipate.

1. **RF connector: U.FL (I-PEX compatible)**, four of them, with switching on the board.
   The coax reserve becomes a U.FL-to-antenna pigtail in LMR-195 or RG316 — **the RG8X/PL-259
   line was wrong**, as the caution above already suspected in writing.
2. **No power supply with the bare carrier board.** The DEVKIT bundles one at $741.40; the
   `M7E-PICO-CB` at $345.00 does not. The module wants 3.3–5.5 VDC and under 2.5 W at +24 dBm,
   which the powered USB hub already in the BOM can supply — so this costs a cable, not $12.
3. **There is no USB connector, because there is no USB.** The module's only control interface
   is `UART; 3.3V logic levels`, and the carrier board brings it out on a 15-pin Molex
   532611571. **This is the bare-header case**, and it is the one line that was called out as
   "a second part nobody budgeted" — correctly. Add a USB-UART bridge and a Molex 1.25 mm
   cable, ~$15 together.
4. **Single SKU for global use**, pre-configured for FCC (NA, SA) 902–928 MHz among seven other
   regions. So the region is *selectable*, not factory-locked — and the adapter must set it
   explicitly at startup rather than assume it. **Still open:** whether the selection survives
   a power cycle.

**Net effect on the $500 cap: roughly neutral, and the shipping plug still is not.** Question 2
saves the $12 that was pencilled in; question 3 spends about $15. The coax line stays the same
size and changes what it buys. None of that touches the real problem the caution above already
names — four shippers at $25–45 against a $15.52 plug.

**One answer reaches past the budget.** If the carrier board really does carry four switched
U.FL ports, then row 5 of the support checklist — per-antenna identity — is not the structural
impossibility [§ 2](#milestone-3s-checklist-scored-against-both) scores it as, and this phase
could close seven of nine rather than six. That is deliberately not rewritten anywhere yet: the
source is a distributor's forum rather than a datasheet, and only one antenna is live at a time,
so two checkpoints on one module time-share the radio. See
[the reader notes](readers/thingmagic-m7e-pico.md#question-1-also-challenges-row-5-of-the-checklist-above).

**Before ordering, archive the user guide.** `jadaktech.com`'s documentation links now redirect
to `novanta.com` and the PDFs 404. The frame codec in `crates/splitforge-thingmagic/` rests on
that document, and it is no longer where it was found.

## 4. Phase 1 — field unit, and finding the real BOM ($2,000)

Contingent on Phase 0. Two jobs: build something that survives an actual race day, and
discover what the product costs to build — which is not what Phase 0 cost.

> **$345 is a developer price, not a BOM price.** The carrier board is a one-off sold in
> ones. The bare module in production quantities is a fraction of it, and quoting the
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

### Compute: Compute Module for the product, Pi 3 stays the support floor

The Pi 3 Model B+ is a development board: no availability commitment suited to a product, no
eMMC, no RTC, and Ethernet sharing a USB 2.0 bus. The Compute Module 4 or 5 is the part
Raspberry Pi sells for embedding — guaranteed availability into the 2030s, onboard eMMC, and
a carrier board designed once.

**This does not overturn [ADR-0002](adr/0002-raspberry-pi-target.md), and the record should
say so.** ADR-0002's argument is that the Pi 3 is the *constrained* case — "if it works
there, it works everywhere." That stays true and stays valuable: it keeps the software honest
about memory and I/O, and keeps the barrier to entry at whatever Pi a volunteer already owns.

- Pi 3 remains the **minimum supported platform**.
- CM4/CM5 becomes the **shipped platform**.

One line in ADR-0002's consequences, and a new ADR for the product target.

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
  prescribes.
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

### Step 5 — systemd and the device node

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

**`RestrictAddressFamilies=AF_UNIX` stays.** A serial adapter opens a file, not a socket, so
this phase widens no network surface whatsoever. It is M3b's LLRP reader that will have to add
`AF_INET` deliberately and fail `unit_file.rs` until it does. Of the two adapters, the serial
one is the *less* privileged — worth recording in the ADR.

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
| Carrier board arrives without a power supply, or with an unexpected RF connector | The four pre-order questions in § 3, plus the $25 connector reserve held back rather than pre-spent |
| Serial protocol documentation is gated behind vendor registration | The Mercury API SDK is publicly downloadable, and SparkFun's M6E-Nano library documents the same protocol family's opcode set. The parser is written against captures either way |
| Read range insufficient for a real lane at 24 dBm or below | Phase 0 is scoped as a narrow supervised lane. Range is a measured output of step 7, not an assumption. If short, that is a Phase 1 choice between a higher-power module and a higher-gain approved antenna |
| Volume module pricing comes back too high to be viable | Discovered in Phase 1 for the price of an email, before any PCB or certification spend. This is why the quote request is a Phase 1 deliverable rather than a Phase 2 one |
| Part 15B testing fails and forces a respin | A ~$1k pre-scan before the full test, budgeted in § 6 for exactly this reason |
| No LLRP reader is ever sourced | Q9b stays open and M3b stays gated — precisely as M3 is today. Nothing regresses, and the product path does not depend on it |
| $500 does not cover core plus tax | Stated trim order: the antenna first, then the coax reserve. Never the RTC, never the tags |

## 10. What this asks someone to decide

Each is a decision, not a guess, and belongs in
[open-questions.md](open-questions.md) until it has an owner.

| # | Decision | Needs | Status |
|---|---|---|---|
| 1 | Does M3 split into M3a / M3b as § 2 proposes? | Roadmap amendment | **Decided** — [ADR-0024](adr/0024-serial-reader-adapter-before-llrp.md); [roadmap](roadmap.md#milestone-3--one-physical-reader) amended |
| 2 | Is the M7e-Pico the first physical adapter, with ADR-0004 standing? | New ADR | **Decided** — ADR-0024; ADR-0004 stands unamended, [Q9a](open-questions.md#q9a-first-serial-module) closed, [Q9b](open-questions.md#q9b-first-llrp-reader-model) still gates M3b |
| 3 | Does `serialport` join the read path? | Covered by the same ADR, per `deny.toml` `[bans]` | **Decided** — ADR-0024, with `default-features = false` mandatory for the Pi cross-build |
| 4 | Does CM4/CM5 become the shipped platform with Pi 3 as the support floor? | New ADR, plus one line in ADR-0002 | **Open** |
| 5 | Is the product's radio a replaceable subassembly, or soldered down? | New ADR — it constrains the carrier design | **Open** |
| 6 | Is [Q10](open-questions.md#q10-gps-pps-time-reference) answered as "required for published results"? | Q10 has been open since M0, and Phase 1 is when it becomes answerable | **Open** |

Decisions 4 and 5 are deliberately *not* bundled into ADR-0024. They constrain a product this
project has not committed to building, on a timescale where nothing forces the choice yet —
and ADR-0024 is expensive enough to reverse already. Nothing in M3a depends on either.

## 11. What this plan does not claim

- **That the M7e-Pico is a supported reader.** It is not, and it will not be until step 7 is
  finished and written up. Until then it is *experimental — under evaluation*, with the two
  criteria it cannot close named in the notes file.
- **That Phase 0 hardware can time a real event.** It is a narrow, supervised, single-lane
  validation platform with manual backup. A wide finish chute, pack finishes, and unsupervised
  operation are all outside what one antenna at 24 dBm should be asked to do.
- **That the Phase 2 numbers are quotes.** They are ranges. Phase 1 exists to replace them.
- **That any of this closes M3b.** It does not. M3b needs an LLRP reader, and M5 needs M3b.
