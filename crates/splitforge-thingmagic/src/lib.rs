//! # splitforge-thingmagic
//!
//! ThingMagic serial reader adapter: frame parsing, connection lifecycle, and reconnect.
//!
//! **Status:** the frame codec only. There is no connection lifecycle here yet and no
//! [`ReaderProvider`] implementation — see `docs/roadmap.md`, Milestone 3a, for what lands
//! here and in what order.
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
//! and the [`crc`] they rest on. Nothing in this crate opens a port, sleeps, or knows what
//! time it is.
//!
//! That is deliberate and it is most of the value. Binary parsing against untrusted input
//! is the highest-risk code in the project, and it is the part that can be finished, in
//! full, before anybody spends $345 — truncated frames, bad checksums, and a `0xFF` in the
//! middle of a payload are all reachable from a byte-slice literal.
//!
//! **What no test here can tell you** is whether these are the *right* bytes. The layout
//! and the CRC coverage come from vendor documentation, not from a capture, and both are
//! written down as assumptions in [`frame`] so that the first person with a real module
//! knows which two things to check. A parser that is internally consistent and externally
//! wrong passes every test in this crate.

// No panicking on any path reachable during an event: a corrupt frame, a missing
// field, or an out-of-range value must become an error the caller can act on, never a
// timer that stops mid-race. Test code is exempt, where panicking on the unexpected is
// the point. See CONTRIBUTING.md, "Code standards".
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod crc;
pub mod frame;

pub use crc::crc16;
pub use frame::{
    COMMAND_HEADER_LEN, CRC_LEN, Decoded, EncodeError, FrameError, MAX_COMMAND_DATA_LEN,
    MAX_FRAME_LEN, MAX_RESPONSE_DATA_LEN, RESPONSE_HEADER_LEN, Response, SOH, decode,
    encode_command, resynchronize,
};
