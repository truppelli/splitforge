# ADR-0033: Each connection starts the stream, with the command the decoder is anchored on

- **Status:** Accepted
- **Date:** 2026-09-15
- **Supersedes:** — (refines when [ADR-0030](0030-the-serial-adapter-waits-for-proof.md) announces a connection that starts a stream; the rest of ADR-0030 stands)

## Context

The serial adapter could only listen. `Port` was `Box<dyn Read + Send>`, and nothing sent the
module a command. User guide § 7 says *"the reader never initiates a communication session"*,
and § 8.8.2 says the module streams during an asynchronous inventory, which the host starts. As
built, the first session on a real module would have opened the port, heard nothing, and recorded
a *suspected* gap.

The roadmap said the bytes were already known. They were not. `vendor-documents.md` recorded the
opcodes, the search-flag values, and the layout of a *response*, but not the body of the command
that starts a read, or anything that has to be configured before it.

Reading the archived sources for that command, all re-verified against their recorded hashes,
found three things that shaped this decision ([findings 14–19](../readers/vendor-documents.md#what-starting-the-stream-settled)):

- **The decoder is anchored on the answer to one specific command.** SparkFun's `startReading`
  sends `0x2F` with an embedded `0x22` asking for option `10`, search flags `00 1B` and metadata
  flags `01 FF`. `CAPTURED_FRAME`, the one real response the decoder is anchored on, is that
  library's annotated example response, and it echoes exactly those bytes.
- **MercuryAPI 2023 builds a different command.** It adds a multi-select option byte whenever
  there is no tag-data filter, which moves every field of the response one byte right, and an
  off-time field. Sending it would produce answers in a layout no capture here shows.
- **An empty field produces a frame at the end of every search cycle.** MercuryAPI and SparkFun
  both describe a `0x22` with status `0x0400`, and SparkFun says it arrives once a second. The
  decoder counted any non-zero status as a fault, so a freshly started stream would have degraded
  health until the first tag.

The module is also a single SKU pre-configured for many regions, and
[the reader notes](../readers/thingmagic-m7e-pico.md) already concluded that the adapter
*"must set it explicitly at startup rather than assume it"*.

## Decision

**1. Every connection runs a start sequence**, waiting for each answer before the next:

| Step | Command | Must succeed |
|---|---|---|
| Stop any stream still running | `2F 00 00 02` | No: a module that was not streaming has nothing to stop |
| Version | `03` | Yes: proves an application firmware answers at this baud |
| Gen2 protocol | `93 00 05` | Yes |
| Region | `97` + the operator's region | Yes |
| Read filter off | `9A 01 0C 00` | Yes: the filter would suppress the burst every `SelectionRule` selects from |
| Start streaming | `2F` + SparkFun's body | Yes |

Each command's bytes are checked against MercuryAPI and SparkFun, which agree on all six. The
start command is SparkFun's, byte for byte, because the decoder is anchored on its answer. A test
holds the echo.

**2. No antenna-port command.** SparkFun sends `91 01 01`. MercuryAPI 2023 sends the second byte
only for the M6e family, which this module is not. The module has one port and the start command
reads from the configured list.

**3. The region is required, with no default.** `splitforge-edge --serial` refuses to start without
`--region`. Names map to codes from `tmr_region.h`. `OPEN`, `NONE`, and the two regions the header
marks for other modules are not offered.

**4. While the sequence runs, only the awaited answer goes to it.** An answer is matched by
opcode, and for `0x2F` also by the option byte it echoes, so a late stop is not taken for the
start. Every other verified frame goes to the decoder. A stream left running by the last
connection is still pouring tag reports in, and a tag report that reached the host is evidence.

**5. A connection is announced when the module accepts the start command.** This refines ADR-0030
for a connection that starts a stream, where a first verified frame or staying up was the test.
A module that answers the version and refuses the region is present and not recording, and
announcing it at its first answer would reset the backoff and write a pair of gap rows on every
attempt.

**6. A refused or unanswered step ends the connection as a new cause,
`Disconnection::NotStarted`**, with the detail *"the reader did not start reading when told to"*.
The service log names the command and status, once per distinct reason. Each answer may take
2 s.

**7. The session anchor moves when the start command is sent.** User guide § 8.8.3 measures the
tag timestamp from *"the time the command to read was issued"*.

**8. An empty `0x22` with status `0x0400` is an end-of-cycle frame: not a read and not a fault.**
It is counted. It is **not** yet treated as proof of life for the silence watchdog, and a
`0x0400` frame with a payload is still a fault.

## Consequences

### What this makes easy

- The first session on a module has a chance of hearing something, and everything it hears is in
  the layout the decoder was anchored on.
- A stream left running across a pulled cable is stopped and restarted on reconnection, so its
  timestamps are anchored to the command that started them.
- A wrong region, a module in its bootloader, or a baud rate that reaches something else shows up
  as a gap that says the reader did not start, not as silence.

### What this makes hard

- **Every byte sent is believed, not observed.** Both sources agree on each command, and no
  command here has been sent to an M7e-Pico. SparkFun's library was written for the M6e Nano.
- **The module is configured on every connection.** Settings are sent again on each reconnect,
  and nothing depends on whether the module saves them.
- **Read power is left at the module's default.** Choosing it is a site decision, and it belongs
  with the first bench measurements, not with the protocol.

### What we accept

- **A refused start retries with growing backoff forever.** A region the module will never take
  produces a gap and one log line, and then nothing more until the reason changes.
- **End-of-cycle frames are counted and not yet used.** Using them to tell a quiet checkpoint from
  a dead module would change the silence watchdog, which is Q14. The period needs observing on
  this module first.
- **SparkFun describes the tag timestamp as time since the last keep-alive**, which the guide
  contradicts. Nothing authoritative rests on it, because receipt time is what counts on this
  module.

## Alternatives considered

| Alternative | Why not |
|---|---|
| Rebuild MercuryAPI 2023's read command | Its multi-select byte and off-time field produce answers in a layout no capture shows, and the decoder would refuse or misread them |
| Send only the start command | A module whose region or protocol was never set, or was set by something else, would start reading on whatever it last held |
| Default the region to North America | The module transmits. Which band is legal depends on where it is, and a default is a guess made on the operator's behalf |
| Send SparkFun's antenna-port command too | The vendor's own code sends a different byte count for this module family |
| Treat end-of-cycle frames as proof of life now | Changes gap detection on a cadence nobody has measured on this module |
| Keep counting every non-zero status as a fault | Two sources document the `0x0400` frame arriving once a second into an empty field, so health would read as broken whenever nobody was crossing |

## References

- [vendor-documents.md, findings 14–19](../readers/vendor-documents.md#what-starting-the-stream-settled)
- [ADR-0024](0024-serial-reader-adapter-before-llrp.md): the device's receipt time is authoritative
- [ADR-0025](0025-m3a-proves-durability-above-the-transport.md): why the adapter streams
- [ADR-0027](0027-a-reader-reports-connection-events-on-the-read-channel.md) and
  [ADR-0030](0030-the-serial-adapter-waits-for-proof.md): the connection events this refines
- [Q14](../open-questions.md#q14-reader-silence-threshold)
- `crates/splitforge-thingmagic/src/command.rs`, `src/start.rs`, `src/provider.rs`,
  `src/tag_report.rs`: `NO_TAGS_FOUND`
