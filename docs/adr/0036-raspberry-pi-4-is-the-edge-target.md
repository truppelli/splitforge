# ADR-0036: The Raspberry Pi 4 is the edge target

- **Status:** Accepted
- **Date:** 2026-09-24
- **Supersedes:** [ADR-0002](0002-raspberry-pi-target.md)'s choice of board. Its
  64-bit Linux target, `aarch64-unknown-linux-gnu`, and single systemd service stand

## Context

[ADR-0002](0002-raspberry-pi-target.md) chose the Raspberry Pi 3 Model B/B+ because it is the
constrained case: *"if it works there, it works everywhere."* Every document since has assumed
a Pi 3 was on hand. The Phase 0 budget carried one in kind, and
[hardware-plan.md](../hardware-plan.md#compute-compute-module-for-the-product-the-pi-4-until-then)
kept it as the minimum supported platform for a future product.

There is no Pi 3 on hand. The Phase 0 order now buys a **Raspberry Pi 4 Model B, 2 GB**
([`Materials-and-Cost-Table.xlsx`](../Materials-and-Cost-Table.xlsx), commit `1a87ed8`), and it
will be the only board SplitForge runs on.

A platform claim is worth what its testing is worth. This project refuses to claim reader
support it has not observed ([hardware-support.md](../hardware-support.md)), and the same
applies to computers. A Pi 3 that nobody owns cannot be tested, so keeping it as the target
would make it a claim that nothing checks.

## Decision

**The Raspberry Pi 4 Model B running 64-bit Raspberry Pi OS is the edge target.** The first
board is the 2 GB model.

**The Pi 3 is no longer a supported or minimum platform.** Nothing stops SplitForge from
running on one, since it is the same `aarch64` build. But the project makes no claim about it
until somebody runs it there.

**Unchanged from ADR-0002:** 64-bit Linux, the `aarch64-unknown-linux-gnu` cross-build and its
CI gate, a single `splitforge-edge.service` with `Restart=always`, and the rule that reader
behavior, storage performance and power-loss recovery are validated on hardware or not at all.

**This answers half of [hardware-plan.md](../hardware-plan.md#10-what-this-asks-someone-to-decide)
decision 4.** The Pi 3 is not the support floor. Whether a Compute Module becomes the product's
shipped platform stays open.

## Consequences

### What this makes easy

- **Ethernet is off the USB bus.** On the Pi 3, Ethernet shared USB 2.0 with the reader. On
  the Pi 4, Gigabit Ethernet has its own controller, so reader traffic and network traffic do
  not contend for the same bus.
- **No powered hub.** With its 3 A supply, the Pi 4 gives its USB ports 1.2 A in total. The
  M7E Hecto draws over 700 mA at +27 dBm
  ([ADR-0035](0035-the-first-module-is-the-m7e-hecto.md)), which fits.
- **The claim and the testing agree.** Every measurement M3a takes, including write latency,
  fsync on real flash, receive-time jitter and memory, is taken on the platform the project
  claims.

### What this makes hard

- **The constraint that kept the software lean is gone.** ADR-0002 valued the Pi 3's 1 GB
  because it forced large events to stream rather than load. 2 GB is more room, not unlimited
  room. The open hygiene item about recovery, `doctor` and the bundle loading the whole journal
  into memory ([roadmap](../roadmap.md#hygiene)) is not closed by a bigger board. It is still an
  out-of-memory crash loop on a large enough journal.
- **Heat.** The Pi 4 throttles when hot, and it shares a closed enclosure with a radio that
  turns its RF off when it overheats
  ([ADR-0035](0035-the-first-module-is-the-m7e-hecto.md)). A throttled Pi adds receive-time jitter, which is what bounds
  accuracy on this hardware ([ADR-0024](0024-serial-reader-adapter-before-llrp.md)). The
  heatsinks and fan in the order are the mitigation. Whether they are enough is something to
  measure.
- **The text says Pi 3.** `README.md`, `hardware-support.md`, `architecture.md`,
  `timing-model.md`, `clock-and-time-discipline.md`, `CONTRIBUTING.md`, the hardware plan, and
  doc comments in `splitforge-edge` and `splitforge-domain` all name it. They are corrected in
  a following change.

### What we accept

**Still no battery-backed real-time clock.** The Pi 4 has none. The Pi 5 is the first model
with one. Everything [clock and time discipline](../clock-and-time-discipline.md) says about a
Pi 3 booting to its last-shutdown time is true of a Pi 4, and the DS3231 stays in the order.

**A board that costs more and draws more** than the one ADR-0002 described. It needs a 3 A
supply, where a Pi 3 managed on 2.5 A. That matters on battery at a checkpoint, and the field
guide still to be written in [Milestone 5](../roadmap.md#milestone-5--field-reliability) has to
account for it.

**SD card wear is unchanged.** The Pi 4 boots from the same microSD, and the same journal
writes land on it.

## Alternatives considered

| Alternative | Why not |
|---|---|
| Keep the Pi 3 as the target and test on a Pi 4 | That claims a platform nobody tests on. The Pi 3 would have become the kind of support claim [hardware-support.md](../hardware-support.md) exists to prevent |
| Pi 4 as the tested board, Pi 3 as a stated floor | The same claim, labeled as a minimum. A floor nobody measures is not one |
| Buy a Pi 3 | It is the older part and would not have been cheaper for the same capability. It would also have spent the budget to preserve a constraint rather than a capability |
| Raspberry Pi 5 | It has an RTC, which would matter. It also costs more, needs a 5 A supply, and runs hotter, and the $500 has no room for it once the radio is paid for |
| Compute Module 4 or 5 | The right question for a product ([hardware-plan.md](../hardware-plan.md#10-what-this-asks-someone-to-decide) decision 4), and a carrier board this project has not designed. Too early for a bench rig |

## References

- [ADR-0002](0002-raspberry-pi-target.md): superseded for its choice of board
- [ADR-0035](0035-the-first-module-is-the-m7e-hecto.md): the module this board powers
- [hardware-plan.md](../hardware-plan.md): § 3, the order, and § 5, the product platform
- [clock-and-time-discipline.md](../clock-and-time-discipline.md): why the RTC stays
- [`Materials-and-Cost-Table.xlsx`](../Materials-and-Cost-Table.xlsx): the order
