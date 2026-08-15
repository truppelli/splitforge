# ADR-0013: The MSRV is a tested floor, not a compatibility promise

- **Status:** Accepted
- **Date:** 2026-08-14
- **Resolves:** Q13

## Context

The workspace pinned `rust-version = "1.85"` from Milestone 0. Nobody decided on 1.85; it
was simply written down, and then defended by inertia.

Milestone 1 turned that into a bill. `time` >= 0.3.47 requires Rust 1.88, so the workspace
held at `time` 0.3.45 — which carries
[RUSTSEC-2026-0009](https://rustsec.org/advisories/RUSTSEC-2026-0009), a stack exhaustion in
its RFC 2822 parser. SplitForge could not reach the vulnerability (it parses RFC 3339
exclusively), so the advisory was accepted in `deny.toml` and `.cargo/audit.toml` with
revisit conditions attached.

That was defensible and uncomfortable. A project whose entire claim is that its records can
be trusted was carrying a known-vulnerable dependency in order to protect a version number
nobody had chosen on purpose. The next advisory might not have been so conveniently
unreachable.

The real question underneath was never "1.85 or 1.88". It was **who the MSRV is a promise
to**, because that determines what the promise is worth:

- **crates.io consumers?** There are none. Every crate here is `publish = false`
- **Distribution packagers?** Not yet, and not soon. SplitForge is pre-alpha with no
  releases. If a distribution ever packages it, that is a new constraint and a new decision
- **The Raspberry Pi?** No. [ADR-0002](0002-raspberry-pi-target.md) settled that the Pi is a
  **deployment** target, not a build host — binaries are cross-compiled to
  `aarch64-unknown-linux-gnu` from a developer machine or from CI. The Pi's toolchain, or
  lack of one, is irrelevant
- **Contributors?** Genuinely, yes — but a Rust released over a year ago is not a barrier to
  anyone using `rustup`

So the MSRV was protecting nobody, and costing something concrete.

## Decision

**The MSRV is the oldest toolchain CI actually tests against. It is a tested floor, not a
compatibility promise, and it moves when holding it would cost more than it protects.**

Effective immediately, `rust-version = "1.88"`, and `time` upgrades to 0.3.55.

It **moves** when:

1. **A security advisory cannot be cleared without it.** This is not a judgment call and
   does not wait for a milestone boundary. An accepted advisory whose only justification is
   the MSRV is a bug in the MSRV
2. **A dependency worth having requires it** — deliberately, in its own pull request, with
   the reason in the commit message

It **does not move** when:

- A newer language feature would merely be tidier. Let-chains reached stable in 1.88 and are
  now available; that was never a reason to raise anything
- A dependency's *latest* release requires it but the current release is fine and supported

Raising it is a one-line change to `[workspace.package]` plus the CI job's toolchain
version. It does **not** need a new ADR each time — this one is the policy. What it needs
is a sentence in the pull request saying which of the two reasons above applied.

The MSRV job runs `cargo check --workspace --all-features --all-targets`, so a
dev-dependency cannot raise the floor unnoticed.

**Revisit this ADR** if SplitForge is ever packaged by a distribution, or published to
crates.io. Both would create a constituency that does not exist today, and the answer would
likely change.

## Consequences

### What this makes easy

- Security patches land by upgrading rather than by writing a justification for not
  upgrading. `deny.toml`'s ignore list is empty again, and staying empty is now the default
- The MSRV has a reason, so the next argument about it is short
- Contributors get the language as it is, not as it was eighteen months ago

### What this makes hard

- Anyone on a pinned older toolchain must update. Given `rustup`, this is a one-line
  inconvenience
- The floor will drift upward over time, because the second trigger has no lower bound.
  Accepted: a floor that never moves is the situation this ADR exists to end

### What we accept

That "tested floor, not a promise" means SplitForge offers no MSRV guarantee at all, and
should not be treated as offering one. That is honest. An MSRV promise that gets broken the
first time it is inconvenient is worse than no promise, and an MSRV promise that is *kept*
at the cost of shipping known-vulnerable dependencies is worse still.

We also accept that this leaves [ADR-0010](0010-time-crate-for-timestamps.md) with a
"What we accept" section describing a cost that no longer exists. ADR-0010's *decision* —
the `time` crate, UTC everywhere — is unchanged, so it is not superseded, and per the
[ADR process](README.md#process) it is not edited after acceptance. This ADR is the record
that the cost was retired.

## Alternatives considered

| Alternative | Why not |
|---|---|
| Keep 1.85, keep the advisory exception | The exception was sound on its facts — RFC 2822 is genuinely unreachable here. But it protected a number nobody chose, and the reasoning would have to be re-litigated at every advisory. It also stops being true the moment anything here parses an HTTP date |
| A fixed policy such as "stable minus two releases" | Sounds rigorous, and would force churn on a schedule rather than for a reason. The floor should move when something needs it to, not every twelve weeks |
| Track the Rust version in Debian stable or Raspberry Pi OS | The obvious rule for a project deployed to a Pi — and wrong here, because [ADR-0002](0002-raspberry-pi-target.md) means the Pi never compiles anything. It would import a constraint from a machine that does not build the code |
| No `rust-version` field at all | Loses the CI job, and with it any signal about what actually builds. The floor being low-value is not a reason to stop measuring it |

## References

- [ADR-0002: Raspberry Pi target](0002-raspberry-pi-target.md) — the Pi is a deployment
  target, not a build host
- [ADR-0010: The `time` crate for timestamps](0010-time-crate-for-timestamps.md) — recorded
  the cost this ADR retires
- [ci.md](../ci.md) — the MSRV job and the accepted-advisory rules
- [RUSTSEC-2026-0009](https://rustsec.org/advisories/RUSTSEC-2026-0009)
