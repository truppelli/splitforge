# ADR-0006: Outbound integrations are always optional

- **Status:** Accepted
- **Date:** 2026-08-14

## Context

SplitForge is expected to interoperate with event-management and results-distribution
platforms, RaceDay Connect among them. The natural way to build that is to treat the
platform as the system of record and SplitForge as its field client.

That design fails in exactly the conditions SplitForge exists for. A checkpoint at the far
end of a trail race has no signal. A platform outage during a Saturday morning 10K is not
an inconvenience to be retried — it is a race that cannot be timed.

## Decision

**Every outbound integration is optional, asynchronous, and outbound-only.** No integration
may sit on any path required to record, derive, or export timing data.

Enforced as follows:

- Only `splitforge-sync` may reference an external service. No other crate may
- `splitforge-sync` is never a dependency of `splitforge-engine`, `splitforge-results`, or
  anything on the read path
- Outbound messages are written to a local `outbox_messages` table and shipped
  asynchronously with retry. Ship failure is a warning, never an error that blocks timing
- Credentials live in local config, are never required for startup, and their absence
  disables sync rather than the timer
- **An event timed with sync disabled must produce byte-identical exports to one timed
  with it enabled**

That last point is the testable form of the whole decision.

## Consequences

### What this makes easy

- The timer works in a field, a basement, or a car park with no signal
- Integration outages are somebody else's problem, discovered after the race
- Adding a second or third destination changes nothing structurally
- CSV and JSON exports remain the primary contract, usable by anyone

### What this makes hard

- No live results without deliberate additional work
- Reconciliation is more complex: the local database and the remote platform can diverge
  and must be resynchronized
- Two paths to test — with and without sync — and the byte-identical requirement means
  testing both properly

### What we accept

Losing the convenience of treating a remote platform as the source of truth. In exchange,
SplitForge is useful standalone to an organizer who has never heard of RaceDay Connect,
which is a precondition for being a credible open-source project rather than a client for
one product.

The operational commitment this creates is worth stating plainly:

> If SplitForge ever cannot time a race because a downstream integration is unavailable,
> that is a P0 bug in SplitForge, not an outage.

## Alternatives considered

| Alternative | Why not |
|---|---|
| Platform as system of record | No signal means no race. Directly contradicts [ADR-0008](0008-offline-first-operation.md) |
| Required at configuration time only | Still couples startup to a remote service, and startup happens at 5 a.m. on a cold morning |
| Sync as a separate external process | Legitimate, and possibly a later refactor. Keeping it in-workspace makes the outbox transactional with the data it describes |
| No integrations at all | Exports alone would work, but organizers reasonably want automation. Optional is the right answer, not absent |

## References

- [architecture.md § 5](../architecture.md#5-the-raceday-connect-boundary)
- [ADR-0008](0008-offline-first-operation.md)
