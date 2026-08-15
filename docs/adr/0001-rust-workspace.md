# ADR-0001: Multi-crate Rust workspace

- **Status:** Accepted
- **Date:** 2026-08-14

## Context

SplitForge mixes concerns with very different testing characteristics: binary protocol
parsing against untrusted input, hard durability guarantees, pure scoring rules, an HTTP
API, and optional outbound integrations. Bundled into one binary, these become mutually
entangled — scoring logic ends up importing a database handle, the protocol parser ends up
reachable from the API, and nothing can be tested without standing up everything.

The specific coupling we must prevent is the timing engine knowing anything about RFID
protocols. If it does, a second reader type is a rewrite instead of an adapter.

## Decision

Structure SplitForge as a Cargo workspace of small, single-responsibility crates, with
explicit dependency rules documented in
[architecture.md § 2](../architecture.md#dependency-rules).

`splitforge-domain` sits at the root and depends on nothing else in the workspace. It
defines types, invariants, and **port traits**. Adapters (`storage`, `llrp`, `sync`,
`simulator`) implement those traits. `splitforge-edge` is the only crate that knows which
concrete implementations exist.

Two rules are load-bearing:

1. `splitforge-engine` must never depend on `splitforge-llrp`.
2. `splitforge-domain` takes no I/O dependencies — no database, HTTP, async runtime, or
   filesystem.

## Consequences

### What this makes easy

- Scoring rules are testable as pure functions with no fixtures
- The simulator is architecturally identical to a real reader, so tests exercise the real
  code path
- A second reader protocol is a new crate, not a refactor
- Compile times stay reasonable — a change to the LLRP parser does not rebuild the engine

### What this makes hard

- More boilerplate: 13 `Cargo.toml` files, version coordination, wiring at the root
- Cross-crate refactors touch more files
- The trait indirection is a real cost when reading unfamiliar code

### What we accept

Ceremony now in exchange for the ability to add hardware support later without touching
timing logic. For a project whose entire value proposition is trustworthy timing data,
protecting the engine from protocol churn is worth the overhead.

## Alternatives considered

| Alternative | Why not |
|---|---|
| Single binary crate | Fastest to start, but nothing prevents the engine importing the protocol parser. That coupling is the thing we most need to avoid |
| Two crates (lib + bin) | Better, but does not separate protocol parsing from scoring, which are the two areas with the most different testing needs |
| Workspace with fewer, larger crates | Reasonable, and possibly where this lands after real use. Starting fine-grained makes merging easy; starting coarse makes splitting hard |

## References

- [architecture.md](../architecture.md)
- [ADR-0004](0004-llrp-first-reader-adapter.md) — the boundary this structure protects
