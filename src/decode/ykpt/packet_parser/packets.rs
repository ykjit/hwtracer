//! Intel PT packets and their constituents.

use deku::prelude::*;
use std::convert::TryFrom;

/// The `IPBytes` field common to all IP packets.
///
/// This tells us what kind of compression was used for a `TargetIP`.
#[derive(Clone, Copy, Debug, DekuRead)]
pub(in crate::decode::ykpt) struct IPBytes {
    #[deku(bits = "3")]
    val: u8,
}

impl IPBytes {
    #[cfg(test)]
    pub(in crate::decode::ykpt) fn new(val: u8) -> Self {
        debug_assert!(val >> 3 == 0);
        Self { val }
    }

    /// Returns `true` if we need the previous TIP value to make sense of the new one.
    pub(super) fn needs_prev_tip(&self) -> bool {
        match self.val {
            0b001 | 0b010 | 0b100 => true,
            _ => false,
        }
    }
}

/// The `TargetIP` fields in packets which update the TIP.
///
/// This is a variable-width field depending upon the value if `IPBytes` in the containing packet.
#[derive(Debug, DekuRead)]
#[deku(id = "ip_bytes_val", ctx = "ip_bytes_val: u8")]
pub(in crate::decode::ykpt) enum TargetIP {
    #[deku(id = "0b000")]
    OutOfContext,
    #[deku(id = "0b001")]
    Ip16(u16),
    #[deku(id = "0b010")]
    Ip32(u32),
    #[deku(id_pat = "0b011 | 0b100")]
    Ip48(#[deku(bits = "48")] u64),
    #[deku(id = "0b110")]
    Ip64(u64),
}

impl TargetIP {
    #[cfg(test)]
    pub(in crate::decode::ykpt) fn from_bits(bits: u8, val: u64) -> Self {
        match bits {
            0 => Self::OutOfContext,
            16 => Self::Ip16(u16::try_from(val).unwrap()),
            32 => Self::Ip32(u32::try_from(val).unwrap()),
            48 => Self::Ip48(val),
            64 => Self::Ip64(val),
            _ => panic!(),
        }
    }

    /// Decompress a `TargetIP` and `IPBytes` pair into an instruction pointer address.
    ///
    /// Returns `None` if the target IP was "out of context".
    pub(in crate::decode::ykpt) fn decompress(
        &self,
        ip_bytes: IPBytes,
        prev_tip: Option<usize>,
    ) -> Option<usize> {
        let res = match ip_bytes.val {
            0b000 => {
                debug_assert!(matches!(self, Self::OutOfContext));
                return None;
            }
            0b001 => {
                // The result is bytes 63..=16 from `prev_tip` and bytes 15..=0 from `ip`.
                if let Self::Ip16(v) = self {
                    prev_tip.unwrap() & 0xffffffffffff0000 | usize::try_from(*v).unwrap()
                } else {
                    unreachable!();
                }
            }
            0b010 => {
                // The result is bytes 63..=32 from `prev_tip` and bytes 31..=0 from `ip`.
                if let Self::Ip32(v) = self {
                    prev_tip.unwrap() & 0xffffffff00000000 | usize::try_from(*v).unwrap()
                } else {
                    unreachable!();
                }
            }
            0b011 => {
                // The result is bits 0..=47 from the IP, with the remaining high-order bits
                // extended with the value of bit 47.
                if let Self::Ip48(v) = self {
                    debug_assert!(v >> 48 == 0);
                    // Extract the value of bit 47.
                    let b47 = (v & (1 << 47)) >> 47;
                    // Copy the value of bit 47 across all 64 bits.
                    let all = u64::wrapping_sub(!b47 & 0x1, 1);
                    // Restore bits 47..=0 to arrive at the result.
                    usize::try_from(all & 0xffff000000000000 | v).unwrap()
                } else {
                    unreachable!();
                }
            }
            0b100 => todo!(),
            0b101 => unreachable!(), // reserved by Intel.
            0b110 => {
                // Uncompressed IP.
                if let Self::Ip64(v) = self {
                    usize::try_from(*v).unwrap()
                } else {
                    unreachable!();
                }
            }
            0b111 => unreachable!(), // reserved by Intel.
            _ => todo!("IPBytes: {:03b}", ip_bytes.val),
        };
        Some(usize::try_from(res).unwrap())
    }
}

/// Packet Stream Boundary (PSB) packet.
#[derive(Debug, PartialEq, DekuRead)]
#[deku(magic = b"\x02\x82\x02\x82\x02\x82\x02\x82\x02\x82\x02\x82\x02\x82\x02\x82")]
pub(in crate::decode::ykpt) struct PSBPacket {}

/// Core Bus Ratio (CBR) packet.
#[deku_derive(DekuRead)]
#[derive(Debug)]
#[deku(magic = b"\x02\x03")]
pub(in crate::decode::ykpt) struct CBRPacket {
    #[deku(temp)]
    unused: u16,
}

/// End of PSB+ sequence (PSBEND) packet.
#[derive(Debug, DekuRead)]
#[deku(magic = b"\x02\x23")]
pub(in crate::decode::ykpt) struct PSBENDPacket {}

/// Padding (PAD) packet.
#[derive(Debug, DekuRead)]
#[deku(magic = b"\x00")]
pub(in crate::decode::ykpt) struct PADPacket {}

/// Mode (MODE.*) packet.
#[deku_derive(DekuRead)]
#[derive(Debug)]
#[deku(magic = b"\x99")]
pub(in crate::decode::ykpt) struct MODEPacket {
    // If we ever need to actually interpret the data inside the `MODE.*` packets, it would be best
    // to split this struct out into multiple structs and have deku parse the different mode kinds
    // independently.
    #[deku(temp)]
    unused: u8,
}

/// Packet Generation Enable (TIP.PGE) packet.
#[deku_derive(DekuRead)]
#[derive(Debug)]
pub(in crate::decode::ykpt) struct TIPPGEPacket {
    ip_bytes: IPBytes,
    #[deku(bits = "5", assert = "*magic & 0x1f == 0x11", temp)]
    magic: u8,
    #[deku(ctx = "ip_bytes.val")]
    target_ip: TargetIP,
}

impl TIPPGEPacket {
    fn target_ip(&self, prev_tip: Option<usize>) -> Option<usize> {
        self.target_ip.decompress(self.ip_bytes, prev_tip)
    }

    pub(super) fn needs_prev_tip(&self) -> bool {
        self.ip_bytes.needs_prev_tip()
    }
}

/// Short Taken/Not-Taken (TNT) packet.
#[deku_derive(DekuRead)]
#[derive(Debug)]
pub(in crate::decode::ykpt) struct ShortTNTPacket {
    /// Bits encoding the branch decisions **and** a stop bit.
    ///
    /// The deku assertion here is subtle: we know that the `branches` field must contain a stop
    /// bit terminating the field, but if the stop bit appears in place of the first branch, then
    /// this is not a short TNT packet at all; it's a long TNT packet.
    ///
    /// FIXME: marked `temp` until we actually use the field.
    #[deku(bits = "7", assert = "*branches != 0x1", temp)]
    branches: u8,
    #[deku(bits = "1", assert = "*magic == false", temp)]
    magic: bool,
}

/// Long Taken/Not-Taken (TNT) packet.
#[deku_derive(DekuRead)]
#[derive(Debug)]
#[deku(magic = b"\x02\xa3")]
pub(in crate::decode::ykpt) struct LongTNTPacket {
    /// Bits encoding the branch decisions **and** a stop bit.
    ///
    /// FIXME: marked `temp` until we actually use the field.
    #[deku(bits = "48", temp)]
    branches: u64,
}

/// Target IP (TIP) packet.
#[deku_derive(DekuRead)]
#[derive(Debug)]
pub(in crate::decode::ykpt) struct TIPPacket {
    ip_bytes: IPBytes,
    #[deku(bits = "5", assert = "*magic & 0x1f == 0x0d", temp)]
    magic: u8,
    #[deku(ctx = "ip_bytes.val")]
    target_ip: TargetIP,
}

impl TIPPacket {
    fn target_ip(&self, prev_tip: Option<usize>) -> Option<usize> {
        self.target_ip.decompress(self.ip_bytes, prev_tip)
    }

    pub(super) fn needs_prev_tip(&self) -> bool {
        self.ip_bytes.needs_prev_tip()
    }
}

/// Packet Generation Disable (TIP.PGD) packet.
#[deku_derive(DekuRead)]
#[derive(Debug)]
pub(in crate::decode::ykpt) struct TIPPGDPacket {
    ip_bytes: IPBytes,
    #[deku(bits = "5", assert = "*magic & 0x1f == 0x1", temp)]
    magic: u8,
    #[deku(ctx = "ip_bytes.val")]
    target_ip: TargetIP,
}

impl TIPPGDPacket {
    fn target_ip(&self, prev_tip: Option<usize>) -> Option<usize> {
        self.target_ip.decompress(self.ip_bytes, prev_tip)
    }

    pub(super) fn needs_prev_tip(&self) -> bool {
        self.ip_bytes.needs_prev_tip()
    }
}

/// Flow Update (FUP) packet.
#[deku_derive(DekuRead)]
#[derive(Debug)]
pub(in crate::decode::ykpt) struct FUPPacket {
    ip_bytes: IPBytes,
    #[deku(bits = "5", assert = "*magic & 0x1f == 0b11101", temp)]
    magic: u8,
    #[deku(ctx = "ip_bytes.val")]
    target_ip: TargetIP,
}

impl FUPPacket {
    fn target_ip(&self, prev_tip: Option<usize>) -> Option<usize> {
        self.target_ip.decompress(self.ip_bytes, prev_tip)
    }

    pub(super) fn needs_prev_tip(&self) -> bool {
        self.ip_bytes.needs_prev_tip()
    }
}

/// Cycle count (CYC) packet.
#[deku_derive(DekuRead)]
#[derive(Debug)]
pub(in crate::decode::ykpt) struct CYCPacket {
    #[deku(bits = "5", temp)]
    unused: u8,
    #[deku(bits = "1", temp)]
    exp: bool,
    #[deku(bits = "2", assert = "*magic & 0x3 == 0b11", temp)]
    magic: u8,
    /// A CYC packet is variable length and has 0 or more "extended" bytes.
    #[deku(
        bits = 8,
        cond = "*exp == true",
        until = "|e: &u8| e & 0x01 != 0x01",
        temp
    )]
    extended: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum PacketKind {
    PSB,
    CBR,
    PSBEND,
    PAD,
    MODE,
    TIPPGE,
    TIPPGD,
    ShortTNT,
    LongTNT,
    TIP,
    FUP,
    CYC,
}

/// The top-level representation of an Intel Processor Trace packet.
///
/// Variants with an `Option<usize>` may cache the previous TIP value (at the time the packet was
/// created). This may be needed to get the updated TIP value from the packet.
#[derive(Debug)]
pub(in crate::decode::ykpt) enum Packet {
    PSB(PSBPacket),
    CBR(CBRPacket),
    PSBEND(PSBENDPacket),
    PAD(PADPacket),
    MODE(MODEPacket),
    TIPPGE(TIPPGEPacket, Option<usize>),
    TIPPGD(TIPPGDPacket, Option<usize>),
    ShortTNT(ShortTNTPacket),
    LongTNT(LongTNTPacket),
    TIP(TIPPacket, Option<usize>),
    FUP(FUPPacket, Option<usize>),
    CYC(CYCPacket),
}

impl Packet {
    /// If the packet contains a TIP update, return the IP value.
    pub(in crate::decode::ykpt) fn target_ip(&self) -> Option<usize> {
        match self {
            Self::TIPPGE(p, prev_tip) => p.target_ip(*prev_tip),
            Self::TIPPGD(p, prev_tip) => p.target_ip(*prev_tip),
            Self::TIP(p, prev_tip) => p.target_ip(*prev_tip),
            Self::FUP(p, prev_tip) => p.target_ip(*prev_tip),
            _ => None,
        }
    }

    pub(super) fn kind(&self) -> PacketKind {
        match self {
            Self::PSB(_) => PacketKind::PSB,
            Self::CBR(_) => PacketKind::CBR,
            Self::PSBEND(_) => PacketKind::PSBEND,
            Self::PAD(_) => PacketKind::PAD,
            Self::MODE(_) => PacketKind::MODE,
            Self::TIPPGE(..) => PacketKind::TIPPGE,
            Self::TIPPGD(..) => PacketKind::TIPPGD,
            Self::ShortTNT(_) => PacketKind::ShortTNT,
            Self::LongTNT(_) => PacketKind::LongTNT,
            Self::TIP(..) => PacketKind::TIP,
            Self::FUP(..) => PacketKind::FUP,
            Self::CYC(_) => PacketKind::CYC,
        }
    }
}
