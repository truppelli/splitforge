# ADR-0048: A bad clock blocks the start and flags the result, and never blocks a publish

- **Status:** Accepted
- **Date:** 2026-09-29
- **Resolves:** [Q11](../open-questions.md#q11-clock-error-budget-enforcement)
- **Extends:** [ADR-0019](0019-pre-race-gates-block-but-can-be-overridden.md)

## Context

[Q11](../open-questions.md#q11-clock-error-budget-enforcement) asked whether SplitForge should
refuse to publish a `final` revision when clock error exceeds the ±0.1 s budget
([clock discipline § 2](../clock-and-time-discipline.md#2-what-accuracy-does-race-timing-actually-need)),
or publish with an accuracy caveat recorded in the revision. It leaned toward the caveat.

Two other things were waiting on it. [Clock discipline § 10](../clock-and-time-discipline.md#10-health-checks-and-alarms)
makes clock state a **blocking** pre-race item, and `doctor`, `clock_source.rs` and the
roadmap all say which states should refuse a `race start` is Q11's to decide. And every read
already records `device_clock_state`, and the service records every wall-clock step, but
nothing carries either into a published result. `doctor` says reads under an untrustworthy
clock *"need an accuracy caveat"*, and no caveat exists.

What can be known without hardware is narrower than what Q11 asked about. **Accumulated drift
measured against a reference** needs `clock_samples`, which needs a GPS/PPS source and an
LLRP reader ([M5](../roadmap.md#still-open--and-nearly-every-item-of-it-needs-hardware)). Two
facts are already in the database:

- the device's clock state when each read arrived, and whether that read was timed by the
  device's clock or the reader's (`timestamp_source`);
- every step the wall clock made, with the span it was seen across (`clock_steps`).

## Decision

**1. Publishing never refuses on the clock.** Q11's leaning, taken. A timer that will not
produce results has failed at its job, and whether a result stands belongs to the organizer
([ADR-0019](0019-pre-race-gates-block-but-can-be-overridden.md), threat model § 5).

**2. The caveat goes on each result it applies to**, as a flag beside the ones scoring already
attaches, not as one figure on the revision. A caveat on the revision says *"some of this may
be wrong"* and leaves everybody to guess which rows. Two flags:

| Flag | On an entry when |
|---|---|
| `untrusted_device_clock` | a crossing its time rests on was timed by the device's clock (`DeviceReceipt`) while `device_clock_state` was not trustworthy (`manual` or `unsynced`) |
| `clock_step_during_result` | a recorded step's span overlaps the entry's, from the earlier of the gun and its start to its finish |

A read timed by the reader's own clock is not flagged by the first: whether to trust that clock
is [Q3](../open-questions.md#q3-reader-clock-trust-defaults). A manual entry is not flagged: a
person typed its time. The overlap in the second is deliberately generous at the edges, because
a step is only seen between two samples ten seconds apart.

Flags are stored in `flags_json`, so **no migration**. They change no time, status or place, and
the revision digest does not cover flags, so adding them cannot make identical results look
changed. They reach `results show` and both exports: the CSV `flags` column lists labels, and a
new label is additive under the export contract. RaceDay Connect's contract carries no flags
today, so the public page does not show them. Whether it should is that integration's decision.

**3. `results publish` says so, after writing.** The view gains `clock_caveats`, the number of
entries carrying either flag, and the audit row carries it because it carries the view. When
it is non-zero, a warning on stderr names the flags and `results show`.

**4. `race start` refuses on a clock measured untrustworthy**, on ADR-0019's pattern:
`--force --note` goes ahead, and the start records the clock either way, as
`{"measurement": …, "state": …}` in the audit detail and the command's output.

- It refuses only on a **measurement** that says the clock is bad: `chronyc` answered, and the
  state is not trustworthy. No `chronyc` at all is a laptop as often as a Pi, and a daemon that
  did not answer is a failed status query. Neither is refused. Both are recorded, so a reviewer
  can see the clock was never established.
- It asks only when the gun is now. `race start --at` records a gun timed another way, after
  the reads it matters to were taken. Refusing it because the clock is bad *now* would block
  the recovery ADR-0015 exists for.
- `--force` is the same flag that overrides the free-space floor. One reason covers both, and
  the record says what each gate measured.

**5. The ±0.1 s budget is not enforced by these.** Neither flag measures error. They mark
results whose error nobody established. Enforcing the budget needs a drift estimate, which
needs `clock_samples`. When that exists, it becomes a third flag on this pattern, carrying the
estimate, and it still does not refuse.

## Consequences

### What this makes easy

- **A disputed time has its answer on the row.** *"Was the clock right when this was
  recorded?"* is a flag, not a forensic query across three tables.
- **The organizer decides, knowingly.** A revision with caveats is published, loudly, and the
  audit trail holds how many.
- **Old revisions stay readable.** No schema change, and a revision published before this has
  no clock flags because none were computed, which is true.

### What this makes hard

- **A Phase 0 Pi needs `--force` for every start.** chrony reports a DS3231-set clock as not
  synchronised, so `rtc` is unreachable and such a device reads `unsynced`
  ([hardware plan § 7](../hardware-plan.md#7-software-plan)). Without GPS or a network with NTP,
  every start on it is forced. That is the gate working: the clock really is unestablished.
  ADR-0019's warning applies, and the audit trail is where a habit of forcing shows.
- **A result can carry a flag it did not deserve**, at the edges of a step's ten-second window.
  It costs a look. The opposite error costs a wrong time nobody questions.
- **A flag alone is not a new result.** Because the digest ignores flags, a caveat that appears
  after a revision was published (a step recorded later, say) needs `--allow-unchanged` to
  publish. That is the digest doing its job: no time, status or place moved.
- **A machine with chrony running unsynchronised fails the CLI's own `race start` tests.** CI has
  no daemon, so it takes the not-measured path. The gate's tests put a fake `chronyc` on `PATH`.

### What we accept

That a result with a caveat is still published, placed and exported. SplitForge's job is that
nobody reads it without being told.

## Alternatives considered

| Alternative | Why not |
|---|---|
| Refuse to publish `final` over the budget | Q11's other option. The software would be deciding whether a result stands, and there is no drift estimate to compare with the budget yet |
| One caveat on the revision instead of flags on entries | Says some rows may be wrong without saying which, and needs a migration where flags need none |
| Refuse a start when the clock state is unknown too | Stops a race because a status query failed, and fires on every laptop |
| A separate `--i-know-the-clock-is-wrong` override, as § 10 first wrote | Two escape hatches on one command. `--force --note` already requires the reason and records it |
| Gate `race start --at` as well | Blocks after-the-fact recovery on the state of a clock the recorded gun did not come from |
| Flag every result in a race with any untrusted read | Blames finishers whose own crossings were timed fine |

## References

- [Q11](../open-questions.md#q11-clock-error-budget-enforcement)
- [ADR-0019](0019-pre-race-gates-block-but-can-be-overridden.md): pre-race gates block, and can be overridden on the record
- [Clock discipline § 10](../clock-and-time-discipline.md#10-health-checks-and-alarms)
- `crates/splitforge-results/src/clock.rs`, `crates/splitforge-cli/tests/clock_budget.rs`
