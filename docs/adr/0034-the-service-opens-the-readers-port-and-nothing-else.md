# ADR-0034: The service opens the reader's port and nothing else

- **Status:** Accepted
- **Date:** 2026-09-21
- **Supersedes:** — (loosens `PrivateDevices=yes` in the unit; [ADR-0032](0032-the-service-speaks-ip-to-this-device-only.md)'s network directives are untouched)

## Context

The unit set `PrivateDevices=yes`, which gives a service its own `/dev` holding `null`, `zero`,
`random`, the pseudo-terminals, and little else. `splitforge-edge --serial` opens a device
node. Under the shipped unit that node did not exist, so the first session with a module would
have opened nothing, and the service would have recorded a confirmed gap for as long as it
ran.

[The hardware plan](../hardware-plan.md) had caught it, and written down the replacement —
`PrivateDevices=no`, `DevicePolicy=closed`, `DeviceAllow=char-ttyUSB rw`, and a udev rule for a
stable name. No test had: the serial rehearsal runs the binary against a pseudo-terminal, but
outside the unit. And the roadmap filed the change under *needs the module*, which most of it
does not.

Running each piece under systemd 252 on Debian bookworm found three things the plan did not
say:

- **`DeviceAllow=char-ttyUSB` is looked up in `/proc/devices` when the service starts**, and the
  kernel lists `ttyUSB` only once the `usbserial` driver has loaded. Where it has not, the line
  allows nothing: a `ttyUSB` node is *"Operation not permitted"* on every open. systemd reports
  it only at debug level, as *`Device allow list pattern "ttyUSB" did not match anything.`* Left
  to udev, the driver loads when a bridge is plugged in, so a Pi booted with the reader unplugged
  starts the service first.
- **`ProtectClock=yes` adds `char-rtc r` to the device list without closing the policy.** With
  `PrivateDevices=no` and nothing else, a serial port opened. The filter has to be stated.
- **The service discarded the reason a port would not open.** `provider.rs` matched `Err(_)`,
  so every failure was a gap with no cause attached, even though `port::open` had kept the
  error's kind and path since the pty rehearsal. The three failures an operator meets — a
  missing node, a node owned by another group, and a node the filter refuses — are fixed in
  three different places and looked identical.

## Decision

**1. The unit keeps a device filter in place of a private `/dev`:**

```ini
PrivateDevices=no
DevicePolicy=closed
DeviceAllow=char-ttyUSB rw
```

The service sees this device's `/dev`. It may open the pseudo-devices every service may, and
USB serial ports, and nothing else. `PrivateDevices=yes` also removes two capabilities and adds
`~@raw-io` to the syscall filter; the unit's bounding set is already empty, and
`@system-service` contains no `@raw-io` call, so neither is lost.

**2. `usbserial` is loaded at boot**, by `deploy/splitforge.modules-load.conf`.
`systemd-modules-load.service` runs before any ordinary service, so the group exists when the
filter is built.

**3. A udev rule gives the port a stable name and the service's group:**
`/dev/splitforge-reader`, `splitforge`, `0660`. The service account does not join `dialout`,
which would grant every serial port on the device. The guide names the port by that link and
never by `ttyUSB0`, which renumbers when the bridge re-enumerates while the old node is held,
as a pulled cable does.

**4. The rule matches any USB serial adapter** until the USB-UART bridge is bought. Narrowing it
to the bridge's vendor, product, and serial number is a one-line change once the bridge exists.
A rule with placeholder IDs would match nothing and make every open *"Permission denied"*.

**5. The service logs why a port did not open**, once per distinct reason, and again after a
connection has proved itself. The install guide maps each message to its fix.

**6. `unit_file.rs` holds the files to each other.** The rule's kernel match must name the group
the unit allows, the rule's group must be the sysusers account, the modules-load file must load
the driver that registers that group, the guide must tell the operator the rule's link, and
every file in `deploy/` must be in the guide's install table.

## Consequences

### What this makes easy

- The service can open a reader, which it could not before.
- A reader plugged in after boot, unplugged, and plugged back in is reopened by the name the
  rule moves with it.
- Each way the port fails to open says which of three files is wrong.

### What this makes hard

- **Exposure rises from 1.1 to 1.3.** `systemd-analyze security` scores `PrivateDevices=` at
  0.2. The service can list every name in this device's `/dev`, though it can open only what
  the filter allows.
- **A filter that allows nothing fails silently at the systemd layer.** Only the service's own
  log says *"Operation not permitted"*. The modules-load file prevents it. Loading the driver by
  hand after the service started does not undo it until the service restarts, and the service
  does not exit on a refused port, so `Restart=always` never gets the chance.
- **A second USB serial adapter contends for the name**, until the rule is narrowed. The plan puts
  only the GPS's PPS on a GPIO pin, and says nothing yet about how its NMEA arrives.

### What we accept

- **None of this has opened a `ttyUSB` port.** The container's kernel has no `usbserial`. The
  filter was observed allowing a registered group (`ttyS`) and refusing an unregistered one
  (`ttyUSB`), and the service ran its whole lifecycle under the unit against a pseudo-terminal.
  Whether `usbserial` is built into the Pi's kernel or loaded as a module is unverified. Loading
  it at boot is harmless either way.
- **A CDC-ACM bridge, which enumerates as `ttyACM`, needs the unit, the rule, and the module
  changed together.** `unit_file.rs` fails until they agree.

## Alternatives considered

| Alternative | Why not |
|---|---|
| Keep `PrivateDevices=yes` and bind the node in with `BindPaths=` | Observed: a node created after the service started never appeared in its `/dev`. A Pi booted with the reader unplugged could never open it |
| `PrivateDevices=no` alone | Observed: `ProtectClock=yes`'s own entry does not close the policy, and a serial port opened. Every device node on the Pi would be reachable |
| `DeviceAllow=/dev/splitforge-reader` | Observed: a node absent when the service started was *"Operation not permitted"* after it was created, and the same node present at start opened. The same morning problem as `BindPaths=` |
| `SupplementaryGroups=dialout` instead of a udev rule | Grants every serial port on the device, and gives no stable name |
| Ship the rule with placeholder vendor and product IDs | It matches nothing, so the node stays `root:dialout` and every open is *"Permission denied"* |
| Have the service exit on a refused port, so `Restart=always` rebuilds the filter | A missing reader is the ordinary state at boot. A crash loop on every unplugged morning buries the logs and restarts a service that is also serving health |

## References

- [deployment.md, *Connecting the reader*](../deployment.md#connecting-the-reader) and
  [*The reader's port*](../deployment.md#the-readers-port): the install steps, the failure table,
  and the observations
- [hardware-plan.md, step 5](../hardware-plan.md): where this was first written down
- [ADR-0021](0021-local-api-listens-on-a-unix-socket.md) and
  [ADR-0032](0032-the-service-speaks-ip-to-this-device-only.md): the unit's network directives,
  which this does not touch
- [ADR-0033](0033-each-connection-starts-the-stream.md): what the service sends once the port
  opens
- `deploy/splitforge-edge.service`, `deploy/99-splitforge-reader.rules`,
  `deploy/splitforge.modules-load.conf`, `apps/splitforge-edge/tests/unit_file.rs`,
  `crates/splitforge-thingmagic/src/provider.rs`: `report_open_failure`
