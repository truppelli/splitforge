# ADR-0004: LLRP as the first reader protocol

- **Status:** Accepted
- **Date:** 2026-08-14

## Context

SplitForge must talk to RFID readers. The RFID timing market has no single dominant
integration path: vendors ship proprietary TCP protocols, serial interfaces, HTTP
callbacks, and vendor SDKs — often several for the same device.

LLRP (Low Level Reader Protocol) is an EPCglobal standard for networked RFID readers. It
is asynchronous and report-driven, which matches how a timing system wants to consume
data: subscribe, then receive reads as they happen rather than polling.

## Decision

Implement **LLRP as the first reader protocol**, in a dedicated `splitforge-llrp` crate
behind the `ReaderProvider` port defined in `splitforge-reader`.

The engine consumes normalized `RawRead` values and must never depend on
`splitforge-llrp`.

Requirements on the adapter:

- Handle **both** `UTCTimestamp` and `Uptime` timestamp parameters explicitly. An uptime
  value must never be interpreted as a date
  ([clock discipline § 6](../clock-and-time-discipline.md#6-llrp-timestamp-specifics))
- Parsing returns errors; it never panics. Malformed input is expected, not exceptional
- Bounded frame sizes and allocation limits — a hostile or broken reader must not exhaust
  memory
- Reconnect with bounded, jittered backoff
- Raw protocol capture behind an explicit diagnostic flag

## Consequences

### What this makes easy

- One protocol covers readers from several vendors
- Network-based, so the reader can sit at the mat while the Pi sits somewhere dry
- Asynchronous reports fit the architecture without polling
- A documented standard means the parser can be written and tested against captures
  before hardware arrives

### What this makes hard

- Binary protocol parsing against untrusted input is the highest-risk code in the project
- LLRP is large; a useful subset must be identified rather than implementing the spec
- Vendor implementations vary in ways documentation does not predict
- **LLRP has no authentication.** Anything on the LAN can impersonate a reader — see
  [threat model S3](../threat-model.md#security-risks)

### What we accept

That LLRP support does not equal support for any particular reader. The support matrix in
[hardware-support.md](../hardware-support.md) stays empty until a physical device is
tested, and Milestone 3 is hard-gated on having one
([Q9](../open-questions.md#q9-first-reader-model)).

We also accept that LLRP will not be the last protocol. The port exists precisely so that
serial timing boxes, barcode scanners, manual entry, and file import can arrive later as
peers, not as special cases.

## Alternatives considered

| Alternative | Why not |
|---|---|
| Vendor SDK first | Fastest path to one working reader, and a dead end for every other reader. Also usually means linking C libraries into the highest-risk code path |
| Serial timing box protocols | Common in the field, but per-vendor and undocumented. A reasonable *second* adapter |
| HTTP webhook readers | Simple, but requires the reader to push to us and puts an HTTP server on the read path. Worse failure semantics |
| Wait and support several at once | Guarantees none of them work well. One protocol, one reader, proven for a full event, first |

## References

- [hardware-support.md](../hardware-support.md)
- [ADR-0001](0001-rust-workspace.md) — the boundary that keeps this replaceable
