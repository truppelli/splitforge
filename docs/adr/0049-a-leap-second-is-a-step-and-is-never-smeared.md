# ADR-0049: A leap second is a step, and is never smeared

- **Status:** Accepted
- **Date:** 2026-09-29
- **Resolves:** [Q12](../open-questions.md#q12-leap-second-handling)
- **Builds on:** [ADR-0048](0048-a-bad-clock-blocks-the-start-and-flags-the-result.md)

## Context

[Q12](../open-questions.md#q12-leap-second-handling) asked what SplitForge does about a leap
second: rely on the time source smearing it, detect and record it, or ignore it. It worked out
what smearing costs. A 24-hour smear runs the clock about 11.6 ppm off, which uses up the
±0.1 s budget after **2.4 hours** of elapsed time. That is fine for a 5K and not for a marathon
or an ultra.

There are three ways a leap second can reach a Pi running chrony:

| How | What the device's clock does | What SplitForge sees today |
|---|---|---|
| chrony's default, `leapsecmode system` | The kernel repeats 23:59:59 (inserted) or skips it (deleted): a one-second step at midnight UTC | The service's step detector compares wall and monotonic time every 10 s and records anything over 250 ms. A leap is recorded as a step of about ±1000 ms |
| `leapsecmode slew` | chrony slews at its `maxslewrate`, correcting a second in about 12 s | Recorded too, as one or two steps, because 0.8 s moves in a 10 s window |
| A smear: `smoothtime … leaponly`, or an upstream server that smears | The rate changes for hours and no single step happens | **Nothing.** The error is spread thinly, the detector's threshold is never crossed, and chrony never reports a leap as pending |

Two things about the third row. Some public time services smear leap seconds for everyone who
uses them: Google Public NTP (`time.google.com`) does, over 24 hours, and so does Amazon's Time
Sync Service. And a device given a mix of smearing and non-smearing servers sees them disagree
by up to a second on the leap day.

[ADR-0048](0048-a-bad-clock-blocks-the-start-and-flags-the-result.md) now publishes every
result that a recorded step falls inside with the flag `clock_step_during_result`. So the first
two rows already end in a flagged result. The third is the only one that produces a wrong time
nobody is told about.

## Decision

**1. A leap second is a step, and SplitForge's devices never smear.** chrony stays on its
default `leapsecmode system`, with no `smoothtime`. The device takes its time from GPS/PPS or
from servers that do not smear, never from a smearing service and never from a mix. This is a
deployment rule, stated in
[clock discipline § 5](../clock-and-time-discipline.md#5-getting-a-time-reference-without-the-internet).

**2. A result spanning the leap is flagged, not corrected.** The step is recorded like any
other, and ADR-0048 flags each result it falls inside. The time is published as measured,
which is up to one second out, and says so on its row. Nothing corrects it automatically.
A correction is possible in principle, because the size and instant of a leap are exactly
known. But it would be a derivation-time model
([clock discipline § 9](../clock-and-time-discipline.md#9-correction-happens-at-derivation-never-in-the-journal))
built for an event that last happened in 2016, and a flagged row already tells the organizer
where to look.

**3. A pending leap is said before the race and recorded with it.** chrony reports a leap as
pending (`Insert second` or `Delete second`) during the UTC day that ends with it. When it
does:
- `doctor` warns that the clock will step at midnight UTC, and which results that flags;
- `race start` goes ahead, because the clock is good, and records `leap_pending` in its clock
  record beside the state.

Neither blocks anything. A leap does not make the clock wrong before midnight or after it; it
makes one elapsed time span two clocks, and that is already flagged.

## Consequences

### What this makes easy

- **Nothing new detects the leap.** The step detector and ADR-0048's flag already cover it, and
  the pending warning is a new arm in a check that already runs.
- **The error is bounded and visible.** At most one second, on rows that say so, rather than up
  to half a second spread invisibly across every time in an ultra.
- **The only silent case is a deployment mistake**, a smearing server, and it is named.

### What this makes hard

- **The deployment rule cannot be checked by `doctor`.** Whether an upstream server smears is not
  in `chronyc tracking`, and SplitForge does not ship `chrony.conf`. The rule is documentation
  until it does.
- **Midnight UTC is evening in the Americas.** The last leap second was at 23:59:60 UTC on
  31 December 2016, 6:59 pm in New York. A New Year's Eve race on a leap night would publish
  every result spanning it with a flag.

### What we accept

- **Up to one second of error on a flagged result**, on a night that has not come since 2016.
  The 2022 General Conference on Weights and Measures resolved to discontinue leap seconds by
  2035. A negative one, which has never happened, is a forward step and is handled the same way.
- **An unflagged smear if the deployment rule is broken.** The detector cannot tell a slow rate
  change from drift, and nothing but `clock_samples` against a reference could.

## Alternatives considered

| Alternative | Why not |
|---|---|
| Rely on smearing | Exhausts the ±0.1 s budget after 2.4 h of elapsed time, and does it without any record |
| Correct spanning results by one second at derivation | Exact, and a model to build and test for an event that last happened in 2016. The flag already says where to look |
| Ignore it | Q12's own rule: deciding to ignore it is fine, doing so without noticing is not. The step is already noticed, so ignoring it would be undoing work |
| Refuse to start a race when a leap is pending | The clock is good. The leap makes one elapsed time span two clocks, which ADR-0048 flags |
| Ship a `chrony.conf` that pins `leapsecmode` | A configuration file SplitForge would own for every site's time sources. Worth doing with the Pi field guide, and not needed to decide this |

## References

- [Q12](../open-questions.md#q12-leap-second-handling)
- [ADR-0048](0048-a-bad-clock-blocks-the-start-and-flags-the-result.md): a bad clock blocks the start and flags the result
- [Clock discipline § 10](../clock-and-time-discipline.md#10-health-checks-and-alarms): the step detector
- chrony, `leapsecmode` and `smoothtime` in [chrony.conf(5)](https://chrony-project.org/doc/latest/chrony.conf.html)
