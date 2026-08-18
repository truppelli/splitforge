# ADR-0021: The local API listens on a Unix socket, not on the network

- **Status:** Accepted
- **Date:** 2026-08-17
- **Deciders:** —
- **Resolves:** [Q5](../open-questions.md#q5-local-api-authentication-model)

## Context

SplitForge's first networked interface has been blocked since Milestone 0 on a question it
could not answer: how does a local API authenticate callers on an event LAN?

The environment is the hard part. [Threat model B2](../threat-model.md#2-trust-boundaries)
describes the network as *"untrusted. Event networks are shared, ad hoc, and often built by
whoever brought a switch."* There is no PKI, no directory, no certificate authority, and
frequently no Internet. [S2](../threat-model.md#security-risks) rates unauthorized LAN
access to the API as High.

The other half is the operator. Whatever is chosen has to work at 6 a.m. in the cold, from
a phone, by a volunteer. The threat model is explicit that this is a security property and
not a usability nicety: *"A timer that refuses to run because a security check failed has
caused the harm it was protecting against."* An authentication scheme that gets bypassed in
practice — the token pasted into a shared note, the TLS check disabled because a
self-signed certificate broke on race morning — is worse than a simple one that gets used.

Three shapes were on the table for two years of planning: a bearer token in local config,
mTLS, and a Unix socket with all remote access over SSH.

## Decision

**The local API listens on a Unix domain socket. It never binds a TCP port, and it performs
no authentication of its own.**

```text
/run/splitforge/api.sock     srw-rw----  splitforge:splitforge
```

Access control is the socket's file permissions, enforced by the kernel. Remote access is
SSH, which the Pi already runs, whose keys the operator already manages, and which is the
transport every other SplitForge operation already uses.

This is not "authentication deferred". It is the observation that **the authentication
problem is better solved by not being reachable.** A process that binds no port cannot be
reached by anything on the LAN, so S2 stops being a risk to mitigate and becomes a risk
that does not apply. There is no token to leak, rotate, or paste into a group chat, and no
certificate to expire the morning of an event.

Three constraints follow, and a pull request that breaks any of them breaks this ADR:

**No TCP listener, at any address, behind any flag.** Not `127.0.0.1`, which invites a
later "just for the console" widening to `0.0.0.0`. The absence of the code path is the
control.

**Health must be cheap enough to poll.** It answers from counts and aggregates —
`SELECT COUNT(*)`, the schema version, a `statvfs` — and never walks the journal. A health
endpoint whose cost grows with the length of the event becomes the outage it was added to
detect, and it is polled hardest exactly when the device is already struggling.

**The API is read-only and cannot stop journaling** ([S10](../threat-model.md#security-risks)).
It observes; it does not write, and the read path does not traverse it.

## Consequences

### What this makes easy

- Deployment. There is nothing to configure: no port, no token file, no certificate, no
  firewall rule. A misconfiguration that exposes the API is not possible because there is no
  setting that would do it.
- Reasoning about the trust boundary. "Who can call this?" has a checkable answer — whoever
  can open that file — rather than an answer that depends on a secret's whole lifecycle.
- Operating from a phone at 6 a.m., because it is the SSH the operator was going to use
  anyway.

### What this makes hard

- **A browser console is ruled out.** `splitforge-web` was sketched in
  [architecture § 7](../architecture.md#7-technology-choices) as a later addition consuming
  this API, and a browser cannot open a Unix socket. Building one requires a new ADR that
  supersedes this one, and that ADR will have to answer the authentication question this one
  side-steps — on its merits, with a real interface to design against, rather than
  speculatively now.
- Monitoring from another machine. A dashboard polling several checkpoints needs an SSH hop
  per device rather than an HTTP GET. At the scale this project supports — one reader, one
  checkpoint, one Pi — that is not yet a real cost.

### What we accept

- **This decision has an expiry date, and it is a browser.** The first serious request for a
  web console supersedes it. Recording it now is still worth it: the health endpoint has been
  blocked for two milestones on a question that did not need answering to build it.
- **File permissions are the whole control.** Anyone who can read that socket is fully
  trusted, so a second account on the Pi is a second operator. That is the same trust model
  SSH access already implies, so it adds no exposure — but it does mean the socket's mode
  and group are load-bearing and must be asserted, not assumed.
- **HTTP over a Unix socket is slightly unusual.** `curl --unix-socket` handles it and so
  does every language's HTTP client, but it is one more thing to explain in a runbook.
- **Axum's dependency tree is large** relative to one endpoint. It is accepted rather than
  hand-rolled because [architecture § 7](../architecture.md#7-technology-choices) already
  chose Axum at Milestone 0, and because replacing a hand-rolled server when Milestone 3
  adds reader state would cost more than it saved.

## Alternatives considered

| Alternative | Why not |
|---|---|
| Bearer token in local config | The token has a lifecycle — generated, distributed, stored, rotated, revoked — and every stage happens in a field with cold hands. On an untrusted LAN without TLS it also travels in the clear, so it needs a certificate too, and now there are two secrets |
| mTLS | Strongest of the three and genuinely unoperable here. Issuing and installing client certificates from a phone in a parking lot is not a thing that happens; what happens instead is that verification gets turned off |
| Bind `127.0.0.1` only | Better than `0.0.0.0` and still a port. It leaves the listener code in place, so widening it later is a one-line change nobody reviews as a security decision. It also gains nothing a socket does not already give |
| Token *and* socket | Belt and braces where the braces do nothing. The kernel already decided who may connect; a token checked afterwards adds a lifecycle without adding a boundary |
| No API at all; extend the CLI | Tempting, and nearly right today — `splitforge status` already answers most of it from the database. It stops being right at Milestone 3, when reader connection state lives in the running process and no one-shot command can see it |

## References

- [Q5 — Local API authentication model](../open-questions.md#q5-local-api-authentication-model)
- [Threat model — S2, S10, B2](../threat-model.md#2-trust-boundaries)
- [Architecture § 7 — technology choices](../architecture.md#7-technology-choices)
- [ADR-0002](0002-raspberry-pi-target.md) — the target is Linux, which is what makes a Unix socket available at all
- [ADR-0008](0008-offline-first-operation.md) — no cloud service on any race-day path
