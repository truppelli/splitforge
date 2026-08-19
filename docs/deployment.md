# Deploying the SplitForge service

How to install and run `splitforge-edge` as a systemd service. The unit and the account it
runs as live in [`deploy/`](../deploy).

> **Scope.** This is the *service* installation. The Raspberry Pi field guide — external
> power, wired Ethernet first, race-day Wi-Fi as a known risk — is a separate open item in
> [Milestone 5](roadmap.md#milestone-5--field-reliability), and it is deliberately not
> written from a desk. See [hardware-support.md](hardware-support.md).

## What gets installed

| File | Destination | What it is |
|---|---|---|
| `target/aarch64-unknown-linux-gnu/release/splitforge-edge` | `/usr/local/bin/splitforge-edge` | The service |
| `target/aarch64-unknown-linux-gnu/release/splitforge` | `/usr/local/bin/splitforge` | The operator CLI |
| `deploy/splitforge.sysusers.conf` | `/usr/lib/sysusers.d/splitforge.conf` | The unprivileged account the service runs as |
| `deploy/splitforge-edge.service` | `/etc/systemd/system/splitforge-edge.service` | The unit |

systemd creates the two directories itself, from `StateDirectory=` and `RuntimeDirectory=`
in the unit — there is nothing to `mkdir` and nothing to `chown`:

```text
/var/lib/splitforge/   0750 splitforge:splitforge   event database + write-ahead sidecar
/run/splitforge/       0750 splitforge:splitforge   the API socket, gone when the service stops
```

## Build

Cross-compile on a workstation ([ADR-0002](adr/0002-raspberry-pi-target.md)). The
[CI image](ci.md#running-the-gates-locally) already has the toolchain:

```bash
docker run --rm -v "$PWD:/repo" -v splitforge-target:/build splitforge-ci cross
```

Or natively, on a Debian or Ubuntu machine with `gcc-aarch64-linux-gnu` installed:

```bash
cargo build --release --target aarch64-unknown-linux-gnu --workspace
```

Building on the Pi itself works and is slow. It is a reasonable fallback when the
cross-toolchain is the thing that is broken.

## Install

Copy the four files to the device, then:

```bash
sudo install -m 0755 splitforge-edge splitforge /usr/local/bin/
sudo install -m 0644 splitforge.sysusers.conf /usr/lib/sysusers.d/splitforge.conf
sudo install -m 0644 splitforge-edge.service /etc/systemd/system/

sudo systemd-sysusers                      # creates the splitforge user and group
sudo systemctl daemon-reload
sudo systemctl enable --now splitforge-edge.service
```

Then check it:

```bash
systemctl status splitforge-edge.service
sudo curl -s --unix-socket /run/splitforge/api.sock http://localhost/health
```

On a device with no configured event, health answers `200` with `raw_reads: 0` — the
service creates and migrates an empty database rather than refusing to start. Configuring
the event is the CLI's job; see the [roadmap](roadmap.md#milestone-2--local-event-console).

## Who can do what

There are two tiers, and they are deliberately different privileges.

**Reading health** needs membership in the `splitforge` group, because that is what the
socket's `0660` permits. This is the whole access-control model for the API
([ADR-0021](adr/0021-local-api-listens-on-a-unix-socket.md)) — there is no token and no
authentication, so adding somebody to this group is granting them access:

```bash
sudo usermod -aG splitforge alice
curl -s --unix-socket /run/splitforge/api.sock http://localhost/health   # no sudo needed
```

**Operating the event** — importing a roster, publishing results, taking a backup — is the
CLI writing to the database directly, and needs write access to it. In the ordinary flow
the operator configures the event before the service ever runs, so the database is a file
they created and own. When the *service* created it instead, its files are `0640` and the
CLI has to run as the service account:

```bash
sudo -u splitforge splitforge --database /var/lib/splitforge/event.db doctor
```

> **Known gap.** Files the CLI creates follow the invoking shell's umask, which on a default
> Raspberry Pi OS install is `0022` — world-readable. The service's own `UMask=0007` does not
> apply to it. Until the CLI's packaging is settled, put `umask 007` in the operator
> account's shell profile. This is one of the reasons the field guide is still open.

## Changing the defaults

The unit passes no arguments; the binary's own defaults are the installed paths. To point
the service somewhere else — an external SSD, say — use a drop-in rather than editing the
unit, so an upgrade does not overwrite the change:

```bash
sudo systemctl edit splitforge-edge.service
```

```ini
[Service]
ExecStart=
ExecStart=/usr/local/bin/splitforge-edge --database /mnt/ssd/event.db
```

The empty `ExecStart=` is required: without it systemd appends a second command rather than
replacing the first. A database outside `/var/lib/splitforge` also needs
`ReadWritePaths=/mnt/ssd` added, because `ProtectSystem=strict` makes everything else
read-only.

## Operating

```bash
sudo systemctl restart splitforge-edge     # SIGTERM; the socket goes with it
sudo systemctl stop splitforge-edge
journalctl -u splitforge-edge -f           # tracing output, journald-native
journalctl -u splitforge-edge -b           # this boot only
```

Stopping the service does **not** stop the CLI from working. Nothing in the read path
traverses the API ([S10](threat-model.md#security-risks)), and today the service does not
write to the journal at all — that arrives with the reader in Milestone 3.

## What the unit guarantees, and why

The reasoning is in [ADR-0022](adr/0022-the-service-never-waits-for-the-network.md). The
short version:

- **It does not wait for a network.** `After=network.target` orders it; nothing `Wants=` a
  network target, so a Pi with an unplugged cable starts immediately instead of waiting 90
  seconds for `network-online.target` that will never arrive.
- **It never stops restarting.** `Restart=always` plus `StartLimitIntervalSec=0`. systemd's
  default gives up permanently after 5 starts in 10 seconds; a timer that has given up
  records nothing for the rest of the event.
- **It cannot open a network socket.** `RestrictAddressFamilies=AF_UNIX` makes ADR-0021 a
  kernel rule rather than a code review. Milestone 3 will widen this for the reader,
  deliberately.
- **Nothing it writes is world-readable.** `UMask=0007`. The database holds participant
  names and the sidecar holds every raw read in plain text
  ([ADR-0018](adr/0018-write-ahead-sidecar-journal.md)).

## Observed

Installed exactly as above, under real systemd, and exercised end to end:

```console
$ systemd-analyze verify /etc/systemd/system/splitforge-edge.service
$ echo $?
0

$ systemctl enable --now splitforge-edge.service && systemctl is-active splitforge-edge
active

$ ls -ld /run/splitforge /var/lib/splitforge && ls -l /run/splitforge /var/lib/splitforge
drwxr-x--- 2 splitforge splitforge   60 /run/splitforge
drwxr-x--- 2 splitforge splitforge 4096 /var/lib/splitforge
srw-rw---- 1 splitforge splitforge      0 api.sock
-rw-r----- 1 splitforge splitforge   4096 event.db
-rw-r----- 1 splitforge splitforge  32768 event.db-shm
-rw-r----- 1 splitforge splitforge 230752 event.db-wal
-rw-rw---- 1 splitforge splitforge      0 event.db.reads.jsonl

$ curl -s --unix-socket /run/splitforge/api.sock http://localhost/health
{"status":"ok","degraded_by":[],"version":"0.0.0","uptime_seconds":1,"database":"event.db",
 "schema_version":4,"raw_reads":0,"free_mb":936308,"min_free_mb":256,"above_floor":true}
HTTP 200

$ kill -9 $(systemctl show -p MainPID --value splitforge-edge)   # the crash
$ systemctl is-active splitforge-edge
active
pid 392 -> 428

$ systemctl stop splitforge-edge
$ systemctl show -p Result -p ExecMainStatus splitforge-edge
Result=success
ExecMainStatus=0
$ ls /run/splitforge
ls: cannot access '/run/splitforge': No such file or directory

$ systemd-analyze security splitforge-edge.service | tail -1
→ Overall exposure level for splitforge-edge.service: 1.0 OK :-)
```

Every mode above is the one the unit produces, not the one it intends. The first run of this
service under systemd created `event.db` and `event.db.reads.jsonl` **world-readable**, which
is what `UMask=0007` is there to fix and what no amount of reading the unit had revealed.

The `SIGKILL` is the restart policy under the only condition that tests it, and the missing
`/run/splitforge` afterwards is the graceful path: the service removed its socket on SIGTERM,
and systemd removed the directory with the service.

## What this does not prove

This ran under systemd 252 in a container on x86-64. That covers the unit's syntax, the
account, the directories and their modes, the syscall filter against real SQLite writes and
a real `statvfs`, the restart policy, and the shutdown path. It does not cover:

- The Pi's kernel, which is where a syscall filter or a `ProcSubset=` restriction would
  differ if it differed anywhere
- Boot ordering on a real boot, with a real network interface in a real state
- Anything about SD cards, power loss, thermals, or clocks — see
  [ADR-0002](adr/0002-raspberry-pi-target.md) and
  [clock discipline](clock-and-time-discipline.md)

Those are validated on hardware or they are not validated
([ordering principle 4](roadmap.md#ordering-principles)).
