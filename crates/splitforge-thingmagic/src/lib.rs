//! # splitforge-thingmagic
//!
//! ThingMagic serial reader adapter: frame parsing, connection lifecycle, and reconnect.
//!
//! **Status:** the frame codec, the command set, and the connection lifecycle — this crate
//! now implements [`ReaderProvider`]. What it cannot yet do is turn a tag report into a read:
//! [`TagReportDecoder`] is a trait with no implementation here, because the metadata flag
//! values that say which bit selects which field are in a header this project does not have.
//! See `docs/roadmap.md`, Milestone 3a.
//!
//! ## Boundaries
//!
//! - **May depend on:** splitforge-reader, splitforge-domain
//! - **Must never depend on:** splitforge-engine, splitforge-storage, splitforge-api
//!
//! The same boundary `splitforge-llrp` declares. These rules come from ADR-0001, are
//! tabulated in `docs/architecture.md`, and are enforced by
//! `crates/splitforge-testkit/tests/dependency_rules.rs` rather than left to review
//! (ADR-0012).
//!
//! ## Why this crate exists beside `splitforge-llrp`
//!
//! [ADR-0024](../../../docs/adr/0024-serial-reader-adapter-before-llrp.md). LLRP remains
//! the first *networked* protocol and Milestone 3b still holds all nine of the support
//! criteria; a serial module is the first *physical* adapter, because one can be bought
//! and a networked reader in this project's price range cannot. Those are different
//! claims, and neither this crate's existence nor its passing tests turn one into the
//! other.
//!
//! **No device is supported.** The support matrix in `docs/hardware-support.md` is empty
//! and stays empty: this module has no reader clock and one RF port, so two of the nine
//! criteria are unclosable on it structurally rather than pending.
//!
//! ## What is here, and what it is worth
//!
//! Framing, as pure functions over `&[u8]` — [`frame::decode`], [`frame::encode_command`],
//! and the [`crc`] they rest on. The I/O sits above them and stays out of their way:
//! [`reassembly`] keeps a buffer to ask [`frame::decode`] about and still touches no port,
//! and [`port`] is the only module in the crate that names `serialport` at all. Everything
//! between them takes a [`PortFactory`], so opening, reading, resynchronizing after a
//! mid-frame disconnect, and bounded jittered reconnect are all exercised against ports that
//! fail exactly when a test wants them to.
//!
//! That layering is deliberate and it is most of the value. Binary parsing against untrusted
//! input is the highest-risk code in the project, and it is the part that can be finished, in
//! full, before anybody spends $345 — truncated frames, bad checksums, and a `0xFF` in the
//! middle of a payload are all reachable from a byte-slice literal.
//!
//! **One of those assumptions was wrong, and finding it is what this warning was for.** The
//! CRC was implemented as CRC-16/CCITT-FALSE, which is what user guide § 7.3 names it; the
//! module computes something else, and every test in this crate passed anyway because they all
//! built frames with the same function they checked them with. It is corrected in [`crc`] and
//! anchored to a frame a real module produced.
//!
//! **What no test here can still tell you** is whether the rest of the layout is right. It
//! comes from vendor documentation rather than from a capture, and it is written down as an
//! assumption in [`frame`] so the first person with a real module knows where to look. A parser
//! that is internally consistent and externally wrong passes every test in this crate — which
//! is not a hypothetical any more.

// No panicking on any path reachable during an event: a corrupt frame, a missing
// field, or an out-of-range value must become an error the caller can act on, never a
// timer that stops mid-race. Test code is exempt, where panicking on the unexpected is
// the point. See CONTRIBUTING.md, "Code standards".
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod command;
pub mod crc;
pub mod frame;
pub mod port;
pub mod provider;
pub mod reassembly;

pub use command::{OpCode, antenna_ports, search_flag};
pub use crc::crc16;
pub use frame::{
    COMMAND_HEADER_LEN, CRC_LEN, Decoded, EncodeError, FrameError, MAX_COMMAND_DATA_LEN,
    MAX_FRAME_LEN, MAX_RESPONSE_DATA_LEN, RESPONSE_HEADER_LEN, Response, SOH, decode,
    encode_command, resynchronize,
};
pub use port::{Port, PortFactory, SerialSettings, serial};
pub use provider::{Backoff, SessionAnchor, TagReportDecoder, ThingMagicReader};
pub use reassembly::{Reassembler, Stats};
