# ADR-0032: The service may speak IP, to this device and nothing else

- **Status:** Accepted
- **Date:** 2026-09-15
- **Supersedes:** — (refines how [ADR-0022](0022-the-service-never-waits-for-the-network.md) enforces [ADR-0021](0021-local-api-listens-on-a-unix-socket.md); both stand)

## Context

[ADR-0022](0022-the-service-never-waits-for-the-network.md) confined `splitforge-edge` with
`RestrictAddressFamilies=AF_UNIX`, so that the kernel, not review, enforced
[ADR-0021](0021-local-api-listens-on-a-unix-socket.md)'s rule that the API binds no port. It
named what that bought: *"answering 'could this process have talked to the LAN?' with a
kernel-level no."*

It also broke the clock check, and nothing noticed. The service stamps a
`DeviceClockState` on every read it writes, and learns it by running `chronyc -c tracking`.
The [2026-09-13 security review](../roadmap.md#security-review--2026-09-13) suspected the
unit stopped that from working, and that every read would be recorded as `unsynced` even on
a GPS-locked Pi. The M5 observation could not have caught it, because it used a stub daemon.

Observed on Debian bookworm, which Raspberry Pi OS is built on (systemd 252, chrony 4.3),
with the shipped unit and binary:

| Configuration | What `chronyc` got |
|---|---|
| Root, no sandbox | An answer |
| `splitforge`, no sandbox | An answer |
| `splitforge`, `ProtectSystem=strict` only | An answer |
| `splitforge`, `RestrictAddressFamilies=AF_UNIX` only | `Could not open connection to daemon` |
| The shipped unit | `/health`: `"measurement":"daemon_unreachable"`, `"state":null` |

`chronyd`'s Unix command socket lives in `/run/chrony`, which is `0700 _chrony`, and chrony
serves it only to root and its own user. Every other account reaches `chronyd` over UDP to
`127.0.0.1:323`, and `AF_UNIX` alone forbids that. The review also suspected
`ProtectSystem=strict`, and that turned out not to matter: the Unix socket was never
available to this account.

`SocketBindDeny=` looked like the way to keep *"binds no port"* while allowing IP. It is
silently ignored on this target. Debian's systemd is built without the BPF framework
(`-BPF_FRAMEWORK`), and a TCP listener opened under `SocketBindDeny=any` accepted connections.

## Decision

**The unit allows IPv4 and restricts IP traffic to this device:**

```ini
RestrictAddressFamilies=AF_UNIX AF_INET
IPAddressDeny=any
IPAddressAllow=localhost
```

`IPAddressDeny=` and `IPAddressAllow=` use cgroup socket filters, which this systemd does
support. Observed under them:

| | Result |
|---|---|
| `chronyc -c tracking` | An answer; `/health` reports `measured`, `ntp_synced` |
| TCP connection to an address off the device | Refused |
| A listener on `0.0.0.0`, connected to over `127.0.0.1` | Answers |
| The same listener, connected to over the device's network address | No connection |

**The guarantee ADR-0022 named is kept, and it is still the kernel's.** Nothing off the device
can reach a socket the service holds, and the service can reach nothing off the device.

**`AF_INET6` is not allowed.** `chronyc` reaches `chronyd` over IPv4, and nothing needs more.

**`chronyc` is bounded in time as well.** It gave up after 7 s against a daemon that received
the request and never replied, and failed in tens of milliseconds when the daemon was stopped or
blocked. `splitforge-timesource` stops it after 10 s regardless, because the edge waits for its
first sample before it starts reading.

**Milestone 3b widens `IPAddressAllow=`** to its reader's address, deliberately.
`apps/splitforge-edge/tests/unit_file.rs` asserts all three directives, so that change cannot
be silent, which is the review ADR-0022 wanted the first network access to get.

## Consequences

### What this makes easy

- Every read carries the clock state the daemon actually reports, which is what gates
  publication.
- Milestone 3b's change to the unit is one address, not a new address family.

### What this makes hard

- **The kernel no longer refuses a listener on loopback.** A defect or a dependency that opened
  a TCP port would be reachable by processes on this device, though not from off it.
  `crates/splitforge-api/tests/socket.rs` still refuses a listener in the API's source, and a
  local account that could reach it can already reach the Unix socket.
- The `systemd-analyze security` exposure level moves from 1.0 to 1.1.

### What we accept

- Enforcement now rests on a filter systemd applies at the cgroup, not on the absence of the
  socket family. That filter was observed working on the target's systemd, and `SocketBindDeny=`
  was observed not working. A future Raspberry Pi OS whose systemd has the BPF framework could
  add `SocketBindDeny=any` back as a second control.

## Alternatives considered

| Alternative | Why not |
|---|---|
| Keep `AF_UNIX` only | Every read stamped `unsynced` on a clock that was fine. That is wrong evidence, and it blocks publication |
| A separate, sandboxed unit that runs `chronyc` and writes its answer to a file the edge reads | Keeps the edge at `AF_UNIX`, at the cost of a second unit, a file contract, and deciding when a file is too old. The IP filter gives the same off-device guarantee with one unit |
| `SocketBindDeny=any` to keep "binds no port" | Silently ignored on this target's systemd, observed accepting a connection |
| Run the service in the `_chrony` group to use the Unix socket | chrony serves that socket to root and its own user only, and `/run/chrony` is `0700`, so a group grants nothing. Running the service *as* `_chrony` would put the timer and the time daemon under one account |
| Read the kernel's synchronization state with `adjtimex` instead of asking `chronyc` | Observed refused under `ProtectClock=yes`, even with no modes set, and the answer cannot tell GPS from NTP, which `DeviceClockState` records |

## References

- [ADR-0021](0021-local-api-listens-on-a-unix-socket.md): the API listens on a Unix socket
- [ADR-0022](0022-the-service-never-waits-for-the-network.md): the confinement this refines
- [Roadmap: Security review, 2026-09-13](../roadmap.md#security-review--2026-09-13)
- `deploy/splitforge-edge.service`, and `apps/splitforge-edge/tests/unit_file.rs`:
  `the_kernel_keeps_this_service_off_the_network`
- `crates/splitforge-timesource/src/lib.rs`: `CHRONYC_TIMEOUT`
