# ADR-0028: The gun decides which crossings count

- **Status:** Accepted
- **Date:** 2026-09-13
- **Supersedes:** —

## Context

Milestone 4 scores the *"first valid finish per participant"* ([timing model § 7](../timing-model.md#first-version-scoring-scope))
and measures chip time from *"the runner's own start"*. Neither sentence says what makes a
crossing valid. Until this decision, scoring answered by taking the earliest crossing of each
mat.

The earliest crossing is right when a runner crosses each mat exactly once, and runners do
not. They warm up across the start mat, walk to the corral through the finish arch, and run
courses that loop back past the start. The [2026-09-13 security review](../roadmap.md#security-review--2026-09-13)
reproduced two results against `splitforge_results::score`:

| Crossings | Scored as |
|---|---|
| Start mat at −15:00, start mat at +0:05, finish at 20:00 | Chip time **35:00**, no flag, placed |
| Finish arch at −10:00, finish at 20:00 | `finish_before_start`, no time, no place. The 20:00 finish was never considered |

The first is the worse of the two, because it is a wrong result that looks right. Neither
needs an attacker, only a start mat near the warm-up area.

[ADR-0015](0015-race-start-records-the-gun.md) already supplies the boundary. `race start`
records the gun as *"an input to scoring, not a filter on evidence"*, and reads before it are
journaled and derived like any others. What was missing is scoring using that input to decide
which crossings are part of the race.

Getting this wrong has an asymmetry worth stating. A rule that makes a chip time too long is
contested by the runner it affects. A rule that makes one too short is contested by nobody,
because nobody complains about a faster time, and the cost falls on everyone behind them.

## Decision

When a gun time is in force (`RaceConfig::gun_time()`: the most recent recorded start, or
else the scheduled start):

1. **A crossing before the gun is not part of the race.** Scoring sets it aside. It is not
   deleted, not rejected by derivation, and it remains a timing event.
2. **The start is the first start-mat crossing at or after the gun**, not the last. A course
   that passes the start mat again must not shorten a chip time. An extra crossing can only
   lengthen one, which puts the error on the runner who can contest it.
3. **A runner whose only start-mat detections came before the gun started at the gun.**
   `start_at` is the gun, the chip time equals the gun time, the entry rests on the detection
   nearest the gun, and it is flagged `start_read_before_gun`. This is usually a runner
   standing on the mat when the gun went off. If many entries carry the flag, the gun was
   probably recorded late, and `results preview` will show that before anything is
   published.
4. **A runner whose only finish-mat detections came before the gun has not finished.** Their
   status is derived as DNF, unless an operator declared otherwise, and the entry is flagged
   `finish_read_before_gun`.
5. **A crossing at the exact instant of the gun counts.**
6. **The start is not bounded by the finish.** A start crossing after the finish still
   produces `finish_before_start`, as [ADR-0017](0017-placement-semantics.md) decided. That is
   what a swapped antenna mapping looks like, and bounding the start by the finish would
   instead place that runner on their gun time.

**When no gun time is in force, nothing changes.** There is no boundary to apply, so the
earliest crossing of each mat counts, as it did before. `splitforge doctor` already warns
about a race with no gun (`config.gun_time`).

**Manual entries follow the same rules.** They are timing events
([ADR-0023](0023-manual-entries-are-derivation-inputs.md)), so a manual finish typed with a
time before the gun is set aside and flagged like any other.

**A warm-up that is not a runner's only evidence carries no flag.** Warm-ups are ordinary.
Flagging every one would teach operators to scroll past flags, and flags only work if
operators read them.

## Consequences

### What this makes easy

- Chip times are correct in the common case, at any race with a start mat near the warm-up
  area, with no configuration.
- A gun recorded late becomes visible before publication, as a run of `start_read_before_gun`
  flags, rather than after.

### What this makes hard

- **A gun recorded late now shortens some chip times.** Take a runner who crossed the start
  mat between the real gun and a `race start` typed afterwards without `--at`. Their crossing
  is now before the recorded gun, so their start is taken to be the recorded gun and their
  chip time comes out short. Before this decision, the crossing counted as recorded. The
  flag is the mitigation, and ADR-0015's `race start --at` is the fix. Gun times in that race
  were already wrong for everyone.
- **A race that started before its scheduled start, with no `race start` recorded**, has its
  earliest real crossings set aside. The mitigation and the fix are the same as above.

### What we accept

- Re-scoring a race published under the old rule can change results. Published revisions do
  not change, because they are immutable, and `results diff` shows what moved.
- A runner who warmed up across a mat and never started is still DNF rather than DNS, because
  any crossing, including one before the gun, counts as having been seen. This decision
  changes which crossings count as a start or a finish. It deliberately does not change what
  separates DNF from DNS.
- The two flags are new values in the results export's `flags` column. Under
  `RESULTS_VERSION`'s rule, an addition does not bump the version.
- **There is no start window.** Some systems count the last start detection within some
  number of minutes of the gun, which handles a runner who crosses, steps back behind the
  line, and starts again. Here, that runner is scored from their first crossing and comes out
  a few seconds slow. Revisit this with waves, which will need a boundary per wave anyway.

## Alternatives considered

| Alternative | Why not |
|---|---|
| The last start crossing before the finish | Any course that passes the start mat again silently shortens chip times, which is the direction nobody reports |
| Keep the earliest crossing, and flag pre-gun crossings | No result changes, so the wrong chip time is still the published one unless someone reads every flag, and every warm-up becomes a flag to read |
| Treat a start detected only before the gun as no start read | Places the runner on the same time, but leaves the chip-time column empty, and `no_start_read_under_chip_time` would claim there was no read when there was one |
| Keep a finish detected only before the gun as `Finished`, unplaced, `finish_before_start` | Shows a warm-up as a finisher without a time, under a flag that names a clock fault |
| Bound the start by the finish | Turns a swapped antenna mapping from a flagged, unplaced entry into a placed one |
| Drop reads before the gun at ingestion or derivation | ADR-0015 rejected gating on race state. Evidence stays, and scoring decides what counts |

## References

- [ADR-0015](0015-race-start-records-the-gun.md): the gun, and why it is not a filter on
  evidence
- [ADR-0017](0017-placement-semantics.md): the flags this adds to, and `finish_before_start`
- [ADR-0023](0023-manual-entries-are-derivation-inputs.md): why manual entries follow the same
  rules
- [Timing model § 7](../timing-model.md#first-version-scoring-scope)
- [Roadmap: Security review, 2026-09-13](../roadmap.md#security-review--2026-09-13)
- `crates/splitforge-results/src/lib.rs`: `Crossings`, and the tests under the ADR-0028
  comment
