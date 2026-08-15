# ADR-0010: The `time` crate for all timestamps

- **Status:** Accepted
- **Date:** 2026-08-14
- **Resolves:** Q2

## Context

SplitForge's entire product is timestamps. The sketches in the Milestone 0 documents used
`DateTime<Utc>` for readability, which was never a decision — it was placeholder prose, and
[clock-and-time-discipline.md](../clock-and-time-discipline.md) explicitly deferred the
choice.

Both `time` and `chrono` are correct, maintained, and capable of everything SplitForge
needs. The differences that matter here are narrow:

- **Surface area.** `time` is smaller. Fewer ways to accidentally produce a local-time value
  in a system whose entire premise is that stored times are unambiguous
- **Local time in threaded processes.** `time` deliberately refuses to read the local
  timezone from a process that may have other threads, because the underlying C call is not
  thread-safe. That is an inconvenience on a desktop and a feature in a service
- **Security history.** `chrono`'s past advisory was about exactly this problem. `time`'s
  response to it was to make the unsafe thing unavailable

Against that, `chrono` has wider ecosystem familiarity, and a contributor is more likely to
have used it.

## Decision

**SplitForge uses the `time` crate. `OffsetDateTime` in UTC is the only wall-clock type
that crosses a crate boundary.**

- Every persisted timestamp is UTC. There is no local-time value in the domain, in storage,
  or in any export
- Serialization is RFC 3339 via `time::serde::rfc3339`, so JSON output is unambiguous and
  sorts correctly as text
- Storage is `INTEGER` microseconds since the Unix epoch, converted with `div_euclid` so
  pre-epoch values round consistently rather than toward zero
- **Interval arithmetic never uses wall-clock time.** Durations between reads come from
  monotonic values (`received_at_monotonic_ns`), because a wall clock that steps mid-race
  is an expected event on a Pi 3 with no battery-backed RTC
- Local time exists only at the point of display, and is the operator's formatting
  preference rather than a stored fact

## Consequences

### What this makes easy

- A timestamp anywhere in the system means the same thing, with no timezone attached to
  reason about
- The clock-discipline model has somewhere honest to live: reader time, device receipt
  time, and monotonic time are three different values with three different types of trust,
  and none of them is silently coerced into another
- Exports are stable text, which matters for the "byte-identical with integrations enabled
  and disabled" criterion in Milestone 6

### What this makes hard

- Contributors who know `chrono` have a small amount of relearning to do
- Any dependency that speaks `chrono` needs a conversion at the boundary
- Formatting for humans is more explicit than `chrono`'s `format` shortcuts

### What we accept

`time` currently constrains the MSRV story: the newest releases require a Rust newer than
the 1.85 this workspace pins, so the workspace holds at `time` 0.3.45. That is a real cost
and worth stating plainly. Raising the MSRV is a separate decision, taken when something
needs it rather than because a version number moved.

We also accept that "no local time anywhere" will occasionally feel pedantic — an operator
reading `2026-04-11T08:00:00Z` for an 8 a.m. race start is doing arithmetic a human should
not have to do. That belongs in the display layer, not the data.

## Alternatives considered

| Alternative | Why not |
|---|---|
| `chrono` | Perfectly workable. Larger surface, and the local-time footgun is one this project would rather not carry into a system whose whole claim is that its timestamps are trustworthy |
| `std::time` only | `SystemTime` has no calendar arithmetic and no serialization format worth exposing. A race needs to say "08:17:32.4", not "1775894252.4" |
| Store an offset alongside every timestamp | Solves a problem SplitForge does not have. A checkpoint does not move between timezones mid-event |

## References

- [clock-and-time-discipline.md](../clock-and-time-discipline.md)
- [ADR-0002: Raspberry Pi target](0002-raspberry-pi-target.md)
- [architecture.md § 7](../architecture.md#7-technology-choices)
