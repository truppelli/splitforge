//! The ThingMagic serial command set: opcodes and the flags that shape a read.
//!
//! # Where this comes from
//!
//! Not from the user guide. § 7 documents the *framing* and stops — no opcode table, no
//! command list, no tag-report layout in 61 pages — because the vendor's stated position is
//! that *"ThingMagic does not support bypassing the MercuryAPI to send commands to the
//! ThingMagic module directly."*
//!
//! So these values are transcribed from MercuryAPI itself, which is MIT-licensed and therefore
//! compatible with this repository's GPL-3.0-or-later:
//!
//! - [`OpCode`] — `serial_reader_imp.h`, `enum TMR_SR_OpCode`
//! - [`search_flag`] — `serial_reader_imp.h`, `enum TMR_SR_SearchFlag`
//!
//! > Copyright (c) 2009 ThingMagic, Inc. Licensed under the MIT License.
//!
//! `docs/readers/vendor-documents.md` records both files with their SHA-256 hashes, and the
//! whole table is reproduced there in prose. What is here is the half a compiler can check.
//!
//! # What is deliberately not here
//!
//! **The tag-report metadata flag values.** The report's field *order* is established — read
//! count, RSSI, antenna, frequency, timestamp, phase, protocol, data, GPIO, then the EPC — and
//! it is written down in `vendor-documents.md`. Which *bit* selects which field is **not**:
//! `TMR_TRD_METADATA_FLAG_*` lives in a header the archived mirror does not carry a current
//! copy of.
//!
//! Guessing them would produce a parser that is internally consistent and externally wrong,
//! which is precisely the failure this crate has already shipped once — see [`crate::crc`].
//! They are absent rather than approximated, and the read path cannot be finished without
//! them.

/// A command opcode.
///
/// Transcribed complete rather than filtered to the ones the read path needs. An exhaustive
/// enum lets [`OpCode::from_byte`] distinguish *"this module sent an opcode that does not
/// exist"* from *"this module sent an opcode nobody bothered to list"* — different faults,
/// and only one of them is the module's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum OpCode {
    /// Write to flash. Bootloader-level; nothing on the read path uses it.
    WriteFlash = 0x01,
    /// Read from flash.
    ReadFlash = 0x02,
    /// Firmware version. The conventional first command after opening a port.
    Version = 0x03,
    /// Leave the bootloader and start the application firmware.
    BootFirmware = 0x04,
    /// Change the serial baud rate. Default is 115200 (§ 5.1.4.2).
    SetBaudRate = 0x06,
    /// Erase a flash region.
    EraseFlash = 0x07,
    /// Verify a firmware image's CRC.
    VerifyImageCrc = 0x08,
    /// Enter the bootloader.
    BootBootloader = 0x09,
    /// Modify flash in place.
    ModifyFlash = 0x0A,
    /// The DSP's silicon identifier.
    GetDspSiliconId = 0x0B,
    /// Which program — bootloader or application — is currently running.
    GetCurrentProgram = 0x0C,
    /// Write a whole flash sector.
    WriteFlashSector = 0x0D,
    /// The flash sector size.
    GetSectorSize = 0x0E,
    /// Modify a flash sector in place.
    ModifyFlashSector = 0x0F,
    /// Hardware version.
    HwVersion = 0x10,
    /// Read one tag ID.
    ReadTagIdSingle = 0x21,
    /// The read path. Also the opcode of [`crate::crc::CAPTURED_FRAME`], which is what anchors
    /// this table to something a real module actually emitted.
    ReadTagIdMultiple = 0x22,
    /// Write a tag's EPC.
    WriteTagId = 0x23,
    /// Write to a tag's memory bank.
    WriteTagData = 0x24,
    /// Lock a tag's memory.
    LockTag = 0x25,
    /// Permanently disable a tag.
    KillTag = 0x26,
    /// Read a tag's memory bank.
    ReadTagData = 0x28,
    /// Fetch buffered tag reports. **The polling path**, rejected by ADR-0025: this buffer
    /// deduplicates in hardware, so what comes back is not the burst a `SelectionRule` needs.
    GetTagIdBuffer = 0x29,
    /// Empty the tag buffer.
    ClearTagIdBuffer = 0x2A,
    /// A protocol-specific write.
    WriteTagSpecific = 0x2D,
    /// A protocol-specific block erase.
    EraseBlockTagSpecific = 0x2E,
    /// Run a tag operation across several protocols.
    MultiProtocolTagOp = 0x2F,
    /// The configured antenna ports.
    GetAntennaPort = 0x61,
    /// Transmit power used for reads.
    GetReadTxPower = 0x62,
    /// The active tag protocol.
    GetTagProtocol = 0x63,
    /// Transmit power used for writes.
    GetWriteTxPower = 0x64,
    /// The frequency hop table.
    GetFreqHopTable = 0x65,
    /// The state of the GPI pins.
    GetUserGpioInputs = 0x66,
    /// The configured regulatory region.
    GetRegion = 0x67,
    /// The power mode.
    GetPowerMode = 0x68,
    /// The user mode.
    GetUserMode = 0x69,
    /// Optional reader parameters.
    GetReaderOptionalParams = 0x6A,
    /// A protocol parameter.
    GetProtocolParam = 0x6B,
    /// Reader statistics — the payload the status/stats report stream carries.
    GetReaderStats = 0x6C,
    /// The saved user profile.
    GetUserProfile = 0x6D,
    /// Which tag protocols this module supports.
    GetAvailableProtocols = 0x70,
    /// Which regulatory regions this module supports.
    GetAvailableRegions = 0x71,
    /// The module's temperature.
    GetTemperature = 0x72,
    /// Configure the antenna ports.
    SetAntennaPort = 0x91,
    /// Set transmit power for reads. Capped at 24 dBm on this module.
    SetReadTxPower = 0x92,
    /// Select the tag protocol.
    SetTagProtocol = 0x93,
    /// Set transmit power for writes.
    SetWriteTxPower = 0x94,
    /// Set the frequency hop table.
    SetFreqHopTable = 0x95,
    /// Drive the GPO pins.
    SetUserGpioOutputs = 0x96,
    /// Set the regulatory region. Required before transmitting.
    SetRegion = 0x97,
    /// Set the power mode.
    SetPowerMode = 0x98,
    /// Set the user mode.
    SetUserMode = 0x99,
    /// Set optional reader parameters.
    SetReaderOptionalParams = 0x9A,
    /// Set a protocol parameter.
    SetProtocolParam = 0x9B,
    /// Save or restore a user profile. The persistence § 5.1.4.2 is uncertain about.
    SetUserProfile = 0x9D,
    /// Install a protocol license key.
    SetProtocolLicenseKey = 0x9E,
    /// Set a fixed operating frequency.
    SetOperatingFreq = 0xC1,
    /// Transmit an unmodulated carrier. A test mode, not a read mode.
    TxCwSignal = 0xC3,
}

impl OpCode {
    /// The wire byte.
    #[must_use]
    pub const fn to_byte(self) -> u8 {
        self as u8
    }

    /// The opcode a byte names, or `None` if no opcode has that value.
    ///
    /// `None` is a fact worth reporting rather than an error worth failing on: a response
    /// carrying an unrecognized opcode is a frame this crate cannot interpret, which the
    /// caller may want to log and skip rather than treat as a broken stream.
    #[must_use]
    pub const fn from_byte(byte: u8) -> Option<Self> {
        // Written as a match rather than a transmute so that an unlisted value is a `None`
        // and never an `OpCode` variant that does not exist. `unsafe_code = "deny"` would
        // refuse the alternative anyway.
        Some(match byte {
            0x01 => Self::WriteFlash,
            0x02 => Self::ReadFlash,
            0x03 => Self::Version,
            0x04 => Self::BootFirmware,
            0x06 => Self::SetBaudRate,
            0x07 => Self::EraseFlash,
            0x08 => Self::VerifyImageCrc,
            0x09 => Self::BootBootloader,
            0x0A => Self::ModifyFlash,
            0x0B => Self::GetDspSiliconId,
            0x0C => Self::GetCurrentProgram,
            0x0D => Self::WriteFlashSector,
            0x0E => Self::GetSectorSize,
            0x0F => Self::ModifyFlashSector,
            0x10 => Self::HwVersion,
            0x21 => Self::ReadTagIdSingle,
            0x22 => Self::ReadTagIdMultiple,
            0x23 => Self::WriteTagId,
            0x24 => Self::WriteTagData,
            0x25 => Self::LockTag,
            0x26 => Self::KillTag,
            0x28 => Self::ReadTagData,
            0x29 => Self::GetTagIdBuffer,
            0x2A => Self::ClearTagIdBuffer,
            0x2D => Self::WriteTagSpecific,
            0x2E => Self::EraseBlockTagSpecific,
            0x2F => Self::MultiProtocolTagOp,
            0x61 => Self::GetAntennaPort,
            0x62 => Self::GetReadTxPower,
            0x63 => Self::GetTagProtocol,
            0x64 => Self::GetWriteTxPower,
            0x65 => Self::GetFreqHopTable,
            0x66 => Self::GetUserGpioInputs,
            0x67 => Self::GetRegion,
            0x68 => Self::GetPowerMode,
            0x69 => Self::GetUserMode,
            0x6A => Self::GetReaderOptionalParams,
            0x6B => Self::GetProtocolParam,
            0x6C => Self::GetReaderStats,
            0x6D => Self::GetUserProfile,
            0x70 => Self::GetAvailableProtocols,
            0x71 => Self::GetAvailableRegions,
            0x72 => Self::GetTemperature,
            0x91 => Self::SetAntennaPort,
            0x92 => Self::SetReadTxPower,
            0x93 => Self::SetTagProtocol,
            0x94 => Self::SetWriteTxPower,
            0x95 => Self::SetFreqHopTable,
            0x96 => Self::SetUserGpioOutputs,
            0x97 => Self::SetRegion,
            0x98 => Self::SetPowerMode,
            0x99 => Self::SetUserMode,
            0x9A => Self::SetReaderOptionalParams,
            0x9B => Self::SetProtocolParam,
            0x9D => Self::SetUserProfile,
            0x9E => Self::SetProtocolLicenseKey,
            0xC1 => Self::SetOperatingFreq,
            0xC3 => Self::TxCwSignal,
            _ => return None,
        })
    }

    /// Every opcode, in ascending wire order.
    ///
    /// Exists so a test can assert the two directions agree without restating the table a
    /// third time.
    pub const ALL: &'static [Self] = &[
        Self::WriteFlash,
        Self::ReadFlash,
        Self::Version,
        Self::BootFirmware,
        Self::SetBaudRate,
        Self::EraseFlash,
        Self::VerifyImageCrc,
        Self::BootBootloader,
        Self::ModifyFlash,
        Self::GetDspSiliconId,
        Self::GetCurrentProgram,
        Self::WriteFlashSector,
        Self::GetSectorSize,
        Self::ModifyFlashSector,
        Self::HwVersion,
        Self::ReadTagIdSingle,
        Self::ReadTagIdMultiple,
        Self::WriteTagId,
        Self::WriteTagData,
        Self::LockTag,
        Self::KillTag,
        Self::ReadTagData,
        Self::GetTagIdBuffer,
        Self::ClearTagIdBuffer,
        Self::WriteTagSpecific,
        Self::EraseBlockTagSpecific,
        Self::MultiProtocolTagOp,
        Self::GetAntennaPort,
        Self::GetReadTxPower,
        Self::GetTagProtocol,
        Self::GetWriteTxPower,
        Self::GetFreqHopTable,
        Self::GetUserGpioInputs,
        Self::GetRegion,
        Self::GetPowerMode,
        Self::GetUserMode,
        Self::GetReaderOptionalParams,
        Self::GetProtocolParam,
        Self::GetReaderStats,
        Self::GetUserProfile,
        Self::GetAvailableProtocols,
        Self::GetAvailableRegions,
        Self::GetTemperature,
        Self::SetAntennaPort,
        Self::SetReadTxPower,
        Self::SetTagProtocol,
        Self::SetWriteTxPower,
        Self::SetFreqHopTable,
        Self::SetUserGpioOutputs,
        Self::SetRegion,
        Self::SetPowerMode,
        Self::SetUserMode,
        Self::SetReaderOptionalParams,
        Self::SetProtocolParam,
        Self::SetUserProfile,
        Self::SetProtocolLicenseKey,
        Self::SetOperatingFreq,
        Self::TxCwSignal,
    ];
}

/// Flags for [`OpCode::ReadTagIdMultiple`], from `enum TMR_SR_SearchFlag`.
///
/// A `u16` on the wire, big-endian, immediately after the option byte. These are what turn a
/// one-shot inventory into the continuous stream
/// [ADR-0025](../../../docs/adr/0025-m3a-proves-durability-above-the-transport.md) chose.
pub mod search_flag {
    /// Use whichever antenna is configured. Zero, so it is the absence of the other schemes
    /// rather than a bit of its own.
    pub const CONFIGURED_ANTENNA: u16 = 0x0000;
    /// Antenna 1, then antenna 2.
    pub const ANTENNA_1_THEN_2: u16 = 0x0001;
    /// Antenna 2, then antenna 1.
    pub const ANTENNA_2_THEN_1: u16 = 0x0002;
    /// Use the configured antenna list.
    pub const CONFIGURED_LIST: u16 = 0x0003;
    /// The two bits the antenna scheme occupies.
    pub const ANTENNA_MASK: u16 = 0x0003;
    /// An operation is embedded in the read.
    pub const EMBEDDED_COMMAND: u16 = 0x0004;
    /// **Stream tag reports** rather than buffering them for a later fetch.
    ///
    /// The mode this adapter runs in. See ADR-0025 for why polling the tag buffer was
    /// rejected: it deduplicates in hardware, which removes the burst every `SelectionRule`
    /// selects from.
    pub const TAG_STREAMING: u16 = 0x0008;
    /// Support a large tag population. MercuryAPI sets this unconditionally.
    pub const LARGE_TAG_POPULATION_SUPPORT: u16 = 0x0010;
    /// Stream status reports alongside tag reports.
    ///
    /// A **candidate keepalive**, and the reason ADR-0025's "no liveness signal" was softened.
    /// Whether these arrive periodically, and whether they arrive with no tags in the field, is
    /// [Q14](../../../docs/open-questions.md) and is not established. Mutually exclusive with
    /// [`STATS_REPORT_STREAMING`].
    pub const STATUS_REPORT_STREAMING: u16 = 0x0020;
    /// Return after N tags. MercuryAPI refuses to combine this with [`TAG_STREAMING`].
    pub const RETURN_ON_N_TAGS: u16 = 0x0040;
    /// Fast search.
    pub const READ_MULTIPLE_FAST_SEARCH: u16 = 0x0080;
    /// Stream reader statistics. Mutually exclusive with [`STATUS_REPORT_STREAMING`].
    pub const STATS_REPORT_STREAMING: u16 = 0x0100;
    /// Trigger the read from a GPI pin.
    pub const GPI_TRIGGER_READ: u16 = 0x0200;
    /// Apply duty-cycle control, which adds an off-time field to the command.
    pub const DUTY_CYCLE_CONTROL: u16 = 0x0400;
}

/// Splits a tag report's antenna byte into its transmit and receive ports.
///
/// **The byte is not an antenna number**, which is the trap § 8.8.3 sets by describing the
/// field as *"the logical antenna port of the tag read"* — true of the value MercuryAPI hands
/// its caller, false of the byte on the wire. It is two nibbles, and a zero nibble means 16:
///
/// ```c
/// tx = (read->antenna >> 4) & 0xF;
/// rx = (read->antenna >> 0) & 0xF;
/// // Due to limited space, Antenna 16 wraps around to 0
/// if (0 == tx) { tx = 16; }
/// if (0 == rx) { rx = 16; }
/// ```
///
/// Turning `(tx, rx)` into the logical port an operator configured needs the reader's
/// transmit/receive map, which this crate does not have yet. Returning the pair — rather than
/// a number that looks like an antenna and is not one — is what keeps that conversion a
/// decision somebody makes rather than one that happens by accident.
#[must_use]
pub const fn antenna_ports(byte: u8) -> (u8, u8) {
    let tx = (byte >> 4) & 0x0F;
    let rx = byte & 0x0F;
    // A nibble of zero is port 16: the field ran out of room, not out of antennas.
    (if tx == 0 { 16 } else { tx }, if rx == 0 { 16 } else { rx })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crc::CAPTURED_FRAME;

    /// The one assertion here anchored outside this file.
    ///
    /// Every other test in this module compares the table against itself, which is exactly how
    /// the CRC stayed wrong through thirty-four passing tests. `CAPTURED_FRAME` is a real
    /// response from a real module, and byte 2 of a response frame is its opcode — so if the
    /// opcode table were transcribed from the wrong enum, this fails.
    #[test]
    fn the_captured_frame_carries_the_opcode_this_table_names() {
        assert_eq!(
            OpCode::from_byte(CAPTURED_FRAME[2]),
            Some(OpCode::ReadTagIdMultiple),
            "the captured frame is a read-tag-ID-multiple response",
        );
        assert_eq!(OpCode::ReadTagIdMultiple.to_byte(), 0x22);
    }

    #[test]
    fn every_opcode_round_trips_through_its_byte() {
        for &op in OpCode::ALL {
            assert_eq!(
                OpCode::from_byte(op.to_byte()),
                Some(op),
                "{op:?} did not survive a round trip",
            );
        }
    }

    #[test]
    fn the_table_is_listed_once_and_agrees_with_itself() {
        // `ALL` is hand-written beside a hand-written `match`, so the two can disagree. This
        // catches an opcode added to one and forgotten in the other, in either direction.
        let listed = OpCode::ALL.len();
        let reachable = (0..=u8::MAX)
            .filter(|&b| OpCode::from_byte(b).is_some())
            .count();
        assert_eq!(
            listed, reachable,
            "`ALL` and `from_byte` disagree on how many opcodes exist"
        );

        let mut sorted = OpCode::ALL.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), listed, "`ALL` contains a duplicate");
    }

    #[test]
    fn an_unassigned_byte_is_not_an_opcode() {
        // Gaps in the vendor's table are real: 0x05, 0x27, and 0x2B are not opcodes. Reporting
        // them as `None` is what lets a caller tell a corrupt frame from an unsupported one.
        for byte in [0x00, 0x05, 0x11, 0x27, 0x2B, 0x60, 0x73, 0x90, 0x9C, 0xFF] {
            assert_eq!(
                OpCode::from_byte(byte),
                None,
                "0x{byte:02X} is not an opcode"
            );
        }
    }

    #[test]
    fn the_antenna_byte_is_two_nibbles_and_zero_means_sixteen() {
        // The trap in one test. A naive `byte as u8` would read 0x11 as antenna 17.
        assert_eq!(antenna_ports(0x11), (1, 1));
        assert_eq!(antenna_ports(0x12), (1, 2));
        assert_eq!(antenna_ports(0x21), (2, 1));
        // Both nibbles wrap independently.
        assert_eq!(antenna_ports(0x00), (16, 16));
        assert_eq!(antenna_ports(0x10), (1, 16));
        assert_eq!(antenna_ports(0x01), (16, 1));
        // The top of the range is representable without wrapping.
        assert_eq!(antenna_ports(0xFF), (15, 15));
    }

    #[test]
    fn the_streaming_flag_is_what_the_adapter_will_set() {
        // ADR-0025 chose streaming. This pins the bit so a later edit that "tidies" the flag
        // module has to change a test that says why the value matters.
        assert_eq!(search_flag::TAG_STREAMING, 0x0008);
        // The two report-streaming flags are mutually exclusive in MercuryAPI, and distinct.
        assert_ne!(
            search_flag::STATUS_REPORT_STREAMING,
            search_flag::STATS_REPORT_STREAMING
        );
        // The antenna scheme lives entirely inside the mask, so setting a scheme cannot
        // disturb the streaming bit.
        assert_eq!(search_flag::ANTENNA_MASK & search_flag::TAG_STREAMING, 0);
        assert_eq!(
            search_flag::CONFIGURED_LIST & !search_flag::ANTENNA_MASK,
            0,
            "an antenna scheme must not set bits outside the mask",
        );
    }
}
