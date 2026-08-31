# Vendor Documents

Provenance for the vendor documentation that reader adapters in this repository are built
from: where each document came from, how to get it again, and how to prove the copy you got
is the one the code was written against.

> **The documents themselves are not in this repository.** See
> [Why the bytes are not here](#why-the-bytes-are-not-here). What *is* here is every
> statement the code depends on, quoted, with its section number — so an assumption can be
> checked without the PDF, and the PDF can be verified when someone has it.

## Why this file exists

`crates/splitforge-thingmagic/`'s frame codec was written from a user guide, and by the time
[#23](https://github.com/truppelli/splitforge/pull/23) went looking for it, the vendor's
documentation portal had moved: `jadaktech.com/documents-downloads/…` 302-redirects to
`novanta.com/precision-medicine/`, and the user-guide PDFs under
`jadaktech.com/wp-content/uploads/…` return 404.

That left the highest-risk code in the project resting on a document nobody could open. Both
documents below were located on DigiKey's CDN, which is a distributor mirror and not the
vendor — so it can rotate too, and a hash is recorded for each.

## The documents

### M7E-PICO & M7E-DEKA User Guide

| | |
|---|---|
| Title | THINGMAGIC M7E-PICO & M7E-DEKA USER GUIDE |
| Document number | 875-0093-01 Rev 2.3 |
| Copyright | © 2023 Novanta Inc. and its affiliated companies |
| Source | `https://mm.digikey.com/Volume0/opasdata/d220001/medias/docus/6592/TMPicoDekaUGRev12102023.pdf` |
| Retrieved | 2026-08-28, HTTP 200 |
| Size / pages | 1,743,238 bytes · 61 pages |
| SHA-256 | `b4659cfdf69f5bf1af0671214d2a519228bace7f1b0a68fcae56b30f07b58a4c` |
| Depended on by | `crates/splitforge-thingmagic/src/frame.rs`, `src/crc.rs` |

### M7E-PICO Specification Sheet

| | |
|---|---|
| Title | M7E-PICO Spec Sheet (06/26/2023) |
| Source | `https://mm.digikey.com/Volume0/opasdata/d220001/medias/docus/5735/M7E-PICO-Spec%20Sheet_06262023.pdf` |
| Retrieved | 2026-08-28, HTTP 200 |
| Size / pages | 231,170 bytes · 3 pages |
| SHA-256 | `5e46820e7b9bcbef80fa9de13cd74f4d7ae14a22112884a4179f3db50d1a336a` |
| Depended on by | [thingmagic-m7e-pico.md](thingmagic-m7e-pico.md), [hardware-plan.md](../hardware-plan.md) |

To verify a copy is the same document:

```bash
sha256sum m7e-pico-deka-user-guide.pdf
# b4659cfdf69f5bf1af0671214d2a519228bace7f1b0a68fcae56b30f07b58a4c
```

## Why the bytes are not here

The user guide's own § 1 says so:

> This product or document is protected by copyright and distributed under licenses
> restricting its use, copying, distribution, and decompilation. No part of this product or
> document may be reproduced in any form by any means without prior written authorization of
> Novanta Corporation and its licensors, if any.

This repository is public. Committing the PDF would be reproducing it, by a means, without
that authorization — so it is not committed, and the hashes and quotations above and below
exist to make that omission cost as little as possible.

Two routes if a durable copy is wanted rather than a durable *record*: ask
`rfid-support@jadaktech.com` for authorization, which § 8 of the guide suggests they are
willing to give integrators; or submit the DigiKey URL to a public web archive, which
preserves the document without this project redistributing it.

Short quotations of technical fact, as below, are ordinary citation. The facts themselves —
that a length field is one byte, that a CRC covers four named fields — are not copyrightable
at all, which is why the section that matters most to the code is reproduced in full.

## What the code depends on, quoted

Everything in this section is the primary source for an assumption stated in
`crates/splitforge-thingmagic/`.

### Frame layout — User Guide § 7.1, § 7.2

Host-to-reader (a **command**, which carries no status word):

```text
Header    Data Length    Command    Data              CRC-16 Checksum
1 byte    1 byte         1 byte     0 to 250 bytes    2 bytes (CRC Hi | CRC Lo)
```

Reader-to-host (a **response**):

```text
Header    Data Length    Command    Status Word    Data              CRC-16 Checksum
1 byte    1 byte         1 byte     2 bytes        0 to 248 bytes    2 bytes (CRC Hi | CRC Lo)
```

### CRC coverage — User Guide § 7.3

> The same CRC calculation is performed on all serial communications between the host and the
> reader. The CRC is calculated on the Data Length, Command, Status Word, and Data bytes. The
> header is not included in the CRC.

### Antenna ports — User Guide § 8.7, and § 5 (module description)

> The module has one antenna port, and the connection is only through the edge vias of the
> module.

> The ThingMagic module has one monostatic antenna port. This port is capable of both
> transmitting and receiving. […] The module also supports Using a Multiplexer, allowing up to
> 16 total logical antenna ports, controlled [via] `/reader/antenna/portSwitchGpos`.

> NOTE: The ThingMagic module does not support bistatic (separate transmit and receive port)

### RF power — User Guide § 5

> The maximum RF power that can be delivered to a 50-ohm load from the antenna port is
> 0.25 Watts

0.25 W is +24 dBm, which is the figure [hardware-plan.md](../hardware-plan.md) uses
throughout.

## The command set is not in this document

§ 7, *Serial Communication Protocol*, is three subsections long: § 7.1 and § 7.2 are the two
framing diagrams quoted above, and § 7.3 is the CRC's covered range. There is no opcode table,
no command list, and no tag-report layout anywhere in the guide's 61 pages. The only opcodes in
it are fault names in Appendix A — `FAULT_INVALID_OPCODE` and its neighbours — which name the
error without naming the values that provoke it. The whole document contains four hexadecimal
numbers, and all four are error codes.

That is why `frame.rs` could be written from this document and why its successor cannot. Framing
is the whole of what § 7 describes, and framing is the whole of what the crate currently does.

**It is a position rather than an omission**, and § 7's opening paragraph states it:

> ThingMagic does not support bypassing the MercuryAPI to send commands to the ThingMagic module
> directly, but some information about this interface is useful when troubleshooting and
> debugging applications which interface with the MercuryAPI.

The framing is documented for people debugging MercuryAPI's traffic, not for people replacing
MercuryAPI — which is what a `ReaderProvider` in this repository would be doing. Above the frame,
the guide defers: § 4 says applications *"can be written using the high level MercuryAPI"*, that
the SDK *"contains sample applications and source code"*, and that it is the **release notes** —
a third document, not archived here and not yet located — which *"contain links to Mercury API
Programmers Guide and the Mercury API SDK."* § 8.8.3 defers the same way for the tag-report
fields: *"see MercuryAPI for code details."*

**What this does not mean.** The opcodes are neither secret nor unavailable — by § 4 the SDK
ships source code, in C among others. What it means is that this file cannot go on being the only
source, that the next source is *code* rather than a specification, and that reaching it runs
through a release-notes document nobody has found yet. Code carries a question a PDF did not.
[ADR-0007](../adr/0007-license-selection.md) makes this repository GPL-3.0-or-later, so what the
SDK's license permits is a thing to establish before reading it into a design, not after.
Archiving it is its own change, with its own row in [The documents](#the-documents), its own
hash, and its own answer to [Why the bytes are not here](#why-the-bytes-are-not-here).

## What the read path will depend on, quoted

Nothing in this section backs code that exists today. These are the statements a
`ReaderProvider` will rest on, recorded while the document is open, because three of them
constrain the design before a line of it is written.

### The serial link — User Guide § 5.1.4, § 5.1.4.1

> The module communicates to a host processor via a TTL logic level UART serial port, accessed
> on the edge "vias."

> Only three pins are required for serial communication (TX, RX, and GND). Hardware handshaking
> is not supported.

> The connected host processor's receiver must have the capability to receive up to 255 bytes of
> data at a time without overflowing. Flow control is not supported.

Default baud is 115200 (§ 5.1.4.2), one of eight from 9600 to 921600. A changed rate survives a
power cycle only *"if that baud rate is changed and saved in the application mode"*, with the
guide's own caveat to *"check the firmware release notes to confirm that saving of settings is
supported."* That is the same persistence mechanism, and the same uncertainty, that
[question 4](thingmagic-m7e-pico.md#the-four-pre-order-questions--answered-from-documentation)
leaves open about the region setting.

### Command and response discipline — User Guide § 7

> The serial communication between MercuryAPI and the ThingMagic module is based on a
> synchronized command-response/master-slave mechanism. Whenever the host sends a message to the
> reader, it cannot send another message until after it receives a response. The reader never
> initiates a communication session; only the host initiates a communication session.

### Streaming — User Guide § 8.8.2

The exception to *"the reader never initiates"*, and the mode a timing system would run in:

> When reading tags during asynchronous inventory operations (MercuryAPI `Reader.StartReading()`),
> the module "streams" the tag results back to the host processor. This means that tags are
> pushed out of the buffer as soon as they are put into the buffer by the tag reading process.
> The buffer is put into a circular mode that keeps the buffer from filling.

> NOTE: The TTL Level UART Interface does not support control lines, so it is not possible for
> the module to detect a broken communications interface connection and stop streaming the tag
> results. Nor can the host signal that it wishes tag streaming to stop temporarily without
> stopping the reading of tags.

The alternative is the tag buffer of § 8.8.1, which the host polls — a FIFO holding, *"as a rule
of thumb […] a maximum of 52 96-bit EPC tags"*, in which *"duplicate tag reads do not result in
additional entries."*

### Tag read metadata — User Guide § 8.8.3

Four of the twelve fields, being the four a timing system needs. This table's columns interleave
under ordinary text extraction, so the pairings below were confirmed against the page in
`pdftotext -table` mode rather than read off a reflowed column:

| Field | User Guide § 8.8.3 |
|---|---|
| Antenna ID | *"The antenna on which the tag was read. When Using a Multiplexer, if appropriately configured, the Antenna ID entry will contain the logical antenna port of the tag read. If the same tag is read on more than one antenna there will be a tag buffer entry for each antenna on which the tag was read."* |
| Read Count | *"The number of times the same tag was read on the same antenna (and, optionally, with the same embedded data value)."* |
| Timestamp | *"The time the tag was read, relative to the time the command to read was issued, in milliseconds. If the Tag Read Meta Data is not retrieved from the Tag Buffer between read commands, there will be no way to distinguish order of tags read with different read command invocations."* |
| RSSI | *"The receive signal strength of the tag response in dBm. For duplicate entries, the user can decide if the meta data represents the first time the tag was seen or reflects the meta data for the highest RSSI seen."* |

## What retrieving these already settled

Three things, none of which needed the module. Each was recorded here first, because each
belongs to a different file and a different review; the second has since been fixed, and the
other two are still open.

### 1. The CRC assumption is confirmed, exactly

`frame.rs` assumption 2 reads: *"The CRC covers `len`, `opcode`, `status`, and `data` —
everything between the `0xFF` and the CRC itself, excluding both."* § 7.3 says precisely that.
`crc_covered_range` is correct and can stop being described as unverified.

**Still unverified:** § 7.3 names the algorithm only as "CCITT CRC-16" and gives no polynomial
or seed. `crc.rs` uses `POLYNOMIAL = 0x1021` and `INIT = 0xFFFF`, anchored on
`crc16(b"123456789") == 0x29B1` — CRC-16/CCITT-FALSE. The user guide neither confirms nor
contradicts that choice, so it stays an assumption pending a capture.

### 2. `MAX_DATA_LEN` was wider than the protocol — since fixed

`frame.rs` assumption 1 — that `len` is one byte — is confirmed. The bound derived from it was
not:

| | Crate, as written | User Guide |
|---|---|---|
| Command data | 255 (`u8::MAX`) | **0 to 250 bytes** (§ 7.1) |
| Response data | 255 (`u8::MAX`) | **0 to 248 bytes** (§ 7.2) |
| Largest frame | `MAX_FRAME_LEN` = 262 | **255** either direction — 3+250+2 and 5+248+2 |

Both directions cap at 255 total, which reads like the protocol's design intent rather than a
coincidence. The crate accepted frames the module cannot legally send, and the docstring claim
that *"`MAX_FRAME_LEN` is 262 bytes and that is a property of the protocol"* described an
assumption rather than the protocol.

Not merely cosmetic: on a desynchronized stream, a length byte of 254 in a response made
`decode` return `Incomplete` and wait for 261 bytes that could never form a valid frame, where
the documented bound identifies it as malformed and resynchronizes. The CRC catches it either
way; the difference is how long the stream stays desynchronized.

**Fixed.** `MAX_DATA_LEN` became `MAX_COMMAND_DATA_LEN` (250) and `MAX_RESPONSE_DATA_LEN`
(248), `MAX_FRAME_LEN` is 255 with the two directions' agreement asserted at compile time, and
an over-long length byte is now `FrameError::DataTooLong` rather than a wait.

### 3. Antenna identity is not structurally unreachable

[#23](https://github.com/truppelli/splitforge/pull/23) raised this from a DigiKey forum answer
and deliberately did not act on forum sourcing. § 8.7 answers it from the user guide: one
*physical* monostatic port, and up to **16 logical ports through an external multiplexer**
driven by `portSwitchGpos` on the GPO lines.

So row 5 of the support checklist — scored *"Cannot — one RF port"* in
[hardware-plan § 2](../hardware-plan.md) and in [the reader notes](thingmagic-m7e-pico.md) — is
reachable, and "structural" is the wrong word for it. Three caveats travel with that, and they
are why this is recorded rather than rescored here:

- It needs a multiplexer that nothing has costed, and that is not in the bill of materials.
- One antenna is live at a time. Two checkpoints on one module time-share the radio, and a
  runner crossing while the switch is on the other port is a read that never happens. § 8.7's
  own note that the module does not support bistatic operation is the same constraint stated
  from the radio's side.
- It is still paper. Whether per-antenna identity survives into the tag-report stream in a form
  the adapter can map to a checkpoint is a measurement.

**Row 4 is untouched.** There is no reader clock, nothing in either document suggests one, and
no wiring changes that.

## What a second reading settled

The three findings above came from checking the codec's assumptions against the guide. These
four came from asking a different question of the same document — what the layer *above* the
codec needs — and the numbering continues because the cross-references do. Still no module
involved.

### 4. The 255-byte ceiling is confirmed from the hardware's side

[#25](https://github.com/truppelli/splitforge/pull/25) derived `MAX_FRAME_LEN` = 255 by adding up
§ 7.1 and § 7.2 — `3 + 250 + 2` and `5 + 248 + 2` — and observed that the two directions agreeing
*"reads like the protocol's design intent rather than a coincidence."*

§ 5.1.4.1 says it outright, and says it about the wire rather than the format: *"The connected
host processor's receiver must have the capability to receive up to 255 bytes of data at a time
without overflowing."* So the compile-time assertion that `MAX_FRAME_LEN == 255` is not only
arithmetic over two data-length caps; it is a stated hardware requirement on the host. Nothing in
the code changes. It is recorded because a confirmation costs nothing to write down and the next
person would otherwise re-derive it.

### 5. The timestamp semantic the notes said to verify first is confirmed

[The reader notes](thingmagic-m7e-pico.md#timestamps) describe the module's timestamp as *"a
relative millisecond timestamp within a continuous-read session […] not since power-on — it is
since the read session started"*, and flag it: *"Verify this first; the whole mapping below rests
on it."*

§ 8.8.3 verifies it, and sharpens *"session"* to something more specific: *"The time the tag was
read, relative to the time the command to read was issued, in milliseconds."* The anchor is the
read command. Mapping onto `ReaderTimestamp::Uptime { micros }` is unaffected — the field's unit
was always a conversion, not a claim about the module's resolution.

The sentence that follows it is new, and it is the sharper half:

> If the Tag Read Meta Data is not retrieved from the Tag Buffer between read commands, there
> will be no way to distinguish order of tags read with different read command invocations.

**Every read command starts a new epoch.** Two reads from either side of one are not comparable
as intervals, which is an argument for the session anchor
[hardware-plan step 3](../hardware-plan.md#step-3--the-timestamp-decision) already prescribes —
and a new argument for running an event on as few read commands as possible, which nothing had
said.

### 6. Antenna identity survives into the tag report, and costs a permissive change

Finding 3 left a caveat open: *"Whether per-antenna identity survives into the tag-report stream
in a form the adapter can map to a checkpoint is a measurement."* § 8.8.3 answers it on paper —
*"the Antenna ID entry will contain the logical antenna port of the tag read"* — and adds that a
tag seen on two antennas produces one buffer entry per antenna, which is the shape a checkpoint
mapping needs.

Still not free. § 8.7.2 names a cost nothing in this repository had priced:

> NOTE: Using an antenna multiplexer will require a Class 2 Permissive Change as trace routes to
> support antenna multiplexing are not covered under the existing regulatory certificates.

That lands on [hardware-plan § 6](../hardware-plan.md#6-phase-2--certification-and-manufacture)
rather than on M3a — a bench experiment is not a finished product seeking a grant — but the
multiplexer path is now a certification line item and not only a parts line item.

### 7. M3a's exit criterion may not be reachable on this interface

Recorded rather than acted on, and it is the finding here that matters most.

[M3a's exit criterion](../roadmap.md#milestone-3a--one-serial-reader) reads:

> a serial module runs for several hours while every read is preserved through deliberately
> induced disconnections and service restarts, and the count of reads the module believes it
> sent matches the count in the journal.

§ 8.8.2 says the module cannot *"detect a broken communications interface connection and stop
streaming the tag results"*, and § 5.1.4.1 says *"Flow control is not supported."* Put together:
during a deliberately induced disconnection the module goes on streaming into a cable that is not
there, and those reads are gone. Not delayed and not buffered — during streaming the buffer is
explicitly circular, and there are no control lines for either end to notice.

So the second clause cannot be satisfied by counting. Whatever the module believes it sent
includes the reads the disconnection ate, and the journal cannot contain them; the first clause
fails over the same window.

**This is a real difference between M3a and M3b**, and
[ADR-0024](../adr/0024-serial-reader-adapter-before-llrp.md) did not anticipate it. M3b's
identical wording is satisfiable because LLRP runs over TCP: the transport knows what it
delivered, the reader buffers while it cannot deliver, and *"the count the reader believes it
sent"* is a question with an answer. Over three wires with no flow control it is not — the
criterion quietly assumed a transport, and only one of the two adapters has it.

Three things could be true instead. Choosing between them is a separate review, not an edit here:

- **The criterion measures the wrong property for this interface**, and M3a should be asked for
  what a serial link can actually prove: no loss while connected, a loss across a disconnection
  that is bounded and *observed* rather than assumed, and a journal that never disagrees with
  what arrived.
- **The adapter should poll the tag buffer (§ 8.8.1) rather than stream**, which bounds the loss
  to a 52-entry FIFO and makes it countable, at a throughput cost nobody has measured — and
  against § 7's *"the reader never initiates"* discipline, which is what polling is for.
- **The criterion stands and M3a cannot close it**, the way two support-checklist rows already
  cannot.

What is not open is whether this was knowable. It was, from a document that had already been
archived and hashed, three findings deep into a file whose whole purpose is that the document
stops being available. It was found by reading the sections the *next* step needs rather than the
sections the current question pointed at.

## Adding a document here

One row per document, with a SHA-256 taken at retrieval, and the code or docs that depend on
it named. If the code depends on a specific claim, quote the claim — a URL that 404s two years
from now is the situation this file exists because of.
