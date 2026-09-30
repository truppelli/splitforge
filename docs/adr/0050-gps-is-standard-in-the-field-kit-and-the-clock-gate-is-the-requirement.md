# ADR-0050: GPS is standard in the field kit, and the clock gate is the requirement

- **Status:** Accepted
- **Date:** 2026-09-29
- **Resolves:** [Q10](../open-questions.md#q10-gps-pps-time-reference)
- **Builds on:** [ADR-0048](0048-a-bad-clock-blocks-the-start-and-flags-the-result.md)

## Context

[Q10](../open-questions.md#q10-gps-pps-time-reference) asked whether a GPS receiver with a
pulse-per-second output is **required** hardware or a strong recommendation. It leaned toward
*required for any event where results are published, enforced by the pre-race check*.
[Hardware plan § 10](../hardware-plan.md#10-what-this-asks-someone-to-decide) listed it as a
decision for the project's owner, because the answer sets the field kit's parts list.

When Q10 was written, nothing checked the clock before a race. Now something does.
[ADR-0048](0048-a-bad-clock-blocks-the-start-and-flags-the-result.md) makes `race start`
refuse when `chronyc` reports the clock untrustworthy, unless it is forced on the record. That
changes what "required" would add.

| Where the timer is | Time sources it can have | What the ADR-0048 gate does |
|---|---|---|
| A venue with no network | GPS, or nothing (a DS3231 alone reads `unsynced`, [hardware plan § 7](../hardware-plan.md#7-software-plan)) | Passes only with GPS. Without it, every start is forced |
| A venue with a network | GPS, or NTP over the network | Passes with either |

So the gate already makes GPS necessary exactly where nothing else can establish the clock.
Requiring it everywhere would add one case: refusing a start on a clock that NTP has already
disciplined.

That case is not where the budget is at risk. With the serial module
([ADR-0024](0024-serial-reader-adapter-before-llrp.md)), the Pi timestamps every read, so both
ends of an elapsed time come from one clock. Offset cancels and only drift counts
([clock discipline § 4](../clock-and-time-discipline.md#4-where-clock-error-actually-matters)).
A clock disciplined by NTP drifts by far less than the budget over any race.

Where GPS matters most is M3b, with networked readers that keep their own clocks. There the Pi
serves time to the readers so the two clock domains become one
([clock discipline § 5](../clock-and-time-discipline.md#5-getting-a-time-reference-without-the-internet)).
That design needs the Pi to have a good reference and the reader to accept a LAN time server.
It does not need that reference to be GPS rather than NTP.

## Decision

**1. GPS+PPS is standard equipment in the field kit, and is not a software requirement.** The
Phase 1 field unit carries a GPS receiver with PPS, as
[clock discipline § 5](../clock-and-time-discipline.md#5-getting-a-time-reference-without-the-internet)
already recommends, with the DS3231 as holdover. A bench setup, or a site with a network, may
run without one.

**2. The requirement is on the clock, not the part.** What an event needs is a clock that
something trustworthy has established, and ADR-0048's gate already enforces that at the start.
There is no GPS-specific gate, no new flag for results timed under `ntp_synced`, and `doctor`
says nothing new.

**3. What stays open is a reader's.** Whether a reader can be pointed at a LAN time server is a
property of the reader model, not of this decision. It moves to
[Q9b](../open-questions.md#q9b-first-llrp-reader-model)'s selection criteria, which already list
a configurable NTP server.

## Consequences

### What this makes easy

- **No new refusal.** A device with a network and NTP starts without `--force`, so forcing stays
  rare enough to mean something ([ADR-0019](0019-pre-race-gates-block-but-can-be-overridden.md)).
- **A low barrier to entry.** Someone trying SplitForge on a bench, or at a club with Wi-Fi,
  needs no receiver.
- **The venue that most needs GPS already gets it.** Offline, GPS is the only way through the
  gate without forcing it on the record.

### What this makes hard

- **"Standard in the kit" is a hardware-plan commitment, not a check.** Nothing stops a field
  unit being built without GPS. At an offline venue the gate would then refuse every start, and
  the audit trail would show the forcing.
- **A network-synced event depends on the venue's network.** If it drops mid-race, chrony keeps
  the clock on its last correction and drift resumes slowly. The start gate has passed by then.
  `clock_samples` against a reference would show it, and is hardware work in M5.

### What we accept

That the ±0.1 s budget is guaranteed by GPS and only very likely under NTP, and that an
organizer running on NTP is not told the difference. Both states are trustworthy by
`DeviceClockState`, and the start record says which one the race began under.

## Alternatives considered

| Alternative | Why not |
|---|---|
| Required for published results: refuse a start under `ntp_synced` unless forced | Refuses a clock that is already disciplined well inside the budget, and makes forcing routine wherever there is a network |
| Required, as a new result flag on anything not `gps_locked` | Flags every result from a well-synced network setup. A flag that is always there tells nobody anything |
| Defer until Phase 1, as hardware plan § 10 proposed | Phase 1 measures the receiver. It does not change this argument, which rests on the gate and on where offset cancels |
| Recommended, with no gate at all | Was the other half of Q10, and ADR-0048 already overtook it: an unestablished clock refuses a start |

## References

- [Q10](../open-questions.md#q10-gps-pps-time-reference)
- [ADR-0048](0048-a-bad-clock-blocks-the-start-and-flags-the-result.md): a bad clock blocks the start and flags the result
- [Clock discipline § 4 and § 5](../clock-and-time-discipline.md#4-where-clock-error-actually-matters)
- [Hardware plan § 10](../hardware-plan.md#10-what-this-asks-someone-to-decide)
