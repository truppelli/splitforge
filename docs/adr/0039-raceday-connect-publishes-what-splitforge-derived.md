# ADR-0039: RaceDay Connect publishes what SplitForge derived, a runner at a time

- **Status:** Proposed
- **Date:** 2026-09-25
- **Extends:** [ADR-0006](0006-optional-outbound-integrations.md)

## Context

[ADR-0006](0006-optional-outbound-integrations.md) settled that every outbound integration is
optional, asynchronous and outbound-only, and [architecture § 5](../architecture.md#5-the-raceday-connect-boundary)
drew the RaceDay Connect boundary before any of it was built. The other side is now built:
RaceDay Connect, the public race page, has pairing and three timing endpoints (a course
manifest, live crossings, result revisions), fitted to SplitForge's domain model on
2026-09-24. Nothing sends to them yet. The organizer plans to time real races with live
results on the page, so the producer is the gap.

Four things SplitForge holds do not map onto that contract directly, and each one fails
silently if it is handled wrong:

- **Crossings are re-derived, not appended.** Derivation is a pure function of the journal
  and the configuration, so a crossing already published can stop being true: a chip
  reassigned to another bib, a gun recorded late that renumbers laps
  ([ADR-0029](0029-a-race-starts-at-the-gun.md)), or a read set aside. RaceDay Connect's
  crossing endpoint only upserted, so a crossing, once sent, stayed on the live board for
  the rest of the race.
- **Lap 0 exists.** ADR-0029 counts every crossing that was over before the gun as lap 0,
  and a lap-1 crossing can start a moment *before* the gun. RaceDay Connect refuses a lap
  below 1 and a negative elapsed time, and it refuses them **for the whole batch**, which
  would park the outbox behind a permanent error.
- **SplitForge does not know how long the course is.** A checkpoint has a role and a
  sequence, not a position. Scoring never needed one; the public page's pace, progress rail
  and live ordering all do.
- **A response is not just success or failure.** A replay is success. An unpaired box will
  never succeed. A refused revision will be refused again byte for byte. Retrying those
  last two forever blocks everything queued behind them; dropping a transient failure loses
  results.

## Decision

**1. Publishing is a pure translation, in `splitforge-sync`, from state SplitForge already
holds.** The manifest, crossings and revisions are functions of `RaceConfig`, the timing
events and a `ResultRevision`. Nothing in them reads a clock, a file or the network, and
nothing in them is reachable from the read path, so ADR-0006's byte-identical-exports
criterion holds by construction: publishing cannot change what was derived.

**2. Crossings are published a runner at a time.** Each pass derives every runner's
crossings, compares them with what was last sent, and sends every runner whose set changed,
including a runner who now has none, whole and named in `replaces`. RaceDay Connect keeps
only what that batch restates for each named runner. A runner is never split across two
batches, because the second would retract the first. A runner whose crossings did not
change is not sent. The `replaces` field was added to RaceDay Connect's contract for this
(SmartSponsor `19f213b`), and it is optional, so the endpoint's older shape still works.

**3. What is not the race is not sent, and the operator is told.** Lap-0 crossings, crossings
by a participant not in the roster, and bibs longer than the 20 characters RaceDay Connect
stores are left out and reported as `Skipped`, with the reason, rather than sent to be
refused. Elapsed time is floored at zero and omitted entirely when no gun is known; the
page works it out once the manifest carries a gun.

**4. The operator places the course once, and only the splits need a number.** A `Course`
names the SplitForge race, the distance key it publishes under, the certified distance, and
each intermediate split's position along one lap. The start is placed at zero and the
finish and lap mats at one lap's length. A split with no position, or one off the lap, is
an error naming the checkpoint, raised before anything is sent. **The distance key is
chosen once**, because RaceDay Connect numbers revisions per key.

**5. A revision is published as SplitForge scored it.** Its number, status, digest and
placings go as they are; RaceDay Connect never recomputes a place. Splits are measured from
the same start as the result (the runner's own start-line read under chip timing, the gun
otherwise), so the finisher page's segments add up to the time it shows. A runner with a
blank roster name is published as `Bib <n>`, because RaceDay Connect needs a name on every
row and refusing the revision would withhold every other runner's result over one field.

**6. Every answer is classified before the outbox acts on it.**

| Answer | Outcome |
|---|---|
| 2xx, replay included | Delivered |
| 401 | Unpaired — stop publishing this race until the operator pairs again |
| 408, 429, 5xx, or no answer at all | Retry, honouring `Retry-After`, else backing off from 1 s to at most 60 s |
| any other 4xx | Refused — set aside and reported, never resent |

**7. The outbox and the transport are the next slice, and they are still ADR-0006's.** An
`outbox_messages` table in `splitforge-storage`, written in the same transaction as what it
describes; a shipper in `splitforge-edge` that reads it off the read path; the last-sent
crossings per runner, so a restart does not resend a whole race; a `splitforge raceday pair`
command; and the HTTP client. **Choosing that client adds TLS to the dependency tree**, which
[the dependency policy](../../deny.toml) treats as a decision, so it is left to that slice.

## Consequences

### What this makes easy

- A correction on the timer reaches the live board: it is sent as the runner's new state,
  not as a crossing that has to be found and deleted.
- The translation can be tested exhaustively without a network, and every contract field
  name is pinned by a test against the JSON RaceDay Connect's own tests send.
- A spectator's board recovers from a lost uplink in one pass: the diff against what was
  last sent is exactly what the page is missing.

### What this makes hard

- The shipper has to keep what it last sent per runner, durably, or a restart turns into a
  resend of every runner in the race. That is a table, and a stored data shape.
- A course has to be configured before a race can be published, which is one more thing to
  do at 5 a.m. The start and finish need nothing, which keeps a point-to-point race to one
  number.

### What we accept

- **Splits on a lap course are the last lap a mat read each runner.** RaceDay Connect's split
  has no lap, and a finisher page listing the same mat four times is worse than one line.
  Scoring does not do intermediate splits on lap courses either (Milestone 4 excludes them).
- **The page shows only what SplitForge's roster holds: bib and name.** RaceDay Connect has
  gender, age, city and division; SplitForge's participant has none of them, so those
  columns and the gender and division places stay empty until the roster grows them.
- A distance key that is changed is a new distance, whose revisions start again at 1, and
  the old one stays on the page. The key is documented as permanent rather than migrated.

## Alternatives considered

| Alternative | Why not |
|---|---|
| Send each new crossing once, as it is derived | Cannot take one back. The first chip reassignment leaves a phantom runner on the board for the rest of the race |
| Resend every crossing on every pass | Correct, and a 2,000-runner field is ~6,000 crossings per pass over a finish-line uplink — sent every few seconds, mostly unchanged |
| A delete endpoint for single crossings | Needs a stable identity for a crossing across derivations. The runner is that identity already; a crossing is not |
| Leave lap 0 and negative times to RaceDay Connect to refuse | It refuses the whole batch, and the outbox would retry the same refusal forever |
| Ask RaceDay Connect to make `meters` optional | The live board orders mats by position and the page computes pace from distance. Without it the page is worse for every race; the operator knows the numbers |

## References

- [ADR-0006](0006-optional-outbound-integrations.md) — outbound integrations are always optional
- [ADR-0029](0029-a-race-starts-at-the-gun.md) — lap 0, and a lap-1 crossing before the gun
- [Architecture § 5](../architecture.md#5-the-raceday-connect-boundary)
- SmartSponsor `docs/architecture/20-raceday-connect.md`, "The contract with SplitForge", and
  `TimingIngestController` — the consumer's side of this contract
