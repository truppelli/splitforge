# ADR-0012: Crate dependency rules are enforced by a test

- **Status:** Accepted
- **Date:** 2026-08-14
- **Resolves:** Q6

## Context

[ADR-0001](0001-rust-workspace.md) and the dependency table in
[architecture.md § 2](../architecture.md#dependency-rules) describe which crate may depend
on which. The rule that earns its keep is **`engine` must never depend on `llrp`** — it is
what makes a second reader protocol, a serial timing box, or a CSV import of somebody
else's reads a new adapter rather than a rewrite.

Until now that table was documentation. Documented architecture erodes in a predictable
way: not by a pull request that announces "this breaks the layering", but by one convenient
import at 11 p.m., each individually defensible, until the boundary exists only in a
diagram. The failure is invisible until the day someone tries to add a second reader and
discovers the engine has opinions about LLRP frames.

Milestone 1 is the right moment. The crates have just acquired real content, and the cost
of enforcing a boundary rises steeply once code has already crossed it.

## Decision

**The dependency table is duplicated as data in
`crates/splitforge-testkit/tests/dependency_rules.rs`, and violating it fails the test
suite.**

- Each package's `Cargo.toml` is parsed, and its workspace-internal dependencies are
  checked against an explicit allow-list
- The allow-list is exhaustive rather than wildcarded, including for `splitforge-cli` and
  `splitforge-edge`. Adding a crate to the workspace therefore forces a deliberate answer
  about what may depend on it, instead of quietly inheriting "everything"
- `engine`, `results`, and `api` are checked against `llrp` separately, in their own test,
  with a failure message that explains the rule rather than restating it
- `splitforge-domain` is additionally checked against a list of I/O crates by name, so the
  first convenient `tokio` import is caught before it grows a justification
- Dev-dependencies are held to the `llrp` rule but not to the full table: a test may reach
  for a helper that the shipped crate may not

## Consequences

### What this makes easy

- The architecture is now falsifiable. "Does this PR break the layering?" has an answer that
  does not depend on a reviewer's memory
- Failure messages name the crate, the offending dependency, and the document that explains
  why — so the test teaches the rule instead of merely blocking the change
- The rule table has exactly one authoritative copy in prose and one in code, and they are
  reviewed together

### What this makes hard

- Two places to update when the architecture genuinely changes: the table in
  `architecture.md` and the list in the test. That friction is the point — an architectural
  change should cost more than an import
- Only direct dependencies are checked. A transitive path through a permitted crate would
  pass, which is acceptable because the permitted crates are themselves constrained

### What we accept

That this is manifest-level enforcement, not module-level. It cannot stop a crate from
growing an internal layering problem, and it says nothing about whether `splitforge-engine`
is *well* designed — only that it cannot see a reader protocol. That is the boundary worth
mechanizing; the rest is review.

## Alternatives considered

| Alternative | Why not |
|---|---|
| `cargo deny`'s `bans.deny` with `wrappers` | Already in the toolchain and would work. Rejected on error messages: a build failure that says `bans.deny` teaches nobody why the rule exists, and this rule is one people need to understand rather than obey |
| Parsing `cargo metadata` in the test | More thorough — it sees the resolved graph — but shells out to `cargo` from inside a test, which risks contending on the package cache lock in CI. Reading manifests is hermetic |
| A CI-only shell script | Invisible to anyone running `cargo test` locally, so the first they hear of it is a failed CI run |
| Leave it as documentation | The status quo, and the one Q6 was raised to end |

## References

- [ADR-0001: Multi-crate Rust workspace](0001-rust-workspace.md)
- [ADR-0004: LLRP as the first reader protocol](0004-llrp-first-reader-adapter.md)
- [architecture.md § 2](../architecture.md#dependency-rules)
- [ci.md](../ci.md)
