# ADR-0019: Pre-race gates block, and can be overridden on the record

- **Status:** Accepted
- **Date:** 2026-08-17
- **Extends:** [ADR-0014](0014-mutable-configuration-immutable-evidence.md)

## Context

Implementing the free-space check surfaced a contradiction between two things the threat
model already says.

[§ 6](../threat-model.md#6-pre-race-operational-checklist) lists a pre-race checklist and
introduces it as: *"Blocking items. Any failure stops the race start, not the timing."* Free
disk space is the first entry.

[§ 5](../threat-model.md#5-design-decisions-that-follow-from-this-model) states as a design principle: *"Availability
is a security property here. A timer that refuses to run because a security check failed has
caused the harm it was protecting against."*

Both are right, and they cannot both be implemented literally. A check that always blocks
will one day stop a race that would have been fine — a threshold set for a different card, a
measurement that is wrong, a 5K that needs a hundredth of the space the floor assumes. A
check that never blocks is a warning, and warnings scroll past at 07:55.

This is not specific to disk space. Every remaining item on that checklist — clock state,
reader connectivity, roster counts — will meet the same question, so it is worth settling
once rather than per-check.

## Decision

**A pre-race gate refuses by default, and can be overridden with `--force`, which requires
a reason and records both in the audit trail.**

```console
$ splitforge race start
error: 180 MB free, below the 256 MB floor. The journal has to hold the whole event, and
       a disk that fills mid-race stops recording. Free space, lower the floor with
       `splitforge device set --min-free-mb`, or start anyway with `--force --note "..."`.

$ splitforge race start --force --note "USB SSD attached, floor is stale"
{"action":"start","forced":true,"free_mb":180,...}
```

```json
{"action":"race.start","detail":{"forced":true,"free_mb":180,
                                 "reason":"USB SSD attached, floor is stale"}}
```

- **The refusal is the default**, so § 6's "blocking" is true: nobody starts below the floor
  by not noticing
- **The override exists**, so § 5 holds: the organizer, not the software, decides whether
  their event runs
- **`--force` requires `--note`**, enforced by the parser rather than at runtime. An escape
  hatch nobody has to justify is a check nobody has to think about
- **The measurement is recorded either way.** A start that went ahead at 180 MB says so,
  which is what makes the decision reviewable when somebody asks afterwards why the journal
  stopped at 14:20
- The error message carries the numbers, the threshold, and every way out. The person
  reading it is outdoors and has somewhere else to be

This is the same shape as `results publish --reason`: the irreversible action is available,
and taking it writes down who took it and why.

### Thresholds live in a device-level table

Schema v4 adds `device_settings`, a key/value table for settings that belong to the machine
rather than to any race. A free-space floor is a property of an SD card; it does not become
a different number because the 10K starts.

Mutable, per [ADR-0014](0014-mutable-configuration-immutable-evidence.md), so no append-only
triggers — the audit log carries the history of changes, which is where it belongs.

Key/value rather than a column per setting because the alternative is a migration every time
a device-level knob appears, and the ones already visible on the roadmap
([Q3](../open-questions.md#q3-reader-clock-trust-defaults): reader clock trust defaults,
offset and skew alarm thresholds) are exactly that shape.

### Shedding order

[architecture.md § 4](../architecture.md#4-failure-behavior) requires that non-essential
writes go first and the journal keeps writing longest. `backup create` therefore refuses
when a snapshot would not leave the floor intact, and says explicitly that the timer is
unaffected. The journal itself is never gated on free space — there is no threshold at
which refusing to record a read is the better outcome.

## Consequences

### What this makes easy

- The checklist in § 6 becomes executable rather than aspirational: `race start` is the
  thing that enforces it
- "Why did we start with 180 MB free?" has an answer, with a name and a reason attached
- Future gates — clock state especially, which is the one that actually threatens result
  accuracy — have a settled pattern to follow instead of relitigating this
- A wrong threshold costs one flag, not a stopped event

### What this makes hard

- `--force` can become habit. If every start needs it, the floor is wrong, and the audit
  trail is where that becomes visible — but only if somebody reads it
- Two more arguments on `record_session`, carried solely to record the override
- The 256 MiB default is a judgement, not a measurement. It is ~1000× the 5K fixture's
  footprint, which is the right margin when being wrong means a race stops recording — but
  nobody has yet measured a full day's journal on a real card

### What we accept

That the gate can be walked past, and that this is correct. SplitForge does not own the
decision about whether an event runs; the organizer standing in the field does. What
SplitForge owns is making sure that decision is deliberate and that it leaves a record.

We also accept that the free-space check tests the *decision*, not the failure. What SQLite
and the sidecar do when a write genuinely fails partway through needs a real full volume,
and stays in Milestone 5 with the rest of the hardware work.

## Alternatives considered

| Alternative | Why not |
|---|---|
| Refuse absolutely, no override | The literal reading of § 6. Simplest to reason about, and it means a stale threshold or a bad measurement can stop a race that was going to be fine — the exact harm § 5 names |
| Warn loudly, never refuse | The literal reading of § 5. Makes the checklist decorative: a warning printed at 07:55 among other output is a warning nobody saw |
| `--force` without a required reason | Cheaper to type and worthless afterwards. The value is not that the override exists, it is that it is explicable |
| A separate `--reason` flag alongside the existing `--note` | Two flags meaning the same thing on one command. `--note` already exists and already lands in the audit trail; `requires = "note"` reuses it |
| Threshold as a CLI flag with a compiled-in default | No migration and no new commands, but the operator has to remember it on every invocation, and the number belongs to the device rather than to whoever typed the command |
| Threshold in `timing_policies` | Race-scoped, and disk space is not a property of a race |

## References

- [threat-model.md § 5](../threat-model.md#5-design-decisions-that-follow-from-this-model) and [§ 6](../threat-model.md#6-pre-race-operational-checklist)
- [architecture.md § 4](../architecture.md#4-failure-behavior)
- [ADR-0014: Mutable configuration, immutable evidence](0014-mutable-configuration-immutable-evidence.md)
- [ADR-0018: Evidence is written to a text sidecar before it reaches the database](0018-write-ahead-sidecar-journal.md)
