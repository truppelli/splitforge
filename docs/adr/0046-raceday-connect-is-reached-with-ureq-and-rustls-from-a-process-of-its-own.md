# ADR-0046: RaceDay Connect is reached with ureq and rustls, from a process of its own

- **Status:** Accepted
- **Date:** 2026-09-27
- **Extends:** [ADR-0006](0006-optional-outbound-integrations.md), [ADR-0039](0039-raceday-connect-publishes-what-splitforge-derived.md)
- **Constrained by:** [ADR-0032](0032-the-service-speaks-ip-to-this-device-only.md)

## Context

[ADR-0039](0039-raceday-connect-publishes-what-splitforge-derived.md) built the translation from
what SplitForge derived to what RaceDay Connect reads, and left the HTTP client to its own
decision, because **choosing it adds TLS to the dependency tree**. This is that decision.

Three things constrain it.

**The timer cannot open a connection off the device, by design.**
[ADR-0032](0032-the-service-speaks-ip-to-this-device-only.md) ships `splitforge-edge.service`
with `IPAddressDeny=any` and `IPAddressAllow=localhost`. The kernel refuses any connection it
tries to make to another host, and `unit_file.rs` asserts the directives. ADR-0039 § 7 put the
shipper in `splitforge-edge`. Built into the timer's process, it would be refused by the timer's
own unit. The only way to change that is to widen the sandbox of the process that records reads,
for the one feature ADR-0006 says must never matter to recording them.

**Only `splitforge-sync` may reference an external service** (ADR-0006). The client's code goes
there, beside the contract it sends.

**The dependency policy is a gate, and so is the Pi.** `deny.toml` allows a fixed list of
licenses. CI cross-builds the whole workspace for `aarch64-unknown-linux-gnu` with a
cross-compiler and nothing else: no cmake, no assembler toolchain beyond `gcc`.

The candidates were measured on 2026-09-27, each as a prototype making one HTTPS request, in the
CI image with the repo's `deny.toml` and `.cargo/config.toml`:

| | ureq 3.4.2, `rustls` (default) | ureq 3.4.2, `rustls-no-provider` + `platform-verifier`, rustls with ring | reqwest 0.13.5, `rustls` + `blocking` |
|---|---|---|---|
| Crates in the tree | 28 | 24, 15 of them new to the workspace | 110, including tokio's runtime, hyper and h2 |
| Crypto | ring | ring | aws-lc-rs (`aws-lc-sys`, built from C) |
| Trusted roots | bundled `webpki-roots` | the system's store | the system's store |
| `cargo deny check licenses` | **fails**: `webpki-roots` is CDLA-Permissive-2.0 | passes | passes |
| aarch64 release binary | 3.1 MB | 3.0 MB | 5.9 MB |
| Expired certificate | refused: *"certificate expired"* | refused: *"certificate expired"* | refused: *"error sending request"* |
| Self-signed certificate | refused: *"UnknownIssuer"* | refused: *"UnknownIssuer"* | refused: *"error sending request"* |
| No system certificates | — | refused: *"No CA certificates were loaded from the system"* | — |

## Decision

**1. The client is [ureq](https://crates.io/crates/ureq) 3, blocking, with rustls and the ring
provider, trusting the system's certificate store.** Exactly:

```toml
ureq = { version = "3.4.2", default-features = false, features = ["rustls-no-provider", "platform-verifier"] }
rustls = { version = "0.23.45", default-features = false, features = ["ring", "std", "tls12", "logging"] }
```

The ring provider is installed once, when the process starts. It lives in `splitforge-sync`, and
nothing else in the workspace names ureq or rustls.

**2. The shipper is a process of its own**, a second binary in the composition root's package,
`splitforge-ship`, with a systemd unit of its own. That unit may reach the network. The timer's
unit is not changed: `IPAddressDeny=any` stays, and `unit_file.rs` goes on asserting it. The
timer does not wait for the shipper, start it or depend on it. The boundary test
(`read_path_boundary.rs`) gains the shipper's binary as a place that may name `splitforge_sync`.

**3. Certificates are checked against the system's store, and nothing is pinned.** Raspberry Pi
OS installs `ca-certificates` and updates it with the rest of the system. Bundled roots would go
stale in a binary nobody rebuilds. Pinning RaceDay Connect's key would break delivery on the day its
certificate rotates, which nobody at a race could fix. **Verification is never switched off**,
and no setting offers to.

**4. Every request is bounded.** There is a timeout on connecting and one on the whole request,
and redirects are not followed. The first numbers are 5 s to connect and 15 s in all. They are
a starting point, not a measurement. A 3xx means the endpoint is misconfigured. It is reported
and retried like a 5xx, so correcting the configuration resumes delivery without losing
anything. In the prototype, with `timeout_connect`, `timeout_global` and `max_redirects(0)`, a
redirect came back as `302 Found` without being followed, and an address nothing answered on
failed with *"timeout: connect"*.

**5. A TLS failure is retried, never refused.** ADR-0039's table sends *"no answer at all"* to
retry, and a handshake that fails is that. This matters most for the reason a handshake fails on
a Pi. **A Pi without a real-time clock boots with the time it was shut down**, and until it
synchronises, every certificate checks against the wrong day. Refusing would throw away a
race's results because the clock was late. So these are retried with the usual backoff, and
reported with the error rustls gives, which names the certificate time it checked against. A
certificate the store does not trust is retried too, because a captive portal on race-day Wi-Fi
looks exactly like one. Retrying never sends anything to a server that has not proved it is
RaceDay Connect.

**6. HTTP/1.1, IPv4 and IPv6, and no proxy support.** Nothing needs HTTP/2. A proxy can be
added when a site needs one.

## Consequences

### What this makes easy

- **The timer's sandbox is unchanged.** The guarantee ADR-0032 kept, that the process holding
  the evidence can reach nothing off the device, holds with RaceDay Connect running. A stalled
  handshake, a slow server or a bug in the shipper cannot touch the process that records reads,
  because it is not in that process.
- **A small tree, all of it Rust except ring's C**, which the cross gate already compiles for the
  Pi. It adds no async runtime, because the shipper sends one request at a time.
- **Errors say what went wrong.** An operator reading *"certificate expired: verification time
  …"* can see that the clock is wrong, which *"error sending request"* would hide.

### What this makes hard

- **Two processes share the event database.** The shipper reads what the timer writes. How it
  records what it has sent without taking the write lock the read path appends through is the
  outbox's decision, the next one ADR-0039 § 7 names. SQLite's WAL lets a reader run beside a
  writer without blocking it, and that is the property it should keep.
- **Two units to install and watch.** `deployment.md` gains the second one, and `doctor` should
  say whether the shipper is running once it exists.
- **The Pi needs `ca-certificates`.** Raspberry Pi OS installs it. Without it, every request
  fails with the message in the table above, which says so.

### What we accept

- **The system's store decides who is trusted.** A root the operating system trusts, RaceDay
  Connect's client trusts. That is the usual trade, and it keeps working after rotations and
  revocations the project never hears about.
- **A late clock delays results.** It does not lose them. They are sent once the clock is
  right, which the rest of the timer needs anyway ([clock discipline](../clock-and-time-discipline.md)).
- **ring is maintained by a small team.** rustls also supports aws-lc-rs, which would cost cmake
  in the cross gate. Moving to it would change one feature, not the client.

## Alternatives considered

| Alternative | Why not |
|---|---|
| reqwest | 110 crates to 24, including tokio's runtime, hyper and h2 for one request at a time, aws-lc-rs built from C for the Pi, and errors that do not say why a certificate was refused |
| ureq with its default `rustls` feature | Bundles `webpki-roots`, whose CDLA-Permissive-2.0 license `deny.toml` does not allow, and whose roots go stale in a binary nobody rebuilds |
| Allow CDLA-Permissive-2.0 in `deny.toml` | A license decision made to avoid a better design: the system's store is more current anyway |
| The shipper inside `splitforge-edge`, with `IPAddressAllow=` widened to RaceDay Connect | Widens the sandbox of the process that records reads for the feature that must never matter to it. A host name also resolves to addresses that change, so the allow list would drift |
| hyper and hyper-rustls directly | More code to write for the same request, and the async runtime reqwest brings |
| `curl` or `native-tls` with OpenSSL | C dependencies to cross-compile and a system library to match on the Pi, where rustls needs neither |
| Pin RaceDay Connect's certificate or key | Breaks on rotation, on race day, with nobody able to fix it |

## References

- [ADR-0006](0006-optional-outbound-integrations.md): integrations are optional, and only `splitforge-sync` reaches outside
- [ADR-0032](0032-the-service-speaks-ip-to-this-device-only.md): the timer speaks IP to its own device only
- [ADR-0039](0039-raceday-connect-publishes-what-splitforge-derived.md): the translation, the delivery table, and § 7's next slice
- [Architecture § 5](../architecture.md#5-the-raceday-connect-boundary): the boundary
- `deny.toml`: the license policy
