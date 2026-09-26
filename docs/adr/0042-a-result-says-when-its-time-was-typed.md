# ADR-0042: A result says when its time was typed

- **Status:** Accepted
- **Date:** 2026-09-26
- **Extends:** [ADR-0023](0023-manual-entries-are-derivation-inputs.md), [ADR-0028](0028-the-gun-decides-which-crossings-count.md)

## Context

[ADR-0023](0023-manual-entries-are-derivation-inputs.md) made a manual entry an input to
derivation: what an operator writes down when the chip does not becomes a timing event, and
scoring treats it like any other. [ADR-0028](0028-the-gun-decides-which-crossings-count.md) made
the finish the first crossing at or after the gun, and the start likewise.

The 2026-09-13 security review reproduced what those two leave together. A runner with a chip
finish at 20:00, and a manual entry at 18:20 added for the same checkpoint, was scored 18:20 and
placed first. The two results, with and without the entry, were identical apart from the time:
the same place, and no flag. `ResultEntry` keeps the ids of the events it used, but no export
column, no flag and no `results diff` said that a published time had been typed in. The audit
trail recorded the `manual add`, and nothing pointed at it. It also found that `manual add
--reason ""` was accepted, where `results declare` refuses a blank reason and the help calls the
reason required.

No attacker is needed for any of this. An operator's typo on race day moves a runner up the
results, and nobody reading them can tell.

## Decision

**1. Scoring does not change.** The earliest crossing at or after the gun still counts,
whatever produced it. An operator may enter a time on purpose, for a runner whose chip read late
as well as one whose chip did not read, and choosing between a chip and a hand is a decision
about evidence this ADR does not make.

**2. A result says where each of its times came from.** Two new flags:
- `manual_start`: the start the result is measured from was entered by hand.
- `manual_finish`: the finish was entered by hand.

**3. And says more when a hand beat a chip.** Two more, added beside the first two:
- `manual_start_over_chip`: a chip crossed the start line at or after the gun, and a manual
  entry earlier than it was used instead.
- `manual_finish_over_chip`: the same at the finish, which is the reproduced case.

A manual entry that loses to an earlier chip crossing is not used, changes nothing, and carries
no flag.

**4. The export contract takes them without a version bump.** `RESULTS_VERSION` is bumped when a
field changes meaning or disappears. `flags` keeps its meaning, the conditions a result carries,
and gains values, which is the extension its rule allows. A consumer that knew every earlier value
and meets a new one sees a flag it does not recognise on a result that needs looking at, which is
the right thing for it to notice.

**5. `manual add` refuses a blank reason**, with the same message shape as `results declare`.

## Consequences

### What this makes easy

- **A typed time is visible wherever a result is read**: the CSV's `flags` column, the JSON, and
  `results show`. The entry it came from is one `manual list` away.
- **The reproduced case is the loudest one.** A hand beating a chip is the combination most
  likely to be a typo, and it carries its own flag.

### What this makes hard

- **Consumers that enumerate flags meet four new values.** The contract's rule allows it, and the
  CSV's column list does not change.

### What we accept

**The typo still wins.** This makes it visible, not wrong. Refusing a manual entry earlier than a
chip crossing, or preferring chips to hands, would be a change to what a result is, and belongs in
its own decision if the flags show it is needed.

## Alternatives considered

| Alternative | Why not |
|---|---|
| Prefer a chip crossing to a manual entry at the same checkpoint | Changes what counts. An operator correcting a late chip read would be overruled by the chip, and ADR-0023 made the entry an input on equal terms |
| A `finish_source` column in the CSV | A new column is the one change a positional reader survives only at the end, and it says less than the flags: nothing about a chip that was passed over |
| Bump `RESULTS_VERSION` | Nothing changed meaning or disappeared, so the contract's own rule does not ask for it |
| Warn at `manual add` time only | The warning reaches the operator who typed it and nobody who reads the results afterwards |

## References

- [ADR-0023](0023-manual-entries-are-derivation-inputs.md): manual entries as derivation inputs
- [ADR-0028](0028-the-gun-decides-which-crossings-count.md): which crossing counts
- [roadmap, security review](../roadmap.md#security-review--2026-09-13): the finding
- `crates/splitforge-export/src/lib.rs`: `RESULTS_VERSION` and its rule
