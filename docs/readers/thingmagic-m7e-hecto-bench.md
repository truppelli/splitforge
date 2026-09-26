# M7E-HECTO: the first bench sessions

A runbook for the first days with SparkFun's M7E Hecto board on the Raspberry Pi 4. It turns the
unticked boxes of [Milestone 3a](../roadmap.md#milestone-3a--one-serial-reader) into things to do,
things to see, and things to keep. The module is described in
[its notes](thingmagic-m7e-hecto.md); the order it comes in is
[hardware-plan.md § 3](../hardware-plan.md#3-phase-0--bench-validation-500-now).

**Keep everything.** Every session runs with `--capture`
([ADR-0040](../adr/0040-a-serial-session-can-be-captured-byte-for-byte.md)), and the capture, the
database and its sidecar, the service's log, and your notes go in one dated directory per
session. A box is ticked on what was observed and kept, never on what was seen once and
remembered.

Sessions 1 to 4 need the board, the Pi, a tag and a desk. Session 5 needs the lane, and 6 and 7
need hours.

## Before the parts arrive

- [ ] Raspberry Pi OS (64-bit) on the high-endurance microSD, on the Pi 4, on wired Ethernet.
- [ ] SplitForge installed as [deployment.md](../deployment.md) describes: the binaries, the unit,
      the sysusers, udev and modules-load files. `systemctl status splitforge-edge` running with
      no reader, `/health` answering.
- [ ] `chrony` running, with the DS3231 fitted if it has arrived. `splitforge doctor` reports the
      clock source.
- [ ] `sqlite3` and `jq` installed (`sudo apt install sqlite3 jq`). The queries below run as root
      against a snapshot, never the live file: `backup create` writes it as the service's user,
      `0640`.
- [ ] Your own account in the `splitforge` group, so `curl` can reach `/health` on the socket
      ([deployment.md](../deployment.md#who-can-do-what)). The loop in session 7 runs over `ssh`
      as that account.
- [ ] A bench race, configured through the CLI as the service's own user, with one reader mapped
      to the finish. The test tags' chips go in once the first reads say what they are.

```bash
export DB=/var/lib/splitforge/event.db
alias sf='sudo -u splitforge splitforge --database $DB'
sf event create --name Bench
sf race create --name Lane --start "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
sf checkpoint add --name start  --kind start
sf checkpoint add --name finish --kind finish
sf reader add --id mat
sf reader map --reader mat --antenna 1 --checkpoint finish
```

## Session 1: the board, before any service

1. **Heatsinks** on the Pi. The fan wired to the header.
2. **The UART switch to `USB`.** On `SER` the Pi sees a CH340C with nothing behind it: a port
   that opens, and a start sequence nobody answers.
3. **Leave the RF resistor where it is for now.** The board ships wired to its PCB trace antenna,
   which reads a tag at one to two feet. That is enough for sessions 1 to 4, and it is the one
   configuration in which the module is never transmitting into an unconnected port.
4. **Plug it in**, and check what the Pi saw:

   ```bash
   lsusb | grep -i 1a86                         # expect: ID 1a86:7523 QinHeng … CH340
   dmesg | tail -5                              # expect: ch341-uart converter now attached to ttyUSB0
   ls -l /dev/splitforge-reader                 # expect: a link to ttyUSB0
   ls -l /dev/ttyUSB0                           # expect: crw-rw---- root splitforge
   udevadm info /dev/ttyUSB0 | grep -E 'ID_VENDOR_ID|ID_MODEL_ID|ID_SERIAL_SHORT'
   ```

**Keep:** the output of all five.

**Closes, or corrects:**
- the udev rule's `1a86:7523`, which came from documentation. If `lsusb` says otherwise, the
  rule and [the reader notes](thingmagic-m7e-hecto.md#deployment-notes) change together, and
  `apps/splitforge-edge/tests/unit_file.rs` holds them to it.
- whether the CH340C carries a USB serial number. An `ID_SERIAL_SHORT` line says it does, which
  would let two boards be told apart by the rule after all.

## Session 2: first contact

Point the service at the board, with a capture:

```bash
sudo systemctl edit splitforge-edge.service
```
```ini
[Service]
ExecStart=
ExecStart=/usr/local/bin/splitforge-edge --serial /dev/splitforge-reader --region na --read-power 20 --capture /var/lib/splitforge/session-2.capture
```
```bash
sudo systemctl restart splitforge-edge
journalctl -u splitforge-edge -f
```

**Expect**, in the log: the `SERIAL reader` line naming region `na` and read power 20.00 dBm, then
*"the reader applied a read power of … dBm, and accepts … to … dBm"*, and no refusal. On
`/health`: `reader.state` `connected`, and `reader.read_power` present.

With **no tag near the board**, for two minutes:

```bash
grep -c '< ff[0-9a-f]\{2\}220400' /var/lib/splitforge/session-2.capture
```

A count near 120 is the end-of-cycle frame at about one a second
([finding 17](vendor-documents.md#17-an-empty-field-produces-a-frame-at-the-end-of-every-search-cycle)),
and the liveness signal [Q14](../open-questions.md#q14-reader-silence-threshold) waits on. Zero
means it does not exist on this module, which is as useful to know. (A frame split across two
reads is missed by the `grep`, so the count is a floor.)

**Then one tag against the board**, for thirty seconds:

```bash
sf reads --limit 5
curl -s --unix-socket /run/splitforge/api.sock http://localhost/health | jq .reader
```

**Expect:** reads with the tag's EPC as `chip`, `decode_faults` 0, `framing_faults` near 0.

**Keep:** the capture, the log, and `/health`. Then, from the capture, the first whole tag report,
a line that starts `< ff` with `22` then `0000` in bytes 2 to 4, and its neighbours if the
frame was split.

**Closes:**
- *Start the stream*: the module accepted every step. The capture holds each answer.
- *Decode a tag report*, once the frame is a test. Paste its bytes into
  `crates/splitforge-thingmagic/src/tag_report.rs` the way `CAPTURED_FRAME` was, asserting the
  chip, antenna and RSSI it decodes to. The decoder is then anchored on this module, not an M6e.
- *Framing before semantics*: every frame verified, so the CRC is this module's too.
- The power read-back: what the module says it applied, and the range it accepts.
- Finding 17, either way.

If a step is refused, the log names it and the status. That is a finding, not a failure: keep
the capture, and stop.

## Session 3: what a connection looks like when it goes

With the service running and a tag against the board:

1. **Pull the USB cable.** Wait ten seconds. Plug it back in.
2. Do it twice more, leaving it out for one second and for sixty.
3. `sudo systemctl restart splitforge-edge`, and later `sudo systemctl kill -s KILL splitforge-edge`.

**Expect:** on each pull, `/health` degraded with a **confirmed** gap within a second, and in the
capture an `error` or `eof` line and a `close`. On each return, `connected`, the gap closed, and
reads resuming. On each restart, recovery reports nothing to replay.

**Keep:** the capture, the log, and the gaps:

```bash
sf reader gaps
```

It lists them newest first with how each was noticed and how long it lasted, and counts the
confirmed, the suspected, and any still open at the top.

**Closes:**
- *`ReaderProvider` on top of the codec*: what an unplugged CH340C returns, which the capture's
  `error` line names. A pseudo-terminal returned `BrokenPipe`
  ([finding 20](vendor-documents.md#20-an-idle-port-times-out-and-a-closed-one-does-not)); this is
  the USB bridge's answer.
- *Compose the module*, and the *Needs the module* unit item: a real `ttyUSB` opened by the unit
  as installed.
- *Detect a disconnection and record it as a bounded gap*: one confirmed, bounded gap per pull.

## Session 4: the clock the module keeps, and the Pi's jitter

One tag against the board for ten minutes, then:

```bash
sf backup create /tmp/snap.db
sudo sqlite3 /tmp/snap.db "
  WITH r AS (
    SELECT received_at_us - LAG(received_at_us) OVER (ORDER BY seq) AS d_rx,
           reader_uptime_us - LAG(reader_uptime_us) OVER (ORDER BY seq) AS d_mod
    FROM raw_reads WHERE reader_uptime_us IS NOT NULL)
  SELECT COUNT(*) AS pairs,
         SUM(d_mod < 0) AS module_counter_went_back,
         MIN(d_rx - d_mod) AS min_us, MAX(d_rx - d_mod) AS max_us,
         AVG(d_rx - d_mod) AS mean_us
  FROM r WHERE d_rx IS NOT NULL;"
```

**Read it as:**
- `module_counter_went_back` counts where the module's timestamp restarted. About one per
  second means it counts from each search cycle, as SparkFun describes. About one per connection
  means it counts from the read command, as the user guide does. That is
  [finding 18](vendor-documents.md#18-sparkfun-and-the-user-guide-disagree-on-what-the-tag-timestamp-counts-from),
  settled.
- Where it did not go back, `d_rx - d_mod` is how much more the Pi's receive clock moved than
  the module's did. Its spread is the receive-time jitter: USB, the CH340C, and the scheduler,
  plus the module's millisecond resolution.

**Keep:** the query and its output, with how the Pi was loaded at the time.

**Closes:** *Session-anchored timestamps*, and *Measure the Pi's receive-time jitter*, which is
what bounds accuracy on this hardware.

## Session 5: the external antenna, and the read zone

1. Power off. **Move the 0 Ω `RF` resistor to the U.FL position** (SparkFun's
   [external antenna guide](https://docs.sparkfun.com/SparkFun_Simultaneous_RFID_Reader_M7E/external_antenna/)).
   Photograph it before and after.
2. Fit the pigtail and the panel antenna **before** powering on. From here, the module is never
   powered without the antenna connected.
3. Map the lane as [hardware-plan.md § 7](../hardware-plan.md#7-software-plan) step 7 lays out:
   static range map, motion trials, crowding, boundary, timed trials. Do it at two or three read
   powers. **Each power is a restart**, and each start records its power on the audit trail
   ([ADR-0038](../adr/0038-each-connection-sets-the-read-power-the-operator-chose.md)).

**Try once:** `--read-power 30`. The module should refuse it with status `0x0103`, the log should
say so, and the connection should not start. That is the refusal path, observed.

**Keep:** the maps, the photographs, the captures, and the power each was taken at.

**Closes:** the read range and RSSI rows of the support checklist, and the threshold for
`first-above-rssi`, calibrated at a power somebody chose.

## Session 6: heat

At the power session 5 chose, in the closed enclosure, with tags being read steadily, for an
hour.

```bash
grep -c '< ff[0-9a-f]\{4\}0504' /var/lib/splitforge/session-6.capture
```

**Expect:** zero. A count above zero is the module turning its RF off for heat, as
[finding 27](vendor-documents.md#27-overheating-is-reported-as-0x504) expects it to say. Note
what `/health` showed at the time, because the adapter counts those frames as decode faults
rather than naming them.

**Keep:** the capture, the count, and the ambient temperature.

## Session 7: the M3a exit run

The criterion: *a serial module runs for several hours while every read the host receives is
preserved through deliberately induced disconnections and service restarts; the journal never
disagrees with what arrived; and every disconnection is detected and recorded as a bounded gap.*

**Size the stream first.** With the read filter off, one tag held in the field streams many reads
a second. Four hours of that is millions of reads, and `doctor` and the bundle still load the
whole journal (the roadmap's [Hygiene](../roadmap.md#hygiene) item). Use a tag that crosses the
lane every few seconds, and check `reads_received` after ten minutes to estimate the total.

**Run it for four hours**, with a written schedule of what was done and when:
- ten cable pulls, of one to sixty seconds;
- five `systemctl restart`, and three `systemctl kill -s KILL`;
- **three power cuts at the Pi's supply.** Before each one, log what the service said it had
  made durable, from another machine:

  ```bash
  while true; do
    ssh pi "curl -s --unix-socket /run/splitforge/api.sock http://localhost/health" \
      | jq -c '[now, .reader.reads_persisted]'
    sleep 0.5
  done >> persisted.log
  ```

  After it boots, the journal must hold at least the last logged `reads_persisted`. If it holds
  fewer, the card acknowledged a write it did not keep.

**Afterwards:**

```bash
sf doctor                     # clean, or a torn write per power cut and nothing worse
sf status                     # raw_reads
ls -l /var/lib/splitforge     # what a day weighs: the database, the sidecar, the capture
```

and `sf reader gaps --limit 200`: one confirmed, bounded gap per pull, and suspected ones only
where the stream was quiet.

and, for the criterion's middle clause, the whole capture against the journal
([ADR-0041](../adr/0041-a-capture-is-checked-against-the-journal-by-payload.md)):

```bash
sudo splitforge-capture check /var/lib/splitforge/session-7.capture --reader mat
```

It replays the capture with the service's own reassembler and decoder, and compares the reads,
payload for payload, with the journal's reads received in the same span. `agree` (exit 0) is the
criterion met. `disagree` (exit 1) lists the payloads on one side and not the other. `incomplete`
(exit 2) means the capture dropped records, so it cannot vouch for the journal: run it again with
a sparser stream.

**Closes**:
- the M3a exit criterion;
- the *Needs the module* measurements: whether the card honours `fsync` (the power cuts), what
  the second sync costs (`recorded_at_us - received_at_us` on the snapshot), what a day's
  journal weighs, and what a write in flight does when the power goes.

## Tooling still missing

Writing this runbook found three things the exit run needs. Two are built. The third can be built
without hardware, and should be before session 7:

1. ~~**A command that lists reader gaps.**~~ Built: `splitforge reader gaps`.
2. ~~**Reconciling a capture with the journal.**~~ Built: `splitforge-capture check`, in
   session 7.
3. **`doctor` and the bundle within the Pi's memory** on a journal of millions of reads, or a
   stream kept small enough that it does not matter.

## What these sessions do not claim

- That the module is supported. It stays *experimental — under evaluation*, whatever these show
  ([why](thingmagic-m7e-hecto.md#why-this-cannot-become-supported)).
- That a lane mapped at the bench is a finish line. It is one antenna, supervised.
- Anything about M3b, which needs an LLRP reader.
