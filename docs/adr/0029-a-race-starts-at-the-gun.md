# ADR-0029: A race starts at the gun: who started, and where laps count from

- **Status:** Accepted
- **Date:** 2026-09-13
- **Supersedes:** —

## Context

[ADR-0028](0028-the-gun-decides-which-crossings-count.md) decided which crossings count as a
start or a finish, and deliberately left one question open: what makes a runner a DNF rather
than a DNS. Scoring answered it by counting any timing event as a sighting, including one
before the gun. So a registered runner who warmed up through the finish arch and never
started was a DNF.

The answer is the race promoter's. **The promoter starts the race, which records the gun, and
every runner is noted at the starting line when it starts. Each lap is recorded from there.**
Placement follows from that sequence, and so does the difference between a runner who started
and one who did not.

Checking the code against that sequence found two more places that ignored the gun, both in
derivation rather than in scoring:

- **Laps counted from a runner's first crossing, not from the gun.** A warm-up over the lap
  line was lap 1, so the first real lap was recorded as lap 2. Lap numbers appear in
  `splitforge derive`, in the `export crossings` lap column, and in every derived timing-event
  identifier.
- **The minimum lap reached back across the gun.** In a criterium whose start line is also the
  lap line, a warm-up 60 s before the gun made the start crossing 5 s after it look like a
  re-read inside a 120 s minimum lap, and it was rejected. The runner's start was lost to their
  warm-up.

Derivation had no way to know the gun. [ADR-0015](0015-race-start-records-the-gun.md) had
made the gun an input to scoring only, and the rule above needs it in derivation as well.

## Decision

When a gun time is in force (`RaceConfig::gun_time()`: the most recent `race start`, or else
the scheduled start):

1. **A runner started if they were at the start line when the race started, or were seen
   anywhere on the course after it.** Otherwise they are a DNS.
   - *At the start line* means a start crossing at or after the gun, or a start read before it,
     which ADR-0028 already treats as a start at the gun.
   - *Seen after it* means any crossing at or after the gun, at any checkpoint. That covers a
     runner the start mat missed, who is still a starter.
   - A crossing before the gun anywhere other than the start line proves nothing.
2. **Laps count from the gun.** Every crossing that was over before the gun is lap 0, and the
   first crossing on the race side of it is lap 1. Manual entries follow the same rule.
   - *On the race side* means the crossing's reads were still arriving when the gun went: its
     **last** read came at or after the gun. A runner standing on the mat when the race starts
     is on lap 1, and a warm-up that ended before the gun is lap 0. A manual entry is one
     instant, so it is on the race side when it is at or after the gun.
   - *Not the credited read.* Under the default selection rule the credited read is the burst's
     first, so a runner on the mat at the gun is credited a moment before it. Numbering by that
     instant gave them lap 0 and the runner a step behind lap 1 for the same start. The credited
     instant is unchanged, and still what scoring uses.
3. **The minimum lap is not measured across the gun.** A crossing that was over before the gun
   cannot make one on the race side a re-read. Between two crossings on the same side of the
   gun, the minimum lap applies as before.
4. **The gun filters nothing.** Every read before it is still grouped, selected, accepted, and
   turned into a timing event, as ADR-0015 requires. Derivation now knows the gun
   (`DerivationInput::gun_time`), and it uses it only to number laps and to bound the minimum
   lap.

**With no gun in force, nothing changes.** Laps count from the first crossing, the minimum lap
applies to every pair, and any crossing means a runner started.

**This does not change where the gun comes from.** Recording `race start` is the promoter's
act. A race nobody started still falls back to its scheduled start, per ADR-0015.

## Consequences

### What this makes easy

- Statuses and lap numbers match what the promoter saw: who was at the line when the race
  started, and each lap from there.
- A warm-up can no longer cost a runner their start crossing, their first lap number, or their
  status.

### What this makes hard

- **Derivation depends on configuration it did not use before.** Recording or correcting
  `race start` changes the lap numbers of crossings near the gun on the next `derive`. Nothing
  stored changes, because derivation is recomputed on demand, and re-deriving is still
  byte-identical for the same journal under the same configuration.
- **Timing-event identifiers of pre-gun crossings change**, because the lap number is part of
  the derived identifier. A revision published before this change that rested on such an event
  names an identifier that re-derivation no longer produces. Under ADR-0028, the only such
  event is a start read before the gun. Revision digests do not include identifiers, so no
  digest changes because of this.

### What we accept

- **A runner who warmed up across the start mat and left before the gun is a DNF, not a DNS.**
  Without a start window, a start read before the gun cannot be told apart from a runner
  standing on the line when it went off. The entry carries `start_read_before_gun`, so it can
  be found and declared DNS.
- **Lap 0 is a value consumers will see.** The crossings export shows `0` in the lap column for
  a crossing that was over before the gun, which is the plainest way to say "before the race".
  Its credited time can still be a moment before the gun on lap 1, for a runner who was on the
  mat when it went.
- Re-scoring a race published under the old rule can move a runner from DNF to DNS. Published
  revisions do not change, and `results diff` shows the move.

## Alternatives considered

| Alternative | Why not |
|---|---|
| Any crossing proves a start (the previous behavior) | Every warm-up turns a DNS into a DNF, which is not what the promoter saw at the line |
| Only a start-mat crossing proves a start | A runner the start mat missed who then ran three laps would be a DNS |
| Judge a crossing's side of the gun by its credited instant | The credited read is usually the burst's first, so a runner on the mat at the gun was lap 0 and the runner a step behind lap 1 for the same start. The four-lap criterium fixture caught it: its first rider crosses at the gun and came out one lap behind the other five |
| Leave pre-gun crossings without a lap number (`Option<u16>`) | Changes the shape of `TimingEvent` and every consumer, to carry the same information lap 0 already carries |
| Drop crossings before the gun from derivation | A filter on evidence, which ADR-0015 rejected |
| A start window (a crossing within N minutes before the gun counts as the start) | Needs a number nobody has measured. For laps, a burst's last read already says whether the runner was on the mat when the gun went. Revisit with waves, which need a boundary per wave anyway |

## References

- [ADR-0015](0015-race-start-records-the-gun.md): `race start` records the gun, and the gun
  is not a filter on evidence
- [ADR-0028](0028-the-gun-decides-which-crossings-count.md): which crossings count as a start
  or a finish
- [Timing model § 5](../timing-model.md#5-deduplication) and
  [§ 7](../timing-model.md#first-version-scoring-scope)
- [Roadmap: Security review, 2026-09-13](../roadmap.md#security-review--2026-09-13): the
  question this answers
- `crates/splitforge-engine/src/lib.rs` and `crates/splitforge-results/src/lib.rs`: the tests
  under their ADR-0029 comments
