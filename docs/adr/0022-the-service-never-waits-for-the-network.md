# ADR-0022: The service never waits for the network and never stops restarting

- **Status:** Accepted
- **Date:** 2026-08-18

## Context

[ADR-0002](0002-raspberry-pi-target.md) settled that SplitForge deploys as a single systemd
service with `Restart=always`. Writing the unit turned out to require three more answers,
each of which looks like packaging trivia and is actually an availability decision:

1. **What does it wait for on boot?**
   [Architecture § 6](../architecture.md#6-deployment) said
   `After=network-online.target`, which was written before the offline-first commitment had
   teeth. A checkpoint has no DHCP server, no Internet, and frequently no switch. On a Pi
   with an unplugged Ethernet cable, `network-online.target` is not reached until
   `NetworkManager-wait-online` times out — 90 seconds by default — and a unit that
   `Wants=` it either boots 90 seconds late or does not start at all.

2. **What happens after it crashes repeatedly?** `Restart=always` on its own is not what it
   sounds like. systemd's default rate limit stops a unit *permanently* after 5 starts
   within 10 seconds, and the state it lands in is `failed`, not `activating`.

3. **What can a defect in it reach?** The device is physically accessible to strangers
   ([S8](../threat-model.md#security-risks)) and shares a network with equipment nobody
   vetted. [ADR-0021](0021-local-api-listens-on-a-unix-socket.md) states that the API binds
   no port, and enforces it with a test that reads the crate's own source — which cannot see
   a dependency.

The governing principle is already written down, in the threat model: *"A timer that refuses
to run because a security check failed has caused the harm it was protecting against."* All
three answers below are that sentence applied to a unit file.

## Decision

**The service is ordered after the network but never depends on it, never stops restarting,
and is confined so that the kernel — not review — enforces ADR-0021.**

```ini
After=network.target time-sync.target      # ordering only; no Wants=, no Requires=
StartLimitIntervalSec=0                    # never give up
Restart=always
RestrictAddressFamilies=AF_UNIX            # ADR-0021, enforced by the kernel
UMask=0007                                 # nothing it writes is world-readable
```

**No network dependency.** An `After=` on a unit that is not in the boot transaction imposes
no delay whatsoever, so this orders the service after the network *when the network is
coming up* and starts it immediately when it is not. `network.target` rather than
`network-online.target`, because the latter is the one that blocks. This is a **correction**
to architecture § 6, made deliberately: the original line would have produced a 90-second
boot delay on exactly the network a checkpoint has.

Reader connectivity is not a startup concern either. Milestone 3 specifies reconnection
after cable removal, reader reboot, and Wi-Fi interruption — a service that retries is
already required, so gating startup on the same network buys nothing and costs a boot.

**Never give up restarting.** `StartLimitIntervalSec=0` removes the rate limit. Whatever
made the service crash is usually transient — a disk hiccup, a reader sending something
unexpected — and a timer that has given up records nothing for the rest of the event, with
no indication beyond a `failed` unit that nobody is watching at 8 a.m. The cost is a crash
loop that fills the journal, which is visible and recoverable in a way a stopped timer is
not.

**Confinement, with two directives that are load-bearing rather than decorative.**
`RestrictAddressFamilies=AF_UNIX` means a TCP listener reaching the deployment — from this
code or from a dependency — cannot open a socket at all. `UMask=0007` exists because the
first real run of this service under systemd created `event.db` and its write-ahead sidecar
**world-readable**: participant names and every raw read in plain text, on a device where a
second account is a realistic thing to find.

## Consequences

### What this makes easy

- Booting with no network at all, which is the normal case at a checkpoint.
- Surviving a transient crash without an operator noticing, at any hour of the event.
- Answering "could this process have talked to the LAN?" with a kernel-level no.

### What this makes hard

- **Milestone 3 must widen `RestrictAddressFamilies` to add `AF_INET`.** That is the point:
  the reader connection is the first thing that legitimately needs the network, and it
  should arrive as a reviewed change to a security directive rather than as a side effect of
  adding a dependency. `apps/splitforge-edge/tests/unit_file.rs` fails until the test is
  updated too, so the change cannot be silent.
- A genuinely broken install crash-loops forever instead of stopping. `systemctl status`
  and the journal both say so plainly, and `Restart=always` was never going to help a
  binary that cannot start.
- The service's own files are group-readable but not group-writable — SQLite asks the
  kernel for `0644` and a umask can only remove bits. An operator editing a database the
  *service* created has to run the CLI as the service account. The ordinary flow avoids it,
  because the operator configures the event with the CLI first and owns the file already.

### What we accept

- **Hardening we cannot fully validate here.** The syscall filter, the empty capability
  bounding set, and `ProcSubset=pid` were verified by starting the service under real
  systemd and exercising the endpoint, a `SIGKILL` restart, and a clean stop — but in a
  container on x86-64, not on a Pi's kernel. `systemd-analyze verify` is clean and
  `systemd-analyze security` scores 1.0. What remains is a hardware check, and it belongs
  with the rest of the Milestone 5 work that does.
- **No watchdog.** `WatchdogSec=` needs `Type=notify` and an `sd_notify` implementation in
  the binary. The health endpoint already answers the same question over the socket
  (ADR-0021), and an external poll is a smaller change than teaching the service to talk to
  systemd. Revisit if a hang — as opposed to a crash — is ever observed.
- **No socket activation.** The service creates its own socket, which keeps the code the
  same whether it runs under systemd or from a shell during development.

## Alternatives considered

| Alternative | Why not |
|---|---|
| `Wants=network-online.target` | The unit the architecture originally named. It converts "no network today" into a 90-second boot delay, or a service that never starts, on precisely the network a checkpoint has |
| Leave systemd's default start rate limit | The default is designed to stop a broken unit from consuming a machine. Here the machine has one job, and a stopped timer is a worse outcome than a noisy one |
| `Type=notify` with a watchdog | Needs `sd_notify` in the binary and a libsystemd dependency or a hand-rolled implementation. The health endpoint answers the same question and already exists |
| `DynamicUser=yes` | Elegant, and wrong for data that outlives the service. A dynamic UID makes the event database's ownership unstable across restarts, and the database is the one artifact that must survive everything |
| Skip the hardening and ship a minimal unit | The two directives that matter — `RestrictAddressFamilies` and `UMask` — are not hygiene. One enforces an ADR the kernel can enforce better than a test; the other fixed a real world-readable database found by running the thing |

## References

- [ADR-0002](0002-raspberry-pi-target.md) — the Pi target and `Restart=always`
- [ADR-0008](0008-offline-first-operation.md) — no cloud service on any race-day path
- [ADR-0018](0018-write-ahead-sidecar-journal.md) — the sidecar whose permissions `UMask=` fixes
- [ADR-0021](0021-local-api-listens-on-a-unix-socket.md) — the API binds no port
- [Threat model — S8, S10](../threat-model.md#security-risks)
- [docs/deployment.md](../deployment.md) — installing the unit, and what was observed running it
