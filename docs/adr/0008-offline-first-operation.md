# ADR-0008: Offline-first — no cloud service on any race-day path

- **Status:** Accepted
- **Date:** 2026-08-14

## Context

Races happen where races happen: trailheads, forest tracks, closed roads, park loops,
mountain passes. Connectivity at those places ranges from poor to absent, and the moments
it fails are correlated with the moments it matters — a few thousand spectators arriving
with phones is itself a reason the network stops working.

Meanwhile the read path has a hard real-time property that most software does not: a
runner crosses the mat exactly once. There is no retry. If the read is not recorded in
that instant, the evidence is gone, and no amount of later reconnection recovers it.

## Decision

**SplitForge must complete an entire event with no Internet connection.** This is a
correctness requirement, not a resilience feature.

Concretely:

- No cloud service, remote API, or third-party dependency on any path required to record
  a read, derive a timing event, generate a result revision, or export results
- No license check, telemetry call, account verification, or credential refresh at startup
  or during an event
- All configuration and all event data live on the local device
- Connectivity is used only for: optional outbound sync, software updates, remote
  diagnostics, and post-race publication — every one of which is deferrable
- Absence of connectivity produces no warning, error, or degraded mode. It is the
  **expected** operating condition

The verification is behavioral: **a full event timed with the network interface down must
produce identical results and byte-identical exports to the same event timed online.**

## Consequences

### What this makes easy

- The timer works anywhere, which is the entire point
- Race-day failure modes shrink to things physically present at the checkpoint
- No outage anywhere else in the world can affect an event in progress
- Security surface shrinks — a device with no outbound dependencies is a smaller target
- Costs stay at zero for an organizer running a village fun run

### What this makes hard

- No live results, no remote monitoring, no centralized fleet management without
  deliberate opt-in work
- Software updates are a manual, pre-event activity
- **Clock discipline becomes a genuine engineering problem**, because offline-first
  removes NTP — the single largest technical consequence of this decision, addressed in
  [clock and time discipline](../clock-and-time-discipline.md)
- Support is harder: no logs to inspect remotely, so diagnostic bundles must be
  self-contained and exportable to a USB stick

### What we accept

Building and testing everything twice — connected and disconnected — and treating the
disconnected case as primary. Features that only work online are permitted, but they may
never become load-bearing, and a reviewer's first question about any new dependency is
"does this work with the cable out?"

## Alternatives considered

| Alternative | Why not |
|---|---|
| Cloud-first with offline cache | The industry default, and it fails at the trailhead. Cached-mode code paths get less testing precisely because they run least often in development |
| Offline mode as a fallback | Same problem stated more politely. The rarely exercised path is the one that breaks |
| Require a local network but allow cloud | A LAN is needed for the reader anyway, but the moment cloud is on the read path the guarantee is gone |
| Local-first sync (CRDTs etc.) | Interesting for multi-device checkpoints later. Massive complexity for a single-device deployment, and out of scope |

## References

- [architecture.md § 1](../architecture.md#1-system-context)
- [ADR-0006](0006-optional-outbound-integrations.md)
- [clock-and-time-discipline.md](../clock-and-time-discipline.md)
