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

## Adding a document here

One row per document, with a SHA-256 taken at retrieval, and the code or docs that depend on
it named. If the code depends on a specific claim, quote the claim — a URL that 404s two years
from now is the situation this file exists because of.
