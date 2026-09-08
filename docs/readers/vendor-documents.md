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

That left the highest-risk code in the project resting on a document nobody could open. The two
**PDFs** below were located on DigiKey's CDN, which is a distributor mirror and not the vendor —
so it can rotate too, and a hash is recorded for each.

The list has since grown past those two, and every addition has told the same story: the
MercuryAPI sources are on third-party GitHub mirrors because the vendor's own SDK download is
also a 404 now. **Not one document this project depends on is still retrievable from the
vendor.** That is the strongest argument this file can make for its own existence, and it was
not the argument it was created with.

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

### MercuryAPI — `serial_reader_l3.c`

The vendor's own implementation of the protocol above, and the authority on everything § 7
leaves out. Located after [the guide turned out to document no command set](#the-command-set-is-not-in-this-document).

| | |
|---|---|
| Title | Mercury API — serial reader low level implementation |
| Copyright | © 2009 ThingMagic, Inc. |
| **License** | **MIT** — *"Permission is hereby granted, free of charge, to any person obtaining a copy of this software […] without restriction"* |
| Source | `https://raw.githubusercontent.com/ppelleti/mercuryapi-corrections/master/serial_reader_l3.c` |
| Retrieved | 2026-08-30, HTTP 200 |
| Size | 208,039 bytes |
| SHA-256 | `97b74cb184068bb1c9841f9f1e13ceedc1d9593fe1a88690787ea4f4e7d66d79` |
| Depended on by | `crates/splitforge-thingmagic/src/crc.rs`; [tag-report layout](#tag-report-layout--serial_reader_l3c-tmr_sr_parsemetadatafrommessage) rows 1–9 |

**There is a second `serial_reader_l3.c` in this file.** The 2023 one is recorded under
[MercuryAPI 2023](#mercuryapi-2023--four-files) and is 84 KB larger. Rows 1–9 of the tag-report
table were taken from *this* 2009 copy and then cross-checked against that one; rows 10–14 exist
only there. Cite by hash, not by filename.

**The license answers the question the section below raised.** MIT is compatible with
GPL-3.0-or-later, so this repository may read, adapt, and incorporate this code with attribution
— which is what makes the command set reachable at all. The copy above is a third-party mirror
rather than a vendor distribution; the license text is in the file's own header, and the hash is
recorded so a vendor-supplied copy can be compared against the one the code was written against.

### MercuryAPI — `serial_reader_imp.h`

Where the opcode *values* live. `serial_reader_l3.c` references 53 distinct
`TMR_SR_OPCODE_*` symbols 123 times and defines none of them — see
[finding 9](#9-the-command-set-is-spread-across-three-files-and-one-was-archived).

| | |
|---|---|
| Title | Mercury API — serial reader internal implementation header |
| Copyright | © 2009 ThingMagic, Inc. |
| **License** | **MIT** — same grant as `serial_reader_l3.c`, verified in the file's own header |
| Source | `https://raw.githubusercontent.com/ppelleti/mercuryapi-corrections/master/serial_reader_imp.h` |
| Retrieved | 2026-08-31, HTTP 200 |
| Size | 53,830 bytes |
| SHA-256 | `5bc6a7dd91947ae5c0aef1bdc8fdaeb87ba3e0d17d2670324ff6b5b5696365c0` |
| Depended on by | [The command set, from the SDK](#the-command-set-from-the-sdk) |

### MercuryAPI — `tmr_utils.h`

The accessor macros the tag-report layout is expressed in, and therefore the authority on
**byte order**: `GETU16AT`, `GETU24AT`, and `GETU32AT` all shift the first byte highest, so
every multi-byte field in a response is big-endian.

| | |
|---|---|
| Title | Mercury API — utility macros |
| Copyright | © 2009 ThingMagic, Inc. |
| **License** | **MIT**, verified in the file's own header |
| Source | `https://raw.githubusercontent.com/ppelleti/mercuryapi-corrections/master/tmr_utils.h` |
| Retrieved | 2026-08-31, HTTP 200 |
| Size | 5,188 bytes |
| SHA-256 | `0f423f6b5219e23596c308d438324df9b217e8a9a8d6a5b9d513212f22cd6d37` |
| Depended on by | [The command set, from the SDK](#the-command-set-from-the-sdk) |

### MercuryAPI 2023 — four files

**A current SDK, and the answer to what
[finding 9](#9-the-command-set-is-spread-across-three-files-and-one-was-archived) recorded as
missing.** These carry the `TMR_TRD_METADATA_FLAG_*` values, the `TMR_SR_STATUS_*` values, the
byte that separates a tag frame from a status frame, and the five tag-report fields the 2009
parser does not have — none of which the 2009 mirror could supply.

**The vendor's own distribution is gone.** `python-mercuryapi`'s build fetches
`mercuryapi-AHAB-1.35.2.72-1.zip` from `jadaktech.com/wp-content/uploads/2022/08/`, which now
returns **HTTP 404** — the same fate as the user-guide PDFs that
[made this file necessary](#why-this-file-exists). So the version below is named by its
copyright rather than by a version string: the sources carry **© 2023 Novanta**, which is the
same vendor and year as the archived user guide, and no file in the tree states a release
number.

A third-party mirror again, and pinned to a **commit** rather than a branch so the URLs cannot
drift the way `master` can.

| | |
|---|---|
| Title | Mercury API — tag data, serial reader header, serial reader, low level implementation |
| Copyright | © 2023 Novanta, Inc. |
| **License** | **MIT** — the same grant as the 2009 files, verified in each file's own header |
| Mirror | `Commutyble/thingmagic-client`, pinned at `0b16964089c3a4234209cb9d04979d276f62a2e0` |
| Retrieved | 2026-09-08, HTTP 200 |
| Depended on by | [Tag-report layout](#tag-report-layout--serial_reader_l3c-tmr_sr_parsemetadatafrommessage), [Metadata flags](#metadata-flags--tmr_tag_datah-enum-tmr_trd_metadataflag), [Status reports](#status-reports--tmr_serial_readerh-and-serial_readerc) |

| File | Path in the tree | Size | SHA-256 |
|---|---|---|---|
| `tmr_tag_data.h` | `c/src/api/tmr_tag_data.h` | 8,906 bytes | `d5352715aa7eec66f879aa29b92ed8586cc7013f93fcb52e3274ee1ea822003f` |
| `tmr_serial_reader.h` | `c/src/api/tmr_serial_reader.h` | 14,748 bytes | `8a709d14a39bfcc1b540178e5fe3c551b700222f99e1c31b39df12d2e8b58fdb` |
| `serial_reader.c` | `c/src/api/serial_reader.c` | 242,673 bytes | `852544644c6384d1a4ee35e09ca126efbf1442af278c1dfe06ed1df093e90573` |
| `serial_reader_l3.c` | `c/src/api/serial_reader_l3.c` | 292,434 bytes | `3db28019080e98fcabed942d453498c84aa95ecdb889cfa6e6ac4f1d8793e57b` |

**The fourth file is the same name as the 2009 one recorded above, and that is the point.**
Rows 10–14 of the tag-report table and the whole of
[finding 13](#13-the-2009-field-order-is-a-prefix-of-the-modern-one) come from *this* copy of
`serial_reader_l3.c`, not from the 208 KB one. Two files fourteen years apart with the same
name is exactly the situation a hash exists for: 292,434 bytes against 208,039, and a different
digest, so there is no way to cite one and mean the other by accident.

Raw URLs take the form:

```text
https://raw.githubusercontent.com/Commutyble/thingmagic-client/0b16964089c3a4234209cb9d04979d276f62a2e0/c/src/api/tmr_tag_data.h
```

**Two of the three tables below were cross-checked against the 2009 mirror and agree exactly**,
which is the strongest corroboration available without a vendor copy — two mirrors, fourteen
years apart, with no common maintainer. Where they differ, they differ by *addition*, and that
difference is recorded in [finding 13](#13-the-2009-field-order-is-a-prefix-of-the-modern-one).

### SparkFun Simultaneous RFID Tag Reader Library

| | |
|---|---|
| Title | `SparkFun_UHF_RFID_Reader.cpp` |
| License | MIT |
| Source | `https://raw.githubusercontent.com/sparkfun/SparkFun_Simultaneous_RFID_Tag_Reader_Library/master/src/SparkFun_UHF_RFID_Reader.cpp` |
| Retrieved | 2026-08-30, HTTP 200 |
| Size | 31,398 bytes |
| SHA-256 | `3269d53c3156abb7a7af3c9960a186eace8c4e2b0bfa41b2f39ba72a2d107f18` |
| Depended on by | `crates/splitforge-thingmagic/src/crc.rs` — `CAPTURED_FRAME` |

Not an independent implementation: its CRC is copied from `serial_reader_l3.c` and carries the
same comment. What it adds is a **captured frame** — a real `0x22` response from a real module,
annotated field by field, including the CRC that module computed. That frame is the only thing
in `splitforge-thingmagic` anchored outside the crate, and it is what caught the defect in
[finding 8](#8-the-crc-was-not-ccitt-false-and-the-codec-computed-the-wrong-checksum).

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

**The MercuryAPI and SparkFun sources above are a different case entirely.** All of them are
MIT, which permits copying and adaptation outright, so nothing forbids vendoring them. They are
still not committed here, for a reason that is engineering rather than legal: this repository
implements the protocol in Rust with its own tests, and a C file sitting beside it would be a
second source of truth that nothing compiles or checks. What is taken from them is recorded
where it is used — the algorithm in `crc.rs`, the captured frame in `CAPTURED_FRAME`, the opcode
and flag tables below — with attribution in the docstring rather than a copied file.

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
source, and that the next source is *code* rather than a specification. Code carries a question a
PDF did not: [ADR-0007](../adr/0007-license-selection.md) makes this repository
GPL-3.0-or-later, so what the SDK's license permits had to be established before reading it into
a design.

**It is MIT** — established, not assumed, from the license header of the file now recorded under
[The documents](#the-documents). MIT is GPL-3.0 compatible, so the command set is reachable with
attribution. The first thing read out of it was not an opcode but the CRC, and that is
[finding 8](#8-the-crc-was-not-ccitt-false-and-the-codec-computed-the-wrong-checksum).

## The command set, from the SDK

What the user guide leaves out, recovered from the three MIT-licensed files recorded above.
These are **facts about a wire protocol** — that opcode `0x22` means "read tag ID multiple" —
which are not copyrightable, and they are reproduced here rather than vendored for the reason
[Why the bytes are not here](#why-the-bytes-are-not-here) already gives: a C file beside the
Rust would be a second source of truth that nothing compiles.

### Opcodes — `serial_reader_imp.h`, `enum TMR_SR_OpCode`

Transcribed complete rather than filtered to the ones the read path needs, because the value of
an exhaustive table is that the next person can tell an unknown opcode from an unlisted one.

| Op | Name | Op | Name |
|---|---|---|---|
| `0x01` | `WRITE_FLASH` | `0x2A` | `CLEAR_TAG_ID_BUFFER` |
| `0x02` | `READ_FLASH` | `0x2D` | `WRITE_TAG_SPECIFIC` |
| `0x03` | `VERSION` | `0x2E` | `ERASE_BLOCK_TAG_SPECIFIC` |
| `0x04` | `BOOT_FIRMWARE` | `0x2F` | `MULTI_PROTOCOL_TAG_OP` |
| `0x06` | `SET_BAUD_RATE` | `0x61` | `GET_ANTENNA_PORT` |
| `0x07` | `ERASE_FLASH` | `0x62` | `GET_READ_TX_POWER` |
| `0x08` | `VERIFY_IMAGE_CRC` | `0x63` | `GET_TAG_PROTOCOL` |
| `0x09` | `BOOT_BOOTLOADER` | `0x64` | `GET_WRITE_TX_POWER` |
| `0x0A` | `MODIFY_FLASH` | `0x65` | `GET_FREQ_HOP_TABLE` |
| `0x0B` | `GET_DSP_SILICON_ID` | `0x66` | `GET_USER_GPIO_INPUTS` |
| `0x0C` | `GET_CURRENT_PROGRAM` | `0x67` | `GET_REGION` |
| `0x0D` | `WRITE_FLASH_SECTOR` | `0x68` | `GET_POWER_MODE` |
| `0x0E` | `GET_SECTOR_SIZE` | `0x69` | `GET_USER_MODE` |
| `0x0F` | `MODIFY_FLASH_SECTOR` | `0x6A` | `GET_READER_OPTIONAL_PARAMS` |
| `0x10` | `HW_VERSION` | `0x6B` | `GET_PROTOCOL_PARAM` |
| `0x21` | `READ_TAG_ID_SINGLE` | `0x6C` | `GET_READER_STATS` |
| `0x22` | `READ_TAG_ID_MULTIPLE` | `0x6D` | `GET_USER_PROFILE` |
| `0x23` | `WRITE_TAG_ID` | `0x70` | `GET_AVAILABLE_PROTOCOLS` |
| `0x24` | `WRITE_TAG_DATA` | `0x71` | `GET_AVAILABLE_REGIONS` |
| `0x25` | `LOCK_TAG` | `0x72` | `GET_TEMPERATURE` |
| `0x26` | `KILL_TAG` | `0x91` | `SET_ANTENNA_PORT` |
| `0x28` | `READ_TAG_DATA` | `0x92` | `SET_READ_TX_POWER` |
| `0x29` | `GET_TAG_ID_BUFFER` | `0x93` | `SET_TAG_PROTOCOL` |
| `0x94` | `SET_WRITE_TX_POWER` | `0x99` | `SET_USER_MODE` |
| `0x95` | `SET_FREQ_HOP_TABLE` | `0x9A` | `SET_READER_OPTIONAL_PARAMS` |
| `0x96` | `SET_USER_GPIO_OUTPUTS` | `0x9B` | `SET_PROTOCOL_PARAM` |
| `0x97` | `SET_REGION` | `0x9D` | `SET_USER_PROFILE` |
| `0x98` | `SET_POWER_MODE` | `0x9E` | `SET_PROTOCOL_LICENSEKEY` |
| `0xC1` | `SET_OPERATING_FREQ` | `0xC3` | `TX_CW_SIGNAL` |

Names are shortened from `TMR_SR_OPCODE_*`. **`0x22` is the read path**, and it is also the
opcode of the captured frame already anchoring `crc.rs` — which cross-checks this table against
something that was in the repository before it.

### Search flags — `serial_reader_imp.h`, `enum TMR_SR_SearchFlag`

The second argument to `0x22`, and what turns a one-shot inventory into a stream:

| Value | Name | Why it matters here |
|---|---|---|
| `0x0000` | `CONFIGURED_ANTENNA` | |
| `0x0003` | `ANTENNA_MASK` | Low two bits select the antenna scheme |
| `0x0004` | `EMBEDDED_COMMAND` | |
| `0x0008` | `TAG_STREAMING` | **The streaming mode** [ADR-0025](../adr/0025-m3a-proves-durability-above-the-transport.md) chose |
| `0x0010` | `LARGE_TAG_POPULATION_SUPPORT` | Set unconditionally by the SDK |
| `0x0020` | `STATUS_REPORT_STREAMING` | See [finding 12](#12-a-liveness-signal-may-exist-after-all-and-adr-0025-assumed-it-did-not) |
| `0x0040` | `RETURN_ON_N_TAGS` | Sync read only — the SDK refuses it while streaming |
| `0x0080` | `READ_MULTIPLE_FAST_SEARCH` | |
| `0x0100` | `STATS_REPORT_STREAMING` | Mutually exclusive with `STATUS_REPORT_STREAMING` |
| `0x0200` | `GPI_TRIGGER_READ` | |
| `0x0400` | `DUTY_CYCLE_CONTROL` | |

### Tag-report layout — `serial_reader_l3.c`, `TMR_SR_parseMetadataFromMessage`

The layout § 8.8.3 deferred on. A **flags word selects which fields are present**, and the
present ones appear in exactly this order — so the parser is a sequence of conditional reads,
not a fixed struct. Multi-byte fields are big-endian, per `tmr_utils.h`.

**Two sources, and the table says which.** Rows 1–9 come from the **2009** `serial_reader_l3.c`
and were cross-checked against the 2023 copy, where they are identical in flag value and in
order. Rows 10–14 exist only in the **2023** copy. Both are recorded under
[The documents](#the-documents) with separate hashes, because they share a filename and differ
by 84 KB — see [finding 13](#13-the-2009-field-order-is-a-prefix-of-the-modern-one).

| Order | Flag | Field | Width | Notes |
|---|---|---|---|---|
| 1 | `0x0001` | read count | `u8` | |
| 2 | `0x0002` | RSSI | `i8` | Signed — read as `(int8_t)`, and dBm is negative |
| 3 | `0x0004` | antenna ID | `u8` | **Not an antenna number** — see [finding 10](#10-the-antenna-byte-is-a-packed-txrx-nibble-pair-not-an-antenna-number) |
| 4 | `0x0008` | frequency | `u24` | |
| 5 | `0x0010` | timestamp | `u32` | Relative to the read command; see [finding 11](#11-mercuryapi-anchors-the-relative-timestamp-exactly-as-adr-0024-prescribes) |
| 6 | `0x0020` | phase | `u16` | |
| 7 | `0x0040` | protocol | `u8` | Must be *kept* — three later fields are conditional on it |
| 8 | `0x0080` | data | `u16` bit-count, then bytes | Length is in **bits**, converted by `tm_u8s_per_bits` |
| 9 | `0x0100` | GPIO status | `u8` | Bit per pin |
| 10 | `0x0200` | Gen2 Q | `u8` | **Gen2 only** — skipped entirely for another protocol |
| 11 | `0x0400` | Gen2 link frequency | `u8` | Gen2 only. An enum: `0x00`/`0x02`/`0x04` → 250/320/640 kHz |
| 12 | `0x0800` | Gen2 target | `u8` | Gen2 only. `0x00` → A, `0x01` → B |
| 13 | `0x1000` | brand identifier | 2 bytes | **Carved out of the EPC** — the parser subtracts 2 from the EPC byte count |
| 14 | `0x2000` | tag type | **EBV**, variable | Extensible Bit Vector, via `parseEBVdata` — the one field with no fixed width |
| — | — | EPC | `u16` bit-count, then bytes | Always present, after the flagged fields |

**The order is the flag bits ascending**, which is worth stating as a rule rather than as a
table to memorize: the parser tests each flag in turn from `0x0001` upward and consumes the
field if set. A decoder that walks the bits in order cannot get the sequence wrong.

Two of these break the pattern of "a flag selects a fixed-width field", and both are traps:

- **Flags 10–12 are also conditional on the protocol.** `serial_reader_l3.c` wraps them in
  `if (TMR_TAG_PROTOCOL_GEN2 == read->tag.protocol)`, so the flag being set is *not* sufficient
  to consume the byte. The protocol comes from field 7, earlier in the same record — which is
  why field 7 has to be retained rather than skipped over.
- **Tag type is variable-length.** Every other field can be skipped by advancing a known number
  of bytes; this one cannot be skipped without decoding it.

The EPC length is also a **bit** count, and for Gen2 the EPC is followed by a two-byte PC word
and then a CRC — with a third PC byte when `pc[0] & 0x02` is set.

### Metadata flags — `tmr_tag_data.h`, `enum TMR_TRD_MetadataFlag`

The flags word above, in full. **This is what
[finding 9](#9-the-command-set-is-spread-across-three-files-and-one-was-archived) recorded as
the missing piece**, and the reason `TagReportDecoder` shipped as a trait with no
implementation.

| Value | Name |
|---|---|
| `0x0000` | `NONE` |
| `0x0001` | `READCOUNT` |
| `0x0002` | `RSSI` |
| `0x0004` | `ANTENNAID` |
| `0x0008` | `FREQUENCY` |
| `0x0010` | `TIMESTAMP` |
| `0x0020` | `PHASE` |
| `0x0040` | `PROTOCOL` |
| `0x0080` | `DATA` |
| `0x0100` | `GPIO_STATUS` |
| `0x0200` | `GEN2_Q` |
| `0x0400` | `GEN2_LF` |
| `0x0800` | `GEN2_TARGET` |
| `0x1000` | `BRAND_IDENTIFIER` |
| `0x2000` | `TAGTYPE` |

`ALL` is not a fixed constant: the 2023 header composes it from `#ifdef TMR_ENABLE_UHF` and
`#ifdef TMR_ENABLE_HF_LF`, so its value depends on how the SDK was compiled. **Nothing in this
repository should use `ALL`** — the flags word arrives on the wire and is read from there.

### Status reports — `tmr_serial_reader.h` and `serial_reader.c`

The other half of what finding 9 could not supply, and the half
[Q14](../open-questions.md#q14-reader-silence-threshold) turns on.

`enum TMR_SR_StatusType` — the *content* of a status report, requested in the `0x22` command
body when `STATUS_REPORT_STREAMING` is set:

| Value | Name |
|---|---|
| `0x0000` | `NONE` |
| `0x0002` | `FREQUENCY` |
| `0x0004` | `TEMPERATURE` |
| `0x0008` | `ANTENNA` |
| `0x000E` | `ALL` (the three above) |

`0x0001` is **not assigned**, in both the 2009 and 2023 copies. Nothing explains the hole and
nothing here depends on it; it is recorded so the next reader does not assume a typo.

**How a status frame is told apart from a tag frame** — `serial_reader.c`,
`TMR_SR_hasMoreTags`. A streaming response carries a **response-type byte**, and the SDK
switches on it:

| Value | Meaning |
|---|---|
| `0x00` | The stream **ends** with this message |
| `0x01` | A tag read; the stream continues |
| `0x02` | A **status** stream response |

Its position is not fixed:

```c
response_type_pos = (0x10 == (msg[5 + idx] & 0x10)) ? (10 + idx) : (8 + idx);
```

…where `idx` is 1 when multi-select or read-after-write is enabled and 0 otherwise. Neither is a
`TMR_SR_SearchFlag`, and `crates/splitforge-thingmagic/src/command.rs` offers no way to request
either — so on the commands this project can currently build, `idx` is 0. The expression is
recorded whole regardless, because a decoder that hard-codes offset 8 is one that silently
misreads every frame the day somebody adds a feature that shifts it.

**This is a decoding hazard, not a nicety.** A status frame parsed as a tag report would produce
a read with a fabricated EPC — evidence about a chip that was never there, in an append-only
table. The response-type byte has to be checked *before* the metadata flags word is read.

## What the read path will depend on, quoted

> **The *"will"* is now half wrong, and the heading keeps it anyway.** ADR-0025 and
> `hardware-plan.md` both link to this anchor, and ADR-0025 is Accepted — the process in
> [docs/adr/README.md](../adr/README.md) does not permit editing an accepted ADR to chase a
> renamed heading. A stale word costs less than two broken links.

**Half of this now backs code that exists.** `crates/splitforge-thingmagic/src/port.rs` opens a
port at 115,200 baud with a test asserting the default, and the `ReaderProvider` above it
implements the reconnect behavior § 8.8.2 forces by saying the module *"cannot detect a broken
communications interface connection."* What is still ahead of the code is the tag-report
reading — § 8.8.3 — because `TagReportDecoder` has no implementation yet.

These were recorded while the document was open, before any of it was needed, because three of
them constrain the design before a line of it is written. That turned out to be the right call:
§ 5.1.4.1's *"flow control is not supported"* is what
[finding 7](#7-m3as-exit-criterion-may-not-be-reachable-on-this-interface) rests on, and it was
read a milestone before anything could act on it.

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

**The other half of this finding has since been settled, and settled against the code.** § 7.3
names the algorithm only as "CCITT CRC-16" and gives no polynomial, seed, or worked example.
That name is wrong, `crc.rs` implemented it, and the codec computed a checksum no module would
have accepted — see
[finding 8](#8-the-crc-was-not-ccitt-false-and-the-codec-computed-the-wrong-checksum). The
*coverage* confirmed above was never the part in doubt.

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

> **Corrected in part by [finding 10](#10-the-antenna-byte-is-a-packed-txrx-nibble-pair-not-an-antenna-number).**
> The conclusion below holds — antenna identity does survive into the report — but *"in a form
> the adapter can map to a checkpoint"* is doing more work than it looks. The byte is a packed
> tx/rx nibble pair, not the logical port § 8.8.3 describes.

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

**Decided by [ADR-0025](../adr/0025-m3a-proves-durability-above-the-transport.md): the first,
with the third applied to the unreachable clause alone.** The criterion is restated around what
a serial link can prove — no loss above the transport, a journal that never disagrees with what
arrived, and every disconnection detected and recorded as a bounded gap — while *"the count of
reads the module believes it sent"* is named structurally unclosable here and kept verbatim in
M3b. The review also **rejected the second**, and on a cost this section had underpriced: § 8.8.1
deduplicates in hardware, so polling does not merely cost throughput, it removes the burst every
`SelectionRule` selects from and makes Milestone 1's exit criterion unobservable on this adapter.
The 52-entry ceiling bounds distinct tags rather than duplicates, which is the worse thing to
lose. What polling would have bought — a command-response round trip is a free liveness check —
is what the new third clause now has to build by hand.

What is not open is whether this was knowable. It was, from a document that had already been
archived and hashed, three findings deep into a file whose whole purpose is that the document
stops being available. It was found by reading the sections the *next* step needs rather than the
sections the current question pointed at.

## What the SDK settled

One finding, and it is the reason this file exists at all.

### 8. The CRC was not CCITT-FALSE, and the codec computed the wrong checksum

`crc.rs` implemented **CRC-16/CCITT-FALSE** — polynomial `0x1021`, seed `0xFFFF`, no reflection,
anchored on the catalogue's published check vector `crc16(b"123456789") == 0x29B1`. That is a
real, correct, well-known checksum. It is not the one this protocol uses.

MercuryAPI's `serial_reader_l3.c` says so in a comment above the function:

> ThingMagic-mutated CRC used for messages. Notably, not a CCITT CRC-16, though it looks close.

The difference is one term. Both feed the register four bits at a time through the same table,
`T[i] = i * 0x1021`; the standard folds the data nibble into the *table index*, and ThingMagic
shifts it into the *bottom of the register* instead:

```text
CCITT-FALSE  crc = (crc << 4) ^ T[(crc >> 12) ^ nibble]  =  (crc << 4) ^ T[crc >> 12] ^ T[nibble]
ThingMagic   crc = ((crc << 4) | nibble) ^ T[crc >> 12]  =  (crc << 4) ^ T[crc >> 12] ^ nibble
```

`T[nibble]` against `nibble`. The two functions agree on no input longer than nothing.

**Proved against real hardware, not against the source.** SparkFun's library documents a captured
`0x22` response, field by field, including the CRC the module put on it:

```text
FF 28 22 00 00 10 00 1B 01 FF 01 01 C4 11 0E 16 40 00 00 01 27 00 00 05 ...
                                                            ... 15 45 E9 4A 56 1D
                                                                          ^^^^^ message CRC
```

Over the 44 bytes § 7.3 says the CRC covers — length, opcode, status, data — the module's answer
is `0x561D`. ThingMagic's algorithm computes `0x561D`. CCITT-FALSE computes `0xF542`.

So the codec would have failed on the first frame it ever saw. Every command it sent would have
been rejected, and every response it received would have looked corrupt — on a $345 board, in a
field, with the frame parser being the last place anybody would look, because it had
thirty-four passing tests.

**Nothing in the crate could have caught it, and the crate said so.** `crc.rs` opened with the
warning that this was *"the part most likely to be wrong in a way no test here can detect"*, and
`lib.rs` with *"a parser that is internally consistent and externally wrong passes every test in
this crate."* Both were exactly right. The frame tests build a frame with `crc16` and then check
it with `crc16`; they pass identically with either function, and they did. The one test that
looked like an external anchor — the `0x29B1` check vector — anchored the crate to the wrong
function's catalogue entry, which is worse than no anchor, because it reads like verification.

**Fixed**, with the algorithm derived from the polynomial rather than copied as a table, and
anchored on the captured frame above. `0xF542` is now pinned as a regression test under a name
that says why: § 7.3 still calls this "CCITT CRC-16", so the next person has the same invitation
to implement the wrong thing.

Three things worth keeping from this:

- **The captured frame is an M6e response, not an M7e-Pico one.** The modules differ; § 7.1–7.3's
  framing does not, and this crate's decoder parses it. That is evidence about the protocol, and
  it is recorded in `CAPTURED_FRAME`'s docstring as exactly that rather than as a Pico capture.
- **A vendor's name for an algorithm is not a specification.** "CCITT CRC-16" is a description of
  what it resembles. The polynomial is shared, which is why it resembles it; nothing else is.
- **This is the argument for the whole approach, tested.** The plan was to write the parser from
  documentation before buying hardware, on the theory that a bug found at a desk is cheaper than
  one found in a field. The parser was wrong, and it was found at a desk, before the order.

## What reading the command set settled

Four findings from the pass that went after the opcodes rather than the CRC. Two of them correct
things this repository had already written down.

### 9. The command set is spread across three files, and one was archived

`serial_reader_l3.c` was recorded here as *"the authority on everything § 7 leaves out."* It is
the authority on the **protocol's behavior** and not on its **constants**. It references 53
distinct `TMR_SR_OPCODE_*` symbols, 123 times, and defines none of them — they live in
`serial_reader_imp.h`, and the accessor macros that fix byte order live in `tmr_utils.h`. Both
are now recorded above with hashes.

**The mirror is not internally consistent, either.** The same repository's `tm_reader.h` is a
2009 file that contains **zero** of the modern symbols `serial_reader_l3.c` depends on —
`TMR_SR_STATUS_*`, `TMR_TRD_METADATA_FLAG_*`, `dspMicros`. So the C file is a late version and at
least one of its headers is an early one. That is not a problem for the facts taken above, all of
which came from files that agree with each other, and it **is** a problem for anything taken from
`tm_reader.h`, which is why nothing was and why it is not recorded as a document.

**What is still missing**, and named so the next reader does not assume it was checked: the
numeric values of `TMR_TRD_METADATA_FLAG_*` and `TMR_SR_STATUS_*`. The tag-report field *order* is
established, from the parser itself; which bit selects which field is not.

> **Closed, 2026-09-08 — and the premise above was wrong twice.**
>
> Both symbol sets are now recorded under
> [Metadata flags](#metadata-flags--tmr_tag_datah-enum-tmr_trd_metadataflag) and
> [Status reports](#status-reports--tmr_serial_readerh-and-serial_readerc). Two corrections
> come with them, and both are worth keeping because the mistake was in the *diagnosis* rather
> than in the effort:
>
> - **Neither symbol was ever in `tm_reader.h`.** `TMR_TRD_METADATA_FLAG_*` is in
>   `tmr_tag_data.h` and `TMR_SR_STATUS_*` is in `tmr_serial_reader.h`. The note above blamed
>   the mirror's 2009 vintage for an absence that was really a wrong filename — the age of the
>   copy was a true fact standing next to a false inference. **The 2023 `tm_reader.h` contains
>   neither symbol either**, which is the check that settles it: the file was never where they
>   live, in any vintage, so no newer copy of *that file* would ever have closed this.
> - **`TMR_SR_STATUS_*` was in the mirror this file already cites, the whole time.**
>   `ppelleti/mercuryapi-corrections/tmr_serial_reader.h` carries it, with values identical to
>   the 2023 SDK's. It was never missing; it was never looked for in the right file.
>
> The lesson is narrow and worth stating: *"the symbol is not in the file I expected"* was
> recorded as *"the symbol is not in this distribution"*, and the second is a much stronger
> claim than what had been checked. A grep across the tree would have closed this in a minute;
> a grep of one file closed nothing and read like a conclusion.

### 10. The antenna byte is a packed tx/rx nibble pair, not an antenna number

§ 8.8.3 says *"the Antenna ID entry will contain the logical antenna port of the tag read"*, and
[finding 6](#6-antenna-identity-survives-into-the-tag-report-and-costs-a-permissive-change) took
that at face value. The parser reads the field as a plain `u8` — and then
`TMR_SR_postprocessReaderSpecificMetadata` immediately takes it apart:

```c
tx = (read->antenna >> 4) & 0xF;
rx = (read->antenna >> 0) & 0xF;

// Due to limited space, Antenna 16 wraps around to 0
if (0 == tx) { tx = 16; }
if (0 == rx) { rx = 16; }
```

The byte is **two nibbles**, a transmit port and a receive port, which are then looked up in the
reader's `txRxMap` to produce the logical antenna the operator configured. A `0` nibble means
16, not 0.

**This is finding 8's shape exactly**: a field whose documented description is true of the value
you end up with and false of the bytes on the wire. An adapter that mapped this byte straight to
`ReaderMessage::antenna` would produce antenna `17` for a tag read on tx 1 / rx 1, and it would do
so consistently enough to look like a working mapping. Per-antenna identity is still reachable —
finding 6 stands — but it costs a nibble split and a map, not a cast.

### 11. MercuryAPI anchors the relative timestamp exactly as ADR-0024 prescribes

[ADR-0024](../adr/0024-serial-reader-adapter-before-llrp.md) decided that *"every connect
captures a session anchor `(received_at_utc, module_relative_us)`"*, and
[finding 5](#5-the-timestamp-semantic-the-notes-said-to-verify-first-is-confirmed) established
that the module's value is relative to the read command. The SDK does the same thing, and it is
worth recording that the design was arrived at independently and agrees:

```c
/* Cache the read time so it can be put in tag read data later */
tm_gettime_consistent(&starttimeHigh, &starttimeLow);
reader->u.serialReader.readTimeHigh = starttimeHigh;
reader->u.serialReader.readTimeLow  = starttimeLow;
```

…and then, per tag, `timestampLow = sr->readTimeLow + read->dspMicros`. The host's clock at
command time, plus the module's relative offset. That is the anchor, in the vendor's own
implementation.

**One thing it does *not* settle, and the name is a trap.** The field is called `dspMicros`, and
§ 8.8.3 says the timestamp is *"in milliseconds."* They cannot both be right, and the addition
above is only dimensionally correct if `dspMicros` shares the unit of `tmr_gettime_low()` —
which is in a platform file that is **not** in the mirror and has not been read. The evidence
points at milliseconds: the vendor's own prose says so, and MercuryAPI's tag timestamps are
milliseconds since the epoch. That is an inference, not a reading, and it is recorded as a named
assumption for the same reason the CRC was: **a 1000× error here would look exactly like a
working timestamp** on a bench and fail only on the arithmetic. Confirm it against a capture, or
against `osdep`, before trusting `ReaderTimestamp::Uptime { micros }`'s conversion.

### 12. A liveness signal may exist after all, and ADR-0025 assumed it did not

[ADR-0025](../adr/0025-m3a-proves-durability-above-the-transport.md) accepts, as a stated cost of
choosing to stream, that *"streaming has no liveness signal, and a quiet stream is
indistinguishable from a quiet checkpoint."* That was the right reading of the **user guide**,
which describes streaming only as tags being *"pushed out of the buffer as soon as they are put
into the buffer."*

The SDK has two more search flags:

```c
TMR_SR_SEARCH_FLAG_STATUS_REPORT_STREAMING = 32,
TMR_SR_SEARCH_FLAG_STATS_REPORT_STREAMING  = 256,
```

…and `serial_reader_l3.c` has a branch, inside the continuous-read receive path, for *"a status
stream response"* — a frame that arrives during streaming and carries reader statistics rather
than a tag. The SDK sets one of the two whenever the caller has asked for stats, and the two are
mutually exclusive.

**This does not close [Q14](../open-questions.md#q14-reader-silence-threshold), and it changes
what Q14 is asking.** What is established is that a non-tag frame *can* arrive during a stream.
What is not established is whether it arrives **periodically**, on what interval, and whether it
arrives at all when no tags are in the field — which is the only property that makes it a
keepalive. The `TMR_SR_STATUS_*` content flags that would say are in the header the mirror does
not carry a current copy of ([finding 9](#9-the-command-set-is-spread-across-three-files-and-one-was-archived)).

So ADR-0025's accepted cost is **stated too absolutely** and was corrected in place — it was
Proposed at the time, and the ADR process permits editing a Proposed one. The honest form is
that no liveness signal is *known* to be available, and there is a named candidate whose
periodicity nobody has established.

> **That window has closed.** ADR-0025 is **Accepted** now, and
> [docs/adr/README.md](../adr/README.md) permits no further in-place edits: *"ADRs are not
> edited after acceptance except to change status. A decision that no longer holds gets a new
> ADR that supersedes it."* The sentence above is kept in the past tense rather than deleted,
> because as written in the present tense it read as standing permission — and the next person
> to find something else ADR-0025 states too absolutely would have taken it.

> **Updated 2026-09-08, and Q14 is still open.** The `TMR_SR_STATUS_*` content flags this
> section said were unavailable are now recorded, and a status frame turns out to be
> unambiguously identifiable on the wire — response-type byte `0x02`, checked before the
> metadata flags are read. So the *mechanics* are settled: SplitForge could ask for status
> reports, and would know one when it saw one.
>
> **What the SDK still cannot say is the only thing Q14 needs.** Nothing in these sources
> states an interval, and nothing states whether a status frame arrives when no tags are in the
> field. The SDK sets a flag and the module's firmware decides the cadence; there is no period
> parameter to read. So the candidate keepalive is now a *reachable* candidate rather than an
> unreachable one, and it is still not known to be a keepalive.
>
> Two routes remain, and both need the module: enable `STATUS_REPORT_STREAMING` against a bench
> module with an empty field and time the frames, or read the firmware's own documentation if a
> copy that covers it ever surfaces. **This is now hardware-gated rather than document-gated**,
> which is a change in *which* queue the question is in and not an answer to it.
>
> One consequence is worth stating now rather than discovering later: **a status frame is not a
> read**, so it must not reach the journal — but it *is* evidence the transport is alive, which
> is exactly what the silence watchdog is guessing at. Wiring it to `last_message` without
> writing a row would let a quiet checkpoint be distinguished from a dead module. That is a
> design worth having and it is **not** built, because building it against an unmeasured cadence
> would bake in the assumption Q14 exists to test.

### 13. The 2009 field order is a prefix of the modern one

The tag-report layout recorded above came from the 2009 `serial_reader_l3.c`, and ends at GPIO
status with the EPC after it. The 2023 parser has **five more flagged fields** between those
two — Gen2 Q, Gen2 link frequency, Gen2 target, brand identifier, and tag type.

That is not a discrepancy; it is a version difference, and it resolves in the reassuring
direction. The nine fields the two copies share have **identical flag values and identical
order**, fourteen years apart. The new ones are appended at higher bits, which is how a
wire format stays compatible.

**It does change what a correct decoder looks like.** Written against the nine-field list, a
decoder is correct only for a module that never sets a flag above `0x0100` — and it would not
fail loudly if one did. It would read the brand identifier's two bytes as the start of the EPC
length and return a plausible, wrong chip id. Which is the failure mode this crate has already
had once, with the CRC ([finding 8](#8-the-crc-was-not-ccitt-false-and-the-codec-computed-the-wrong-checksum)):
internally consistent, externally wrong, and fully tested.

The defence is to **decode by walking the flag bits ascending** and to treat an unknown
high bit as a hard error rather than as an unset field — because at that point the parser has
lost its place in the stream and everything after it is a guess.

## Adding a document here

One row per document, with a SHA-256 taken at retrieval, and the code or docs that depend on
it named. If the code depends on a specific claim, quote the claim — a URL that 404s two years
from now is the situation this file exists because of.
