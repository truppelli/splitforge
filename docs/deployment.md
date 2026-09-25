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
| `deploy/99-splitforge-reader.rules` | `/etc/udev/rules.d/99-splitforge-reader.rules` | The reader's port: a stable name, owned by the service's group |
| `deploy/splitforge.modules-load.conf` | `/etc/modules-load.d/splitforge.conf` | Loads the USB serial driver at boot, before the service starts |

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

Copy the six files to the device, then:

```bash
sudo install -m 0755 splitforge-edge splitforge /usr/local/bin/
sudo install -m 0644 splitforge.sysusers.conf /usr/lib/sysusers.d/splitforge.conf
sudo install -m 0644 splitforge-edge.service /etc/systemd/system/
sudo install -m 0644 99-splitforge-reader.rules /etc/udev/rules.d/
sudo install -m 0644 splitforge.modules-load.conf /etc/modules-load.d/splitforge.conf

sudo systemd-sysusers                      # creates the splitforge user and group
sudo systemctl restart systemd-modules-load  # loads usbserial now; every boot does it anyway
sudo udevadm control --reload && sudo udevadm trigger --subsystem-match=tty
sudo systemctl daemon-reload
sudo systemctl enable --now splitforge-edge.service
```

The order matters once: `systemd-sysusers` has to create the group before udev can give the
reader's port to it, and the driver has to be loaded before the service starts, for the
reason [below](#connecting-the-reader).

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

**The group grants more than health, and it is read access.** `/var/lib/splitforge` is
`0750` and the files the service creates in it are `0640`, so a member can read the event
database, with every participant's name, and the write-ahead sidecar, with every raw read.
Add people to the group on that understanding. What it does not grant is writing any of it.
That includes the sidecar, which the service creates `0640` itself rather than leaving to the
umask. Every start replays the sidecar into the append-only journal, and each replay is
written to the audit trail as `journal.replay`, so write access to that file would be write
access to evidence.

A sidecar created by a build before this change is `0660`, because the mode only applies
when the file is created. Check with `ls -l /var/lib/splitforge` and fix it with
`sudo chmod 640 /var/lib/splitforge/event.db.reads.jsonl`.

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
> apply to it. The sidecar is the exception, because it asks for `0640` whoever creates it.
> The database is not. Until the CLI's packaging is settled, put `umask 007` in the operator
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

## Connecting the reader

With no arguments the service reads from nothing. To read from a ThingMagic serial module,
add a drop-in:

```bash
sudo systemctl edit splitforge-edge.service
```

```ini
[Service]
ExecStart=
ExecStart=/usr/local/bin/splitforge-edge --serial /dev/splitforge-reader --region na
```

`--region` has no default, because the module transmits and which band is legal depends on
where it is ([ADR-0033](adr/0033-each-connection-starts-the-stream.md)).

**Name the port `/dev/splitforge-reader`, never `/dev/ttyUSB0`.** The udev rule creates that
name and moves it with the device. `ttyUSB0` becomes `ttyUSB1` if the bridge re-enumerates
while the old node is still held open, which is what a pulled cable does, and a service
pointed at `ttyUSB0` would never reconnect.

**The service may open USB serial ports and no other device**
([ADR-0034](adr/0034-the-service-opens-the-readers-port-and-nothing-else.md)). The unit's
filter names the `ttyUSB` device group, and systemd looks that group up when the service
starts. The kernel lists it only once the `usbserial` driver has loaded, and left to udev it
loads when a bridge is plugged in — so on a Pi booted with the reader unplugged, the service
would start first and be refused the port for as long as it ran. `splitforge.modules-load.conf`
loads the driver at boot, before the service. **If you load it by hand instead, restart the
service afterwards.**

When the port does not open, the service logs why, once per reason, and `/health` reports the
reader `disconnected` with a confirmed gap:

| The log says | What it means | Fix |
|---|---|---|
| `No such file or directory` | Nothing is plugged in, or the udev rule is not installed, so there is no `/dev/splitforge-reader` | Plug in the reader. `ls -l /dev/splitforge-reader` should show a link |
| `Permission denied` | The node exists and belongs to another group, usually `dialout`: the rule did not apply | Install the rule, then `udevadm trigger --subsystem-match=tty`. The node should be `splitforge` and `0660` |
| `Operation not permitted` | The unit's device filter refused it. Either the driver was not loaded when the service started, or the bridge is not a `ttyUSB` device | `grep ttyUSB /proc/devices`. If it is listed now, restart the service. If the port is a `ttyACM`, the unit and the rule both need changing |

A full line reads:

```text
splitforge-thingmagic: the port did not open: opening the serial port /dev/splitforge-reader: Operation not permitted. It is retried with backoff; this is logged again only if the reason changes.
```

The service does not exit when the port will not open, so `Restart=always` never gets a
chance to fix a refusal. It keeps retrying until the port opens or it is restarted.

**The rule matches the reader's USB-UART bridge**, the CH340C on SparkFun's M7E Hecto board,
by its vendor and product ID, `1a86:7523`
([the reader notes](readers/thingmagic-m7e-hecto.md#deployment-notes)). Those IDs come from
documentation; if `/dev/splitforge-reader` does not appear with the board plugged in, compare
them with `udevadm info /dev/ttyUSB0`. Any other CH340 adapter on the same Pi contends for the
name, so keep them unplugged. CH340-family bridges are not expected to carry a serial number,
so two identical boards on one Pi cannot be told apart by this rule.

## Operating

```bash
sudo systemctl restart splitforge-edge     # SIGTERM; the socket goes with it
sudo systemctl stop splitforge-edge
journalctl -u splitforge-edge -f           # tracing output, journald-native
journalctl -u splitforge-edge -b           # this boot only
```

**The service removes only a socket at its socket path.** A socket left there by a crash is
cleared and replaced. Anything else, such as a file a mistyped `--socket` in a drop-in points at,
is left alone, and the service stops with a line naming it: *"… is not a socket, so it was left
alone and the API was not started."* systemd then restarts it and it refuses again, so the line
repeats in `journalctl -u splitforge-edge` until `--socket` is corrected.

Stopping the service does **not** stop the CLI from working. Nothing in the read path
traverses the API ([S10](threat-model.md#security-risks)). The service writes reads to the
journal itself, from `--serial` or `--simulate`, and the CLI reads the same database.

## What the unit guarantees, and why

The reasoning is in [ADR-0022](adr/0022-the-service-never-waits-for-the-network.md). The
short version:

- **It does not wait for a network.** `After=network.target` orders it; nothing `Wants=` a
  network target, so a Pi with an unplugged cable starts immediately instead of waiting 90
  seconds for `network-online.target` that will never arrive.
- **It never stops restarting.** `Restart=always` plus `StartLimitIntervalSec=0`. systemd's
  default gives up permanently after 5 starts in 10 seconds; a timer that has given up
  records nothing for the rest of the event.
- **It cannot reach the network, or be reached from it.** `IPAddressDeny=any` with
  `IPAddressAllow=localhost` makes ADR-0021 a kernel rule rather than a code review.
  `RestrictAddressFamilies=AF_UNIX AF_INET` allows IPv4 only because `chronyd` answers this
  account over UDP to `127.0.0.1` and nothing else
  ([ADR-0032](adr/0032-the-service-speaks-ip-to-this-device-only.md)). Milestone 3b will widen
  `IPAddressAllow` to its reader's address, deliberately.
- **It may open the reader's port and no other device.** `DevicePolicy=closed` with
  `DeviceAllow=char-ttyUSB rw`, in place of `PrivateDevices=yes`, which hid the port
  ([ADR-0034](adr/0034-the-service-opens-the-readers-port-and-nothing-else.md)).
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

That transcript predates [ADR-0032](adr/0032-the-service-speaks-ip-to-this-device-only.md).
Under the unit as ADR-0032 left it, on Debian bookworm with systemd 252 and chrony 4.3, the
exposure level was **1.1**, and `/health` reports the time source as it should:

```console
$ curl -s --unix-socket /run/splitforge/api.sock http://localhost/health   # abridged
{"status":"ok","degraded_by":[],...,"clock_source":{"measurement":"measured","state":"ntp_synced"}}

$ systemd-analyze security splitforge-edge.service | tail -1
→ Overall exposure level for splitforge-edge.service: 1.1 OK :-)
```

With `RestrictAddressFamilies=AF_UNIX` alone, the same health line read
`{"measurement":"daemon_unreachable","state":null}`, and every read was stamped `unsynced`.

### The reader's port

Under the unit as it is now, the exposure level is **1.3**. The one row that changed is
`PrivateDevices=`, at 0.2:

```console
$ systemd-analyze security --offline=yes splitforge-edge.service | grep -E "PrivateDevices|DeviceAllow"
✗ PrivateDevices=  Service potentially has access to hardware devices                0.2
✗ DeviceAllow=     Service has a device ACL with some special devices: char-rtc:r char-ttyUSB:rw  0.1
```

Opening a port as the service account, under each configuration. Every node was owned by
`splitforge` and `0660`, as the udev rule leaves it, so the filter was the only thing that
differed:

| Configuration | A `ttyS` port | A `ttyUSB` port, driver not loaded |
|---|---|---|
| No sandbox | opens | `No such device or address` |
| `PrivateDevices=yes`, as it was | not in the service's `/dev` | not in the service's `/dev` |
| `PrivateDevices=no`, `ProtectClock=yes` | opens | `No such device or address` |
| `DevicePolicy=closed` | `Operation not permitted` | `Operation not permitted` |
| `closed`, `DeviceAllow=char-ttyS rw` | opens | `Operation not permitted` |
| `closed`, `DeviceAllow=char-ttyUSB rw` | `Operation not permitted` | `Operation not permitted` |

The last row is the unit, on a kernel with no `usbserial` driver loaded: the filter allowed
nothing, and systemd logged why only at debug level, as
`Device allow list pattern "ttyUSB" did not match anything.` The third row is why
`DevicePolicy=closed` is written out: `ProtectClock=yes` adds its own entry to the list and
still left a serial port open.

The service itself, under the unit with only `ExecStart` changed, against a scripted module
on a pseudo-terminal — which the filter always allows, and which is a real tty to the
service's terminal calls:

```console
$ systemctl start splitforge-edge                 # nothing plugged in
splitforge-thingmagic: the port did not open: opening the serial port /dev/splitforge-reader: No such file or directory. ...
{"status":"degraded",...,"reader":{"kind":"serial","state":"disconnected",...,"open_gap":{"detection":"confirmed",...}}}

$ socat pty,link=/dev/splitforge-reader,rawer,group=splitforge,perm=0660 EXEC:module.sh &
{"status":"ok",...,"reader":{"kind":"serial","state":"connected","reads_received":3,
 "reads_persisted":3,"framing_faults":0,"decode_faults":0,"open_gap":null},
 "clock_source":{"measurement":"measured","state":"ntp_synced"}}

$ kill %1                                         # the cable
splitforge-thingmagic: the port did not open: opening the serial port /dev/splitforge-reader: No such file or directory. ...
{"status":"degraded",...,"reader":{"kind":"serial","state":"disconnected","reads_persisted":3,
 "open_gap":{"detection":"confirmed",...}}}
```

The module answered the start sequence and streamed three tag reports and an end-of-cycle
frame, and the three reads are in the journal. The second log line is the first one again,
logged because a connection proved itself in between. And the three messages in the table
[above](#connecting-the-reader) are each what the service logged, from a missing node, a
`root:dialout` node, and a node the filter refused.

Binding the one node into a private `/dev` with `BindPaths=` was tried as an alternative and
does not work: a node created after the service started never appeared inside it.

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
- A real USB serial bridge. The container's kernel has no `usbserial` driver, so no
  `ttyUSB` port was ever opened: the filter's behavior for a group it allows was observed
  on `ttyS` and on pseudo-terminals, and whether `usbserial` is built into the Pi's kernel
  or loaded as a module is unverified. The udev rule was checked with `udevadm test` against
  a `ttyS` port, with its match changed to fit
- Anything about SD cards, power loss, thermals, or clocks — see
  [ADR-0002](adr/0002-raspberry-pi-target.md) and
  [clock discipline](clock-and-time-discipline.md)

Those are validated on hardware or they are not validated
([ordering principle 4](roadmap.md#ordering-principles)).
