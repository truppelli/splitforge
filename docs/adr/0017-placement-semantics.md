# ADR-0017: What takes a place, and what a place is worth

- **Status:** Accepted
- **Date:** 2026-08-16
- **Supersedes:** —

## Context

Milestone 4 assigns "overall placement by the selected timing policy". That sentence hides
four decisions, each of which is the kind of thing a runner will argue about, and each of
which is silently made by whoever writes the sort.

1. **Who competes for a place?** Finishers obviously. What about a disqualified runner who
   physically crossed the line third — do they occupy third, or does everyone move up?
2. **What happens to ties?** Two runners on the same recorded millisecond is rare but not
   negligible with a shared mat and a 3 s dedup window.
3. **What is the tiebreak when times are genuinely equal?** Some deterministic order is
   required or the results sheet reshuffles between runs of the same command.
4. **What about a finisher with no defensible time** — a chip-timed runner who missed the
   start mat, or one whose finish precedes their start because of a clock fault?

None of these has a single universal answer, which is why they belong in a decision record
rather than in a comparator.

## Decision

**1. Only `Finished` takes a place.**

`DNS`, `DNF`, and `DQ` are unplaced, and the runners behind them move up. A disqualified
runner who crossed third does not occupy third — the second-place finisher becomes second,
not third-of-a-field-with-a-hole.

**Their measured times are kept and shown.** The clock did not lie about when they crossed;
the decision was about whether it counted. Erasing the time would destroy the evidence that
makes the disqualification reviewable, and `docs/timing-model.md` § 1 does not permit
throwing away a measurement because of a later judgement.

**2. Ties share a place, and the shared places are consumed.** Standard competition ranking:
1, 2, 2, 4. Two runners tied for second are both second, and the next finisher is fourth.
This is what athletics does, and the alternative ("dense" ranking, 1, 2, 2, 3) quietly
awards the fourth-fastest runner a third place.

**3. Ties break by bib, numerically where the bib is numeric.** Not by name, not by
insertion order, not by participant UUID. It is arbitrary — any tiebreak is — but it is
*stable*, *visible*, and *explicable to the person who came fourth*. Bibs are strings
because they are not always numeric, so `10` sorting before `9` is a real hazard; the
comparator parses them as numbers when it can.

**4. A finisher with no defensible time is placed nowhere and flagged.** Specifically:

| Situation | Status | Place | Flag |
|---|---|---|---|
| Chip timing, no start-line detection | `Finished` | placed, on gun time | `no_start_read_under_chip_time` |
| No gun recorded and no scheduled start | `Finished` | none | `no_gun_time` |
| Finish precedes the start it is measured from | `Finished` | none | `finish_before_start` |
| Operator declared `Finished`, no finish crossing | `Finished` | none | `declared_finished_without_finish_read` |

The first row is the interesting one. `docs/timing-model.md` § 7 requires that a chip-timed
participant with no start read is "a flagged condition with a defined fallback, not a null",
and the defined fallback is the gun. A missed start mat must not turn a finisher into a
blank row — but the result must not silently claim to be a chip time either, so the entry
carries both the fallback and the flag.

The remaining three produce no time because no honest time exists. They are reported rather
than guessed, and `results preview` counts flagged entries so they are visible *before*
anybody publishes.

## Consequences

**Good.** Every one of these questions has an answer written down before somebody asks it
at a prize-giving, and each answer is a test in `crates/splitforge-results`. The
consequential ones — a DQ vacating its place, ties consuming places — are tested against
hand-computed expectations rather than against the implementation's own output.

**Good.** Scoring never produces a negative or nonsensical duration in a `place` column.
The worst it produces is an absent place next to a visible flag, which is a question an
operator can act on.

**Bad.** A field of two thousand with a bib-number tiebreak means, in the rare exact tie,
that the lower bib appears first. Somebody will eventually notice and dislike it. The
defence is that the alternative is not "fairer", it is "unstated" — and an unstated tiebreak
reshuffles between two runs of the same command, which is worse in every way that matters.

**Bad.** Keeping a disqualified runner's time on the published sheet means the time is
publishable, quotable, and screenshot-able. That is a deliberate trade: the alternative
hides the evidence that makes the decision reviewable.

**The thing to watch.** Age-group and wave scoring (excluded from this milestone) will want
placement *within a subset*. When that lands, these four rules should apply unchanged within
each subset rather than being reimplemented alongside a second, subtly different comparator.

## Alternatives considered

**A disqualified runner keeps their position, and places have holes.** Some sports do this
for provisional results pending appeal. Rejected because SplitForge already has a better
mechanism for "pending appeal": publish a provisional revision, and publish another when the
appeal resolves ([ADR-0016](0016-status-declarations-are-evidence.md)). Two mechanisms for
the same uncertainty is one too many.

**Erase a disqualified runner's times.** Rejected above — it destroys the measurement the
decision has to be reviewed against.

**Dense ranking for ties (1, 2, 2, 3).** Rejected: it awards the fourth-fastest finisher a
third place, which is the thing a results sheet is supposed not to do.

**Refuse to place anyone when any entry is flagged.** Considered, because a results sheet
with a known problem in it is a results sheet somebody will publish anyway. Rejected as
disproportionate: one runner who missed a start mat should not block results for the other
four hundred. `results preview` surfacing the flag count before publication is the
proportionate version, and `doctor` already exists for the pre-race checks that *should*
block.
