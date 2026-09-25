# ADR-0038: Each connection sets the read power the operator chose

- **Status:** Accepted
- **Date:** 2026-09-25
- **Extends:** [ADR-0033](0033-each-connection-starts-the-stream.md)

## Context

[ADR-0033](0033-each-connection-starts-the-stream.md) made every connection start the stream:
stop, version, Gen2, **the region the operator chose**, read filter off, start. The region has
no default, because which one is legal depends on where the device is, and the module is one
SKU pre-configured for several. `splitforge-edge --serial` will not start without `--region`.

**Nothing sets the transmit power.** The module reads at whatever it holds, and nothing
this project has read says what that is. The M7E-HECTO user guide gives the range, *"from
0 dBm to +27 dBm, in 0.5 dB increments"* (§ 5.3.1), and not the value it starts at.
[ADR-0035](0035-the-first-module-is-the-m7e-hecto.md) named this as a separate decision.

Power is the setting the rest of the bench work depends on:

- **The read zone.** How far from the mat a chip is read, and so where off-course reads begin,
  is set by power more than by anything else the software controls. The order's lane-materials
  line says why that matters: *"A read zone you cannot reproduce next weekend is not a
  measurement."*
- **RSSI.** `--selection-rule first-above-rssi:` chooses a read from a burst by signal
  strength, and the threshold is to be calibrated at the bench
  ([hardware-plan.md § 7](../hardware-plan.md#7-software-plan), step 5). A received signal
  strength is comparable only with others taken at the same transmit power. A threshold
  calibrated at a power nobody set, and nobody recorded, cannot be carried to the next event.
- **Current and heat.** SparkFun gives the board's draw as over 700 mA at +27 dBm, and warns
  that a 500 mA USB port may brown out above +22 dBm. The module turns its RF off when it
  overheats (§ 5.4.2.2, [finding 27](../readers/vendor-documents.md#27-overheating-is-reported-as-0x504)),
  in an enclosure that is closed but for its rear vents.
- **Regulation.** The guide's own note on § 5.2.1: *"Maximum power may have to be reduced to
  meet regulatory limits, which specify the combined effect of the module, antenna, cable and
  enclosure shielding of the integrated product."* The limit is a property of the whole
  installation, as the region's is of the place.

**The command is known, from both sources the start sequence was checked against.** Read power
is set with opcode `0x92` and a signed 16-bit big-endian value in centi-dBm, so `2700` is
27.00 dBm: MercuryAPI 2023's `TMR_SR_cmdSetReadWriteTxPower` (`SETS16`), and SparkFun's
`setReadPower`, which caps at `2700`. It is read back with `0x62`. Option `0x00` returns the
power alone, and option `0x01` also returns the module's own maximum and minimum, which is where
MercuryAPI's `/reader/radio/powerMax` and `powerMin` come from (`TMR_SR_cmdGetReadTxPowerWithLimits`,
`GETU16AT(msg, 6)`, `8` and `10`). The module refuses a value outside its range with
`FAULT_MSG_POWER_TOO_HIGH` (`103h`) or `FAULT_MSG_POWER_TOO_LOW` (`106h`). The MercuryAPI files
were re-verified against the hashes [vendor-documents.md](../readers/vendor-documents.md)
recorded on 2026-09-08.

## Decision

**1. `splitforge-edge --serial` requires `--read-power`, and it has no default**, for the same
reasons `--region` has none. The value is in dBm, with up to two decimal places (`20`, `22.5`,
`27.00`). The service refuses a negative or unparseable value itself. **Whether the value is in
range is the module's to say**, not a constant in this project: the M7e-Pico stopped at +24 and
the Hecto stops at +27, and the next module will stop somewhere else.

**2. The start sequence sets it on every connection**, after the region and before the read
filter and the start: `0x92` with the value in centi-dBm, and its answer must report success. A
module that refuses it ends the connection as `NotStarted`, the way a refused region does, and
the refusal names the step and the value. It comes after the region because nothing this project
has read says whether setting the region resets the power, and setting power second is right
either way.

**3. Then it asks what the module applied**, with `0x62` and option `0x01`: the power it now
holds, and its own maximum and minimum. The module calibrates at 0.5 dB and interpolates between,
so what it reports is what it is doing. This step is **not** required to succeed; a module that
reads, having accepted the set, is not refused for failing to describe itself. `/health` reports
the answer under the reader, and the service logs it when it changes.

**4. The service records its reader configuration on the audit trail when it starts**, as
`reader.configured`: the reader, the device path, the baud rate, the region, and the requested
read power. Region and power are what a read's signal strength and read zone mean against, and a
journal of reads with no record of either cannot be interpreted after the event. One row per start,
not per connection: both are set from the command line, so they change only when the service
restarts.

**Write power is not set.** SplitForge never writes to a tag.

## Consequences

### What this makes easy

- **A read zone and an RSSI threshold that can be reproduced.** The bench can map a lane at one
  power, move it, and map it again, and the numbers carry to the next event with the setting
  written beside them.
- **The same compliance story as the region.** The operator chooses, per installation, and the
  choice is on the record, instead of the module's default standing in for a decision.
- **One control for current and heat.** If the enclosure runs hot or a supply sags, the answer is
  a lower number on the command line, not new code.

### What this makes hard

- **Another required flag.** A drop-in that has `--serial` and `--region` and not
  `--read-power` stops working at upgrade, and says why. `deployment.md` and the unit comments
  change with the code.
- **A value out of range is learned at the first connection**, from the module, rather than when
  the service parses its arguments. The refusal names the value and the step, and the next
  connection repeats it, which is what a refused region already does.
- **Changing power means restarting the service**, as changing region does. Mapping a lane at
  several powers is several starts.

### What we accept

**Power is not recorded on each read.** A read carries no power column, and adding one would
change the shape of the evidence (ADR-0005). While power changes only when the service restarts,
the `reader.configured` row and the reads' `recorded_at` place every read under exactly one
setting. If power ever changes without a restart, that stops being true, and a read column
becomes the right answer.

**The module's default is never known.** This decision makes it not matter, rather than finding
it out. `0x62` on a first connection, before any set, would find it; nobody needs it.

**This is still one module's command set.** `0x92` and `0x62` are the M6e and M7E family's; an
LLRP reader configures power in `ROSpec`/`AntennaConfiguration`, and M3b will decide that there.

## Alternatives considered

| Alternative | Why not |
|---|---|
| Leave the module at its default | The default is not documented, so the read zone, the RSSI threshold calibrated against it, and the current drawn are all set by a number nobody has seen |
| A default power, such as 20 dBm | A default is a decision nobody made, taken silently on every install. It is the same reason `--region` has none |
| Always the module's maximum | The largest read zone, so the most off-course reads, the most heat, and the most current. A timing line wants the smallest zone that reads every runner, and only the bench can say what that is |
| Hard-code 0 to 27 dBm in the service | That is one module's range. The module reports its own, and refuses what it cannot do |
| Per-reader configuration in the database (`splitforge reader set --read-power`) | Right when one device has several readers with different antennas. There is one reader, and the region is a flag; the two move together if either moves |
| A read-power column on `raw_reads` | Changes the evidence's shape for a value that, under this decision, cannot change between restarts. Revisit if it ever can |

## References

- [ADR-0033](0033-each-connection-starts-the-stream.md): the start sequence this adds a step to,
  and the precedent of `--region`
- [ADR-0035](0035-the-first-module-is-the-m7e-hecto.md): where this was named as a separate
  decision, and the board's current draw
- [ADR-0005](0005-raw-read-append-only-journal.md): the evidence shape a read-power column would
  change
- M7E-HECTO user guide 875-0106-01 Rev 1.4, § 5.2.1, § 5.3.1, § 5.4.2.2, and the fault table
- MercuryAPI 2023 `serial_reader_l3.c`: `TMR_SR_cmdSetReadWriteTxPower`,
  `TMR_SR_cmdGetReadWriteTxPower`, `TMR_SR_cmdGetReadTxPowerWithLimits`
- SparkFun `SparkFun_UHF_RFID_Reader.cpp`: `setReadPower`, `getReadPower`
- [hardware-plan.md § 7](../hardware-plan.md#7-software-plan): the bench steps that depend on
  power
