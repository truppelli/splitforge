# Hardware Support

## Support matrix

| Reader vendor | Model | Protocol | Status | Validated by | Date |
|---|---|---|---|---|---|
| — | — | — | **No reader has been tested.** | — | — |

This table is empty on purpose, and it is the honest state of the project.

## Under evaluation

Devices being worked on that have **not** met the bar above. Nothing here is a support claim.

| Vendor | Model | Protocol | Status | Notes |
|---|---|---|---|---|
| JADAK / Novanta (ThingMagic) | M7e-Pico | ThingMagic serial | **experimental — under evaluation** | [notes](readers/thingmagic-m7e-pico.md) |

The M7e-Pico is the first physical adapter
([ADR-0024](adr/0024-serial-reader-adapter-before-llrp.md),
[Q9a](open-questions.md#q9a-first-serial-module)). **It can never reach the matrix above on
its own**, and that is worth stating here rather than only in its notes file: it has no
reader clock, so criterion 4 has nothing to measure; and it has one RF port, so criterion 5
has no second antenna to identify. Those are structural, not pending.

[hardware-plan.md](hardware-plan.md) is the budget behind that work. Its § 2 scores this
module against the nine criteria below without softening any of them; the decisions it asked
for are now recorded in ADR-0024, and the ones it asked for and did **not** get are still
listed in its § 10.

## The rule

> **SplitForge does not claim support for a reader vendor or model until that physical
> device has been tested against it.**

Protocol documentation is not evidence of support. LLRP is a standard, and readers
implement it with meaningful variation: optional parameters that are absent, timestamp
types that differ from the documented default, keepalive behavior that diverges under
load, reconnect semantics that only reveal themselves when a cable is pulled mid-report.

A support claim in a race timing project is a claim that an organizer can stake an event
on. It is not a compatibility hint.

## What "supported" requires

A reader moves into the table above only after all of the following have been observed on
the physical device:

- [ ] Connects, and reconnects after reader reboot, cable pull, and network interruption
- [ ] Delivers reads under sustained load without dropping the connection
- [ ] Timestamp type identified — `UTCTimestamp` or `Uptime` — and handled correctly
- [ ] Clock offset and skew measured over a multi-hour session
      ([clock discipline](clock-and-time-discipline.md))
- [ ] Antenna identity reported and correctly mapped
- [ ] RSSI reported, or its absence documented
- [ ] Malformed/truncated frames handled without process exit
- [ ] Read counts reconciled: reads the reader believes it sent == reads in the journal
- [ ] Behavior documented in a per-model notes file under `docs/readers/`

Anything less gets recorded as **"experimental — under evaluation,"** with the specific
gaps named.

## First hardware contract

Whatever the first reader turns out to be, the adapter must expose:

| Capability | Requirement |
|---|---|
| Addressing | Configurable hostname/IP and port |
| Identity | Stable reader identity, independent of DHCP lease |
| Health | Connection state, last-report time, error counters |
| Antennas | Per-antenna identity, mappable to checkpoints |
| Chip ID | EPC or equivalent chip identifier |
| Time | Reader timestamp when supplied, with type recorded |
| Signal | RSSI when supplied |
| Resilience | Configurable reconnect and backoff behavior |
| Diagnostics | Raw protocol capture mode behind an explicit flag |

**The first deliverable is not accurate finish timing.** It is: connect reliably, receive
records, persist every raw read, survive a restart, and prove no stored read was silently
lost.

## Compute platform

| Component | Target | Notes |
|---|---|---|
| Board | Raspberry Pi 3 Model B / B+ | 64-bit ARM Cortex-A53, 1 GB RAM |
| OS | 64-bit Raspberry Pi OS | |
| Rust target | `aarch64-unknown-linux-gnu` | [ADR-0002](adr/0002-raspberry-pi-target.md) |
| Storage | High-endurance microSD, A2 or better | USB SSD preferred for multi-day use — SD wear is a real failure mode |
| Network | **Wired Ethernet** | On the Pi 3, Ethernet is routed over USB 2.0 and shares bandwidth with USB. Adequate for reader traffic; race-day Wi-Fi is the larger risk |
| Power | External battery/UPS | Power loss during a write is expected, not exceptional |

1 GB RAM and USB-attached Ethernet are the constraints that keep the first deployment
modest: one reader, one checkpoint, one database.

## Timekeeping hardware

The Pi 3 has **no battery-backed real-time clock**, and offline-first operation means no
NTP. See [clock and time discipline](clock-and-time-discipline.md) for the full analysis.

| Component | Recommendation | Why |
|---|---|---|
| DS3231 RTC (I²C) | **Strongly recommended** | ±2 ppm, holds time across power-off. ~£5 removes the worst clock failure mode |
| GPS receiver with PPS | **Recommended** | Stratum-1 UTC with no infrastructure. Lets the Pi serve NTP to the readers, collapsing both clocks into one domain |

**OPEN:** whether these become required hardware or documented recommendations —
[Q10](open-questions.md#q10-gps-pps-time-reference).

## Deliberately unsupported

- Multiple simultaneous readers — not until one works for a full event
- Multiple checkpoints on separate devices — no distributed coordination in early milestones
- **Handheld** readers — a different operating model, not just a different transport
- Barcode-only timing — the ports exist for it; the work does not
- Pi Zero / Pi 1 / Pi 2 — 32-bit and underpowered for sustained journal writes

Two lines moved off this list, and both moves were decisions rather than drift:

- **USB/serial readers were listed here as "revisit after LLRP."**
  [ADR-0024](adr/0024-serial-reader-adapter-before-llrp.md) revisits it early, and the
  reasoning is in that ADR: LLRP is still the first *networked* protocol and M3b still holds
  all nine criteria, but a current serial module can be bought now where a networked one
  could not, and six of the nine can be retired with it. Being *worked on* is still not
  being *supported* — that is what the [Under evaluation](#under-evaluation) table above is
  for.
- **Manual entry shipped in Milestone 4**
  ([ADR-0023](adr/0023-manual-entries-are-derivation-inputs.md)), as an input to derivation
  rather than an override of its output. It is not a reader and never appears in these
  tables, but "manual-entry-only timing" is no longer an accurate description of what is
  excluded.
