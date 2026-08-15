# ADR-0002: Raspberry Pi 3 on 64-bit Linux as the first edge target

- **Status:** Accepted
- **Date:** 2026-08-14

## Context

SplitForge runs at a checkpoint, which means it runs on hardware someone carries to a
field, powers from a battery, and leaves outdoors for several hours. The platform choice
determines the performance envelope, the deployment story, and how easy it is for an
organizer to obtain and replace the machine.

The Raspberry Pi 3 has a 64-bit ARM Cortex-A53, 1 GB RAM, four USB 2.0 ports, and
Ethernet routed over USB 2.0. It is cheap, widely available, replaceable at short notice,
and familiar to the kind of person who volunteers to run timing.

## Decision

Target the **Raspberry Pi 3 Model B/B+ running 64-bit Raspberry Pi OS**, building for
`aarch64-unknown-linux-gnu`.

Deploy as a single systemd service (`splitforge-edge.service`) with `Restart=always`.

Support both a cross-compile workflow for fast iteration and native validation on real
hardware before any release.

## Consequences

### What this makes easy

- Cheap, replaceable hardware — a spare Pi in the kit bag is a viable recovery plan
- Standard Linux deployment: systemd, journald, apt, SSH
- 64-bit userspace avoids 32-bit time and pointer awkwardness
- One well-known target to optimize and test against

### What this makes hard

- 1 GB RAM caps in-memory working sets; large events must stream, not load
- Ethernet shares the USB 2.0 bus, so reader traffic and USB storage contend
- **No battery-backed RTC** — a significant problem for a timer, addressed in
  [clock and time discipline](../clock-and-time-discipline.md)
- SD card wear is a genuine failure mode under sustained journal writes
- Cross-compilation adds CI complexity and a linker dependency

### What we accept

Constraining the first deployment to one reader, one checkpoint, one database. The Pi 3 is
comfortably sufficient for that and uncomfortable beyond it. Scaling to multiple readers is
deliberately deferred rather than designed for speculatively.

We also accept that cross-compilation proves only that the code builds. Reader behavior,
storage performance, and power-loss recovery are validated on hardware or not at all.

## Alternatives considered

| Alternative | Why not |
|---|---|
| Raspberry Pi 4/5 | Better hardware, and the Pi 5 even has an RTC. But the Pi 3 is the constrained case — if it works there, it works everywhere. Nothing prevents running on a 4 or 5 |
| x86 mini PC | More capable and more expensive; less familiar to volunteers; worse power story on battery |
| Pi Zero / Pi 2 | 32-bit and underpowered for sustained journal writes |
| Microcontroller (ESP32 etc.) | No filesystem story that supports a transactional journal. The durability requirement rules it out |

## References

- [hardware-support.md](../hardware-support.md)
- [clock-and-time-discipline.md](../clock-and-time-discipline.md)
