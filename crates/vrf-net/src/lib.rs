//! Unreal Engine replication layer: packets -> bunches -> content blocks -> fields.
//!
//! No field payload is skipped, descriptor or not (the upstream parser skips
//! any field it has no descriptor for). Every property and RPC reaches the
//! caller's sink as `(handle, bit_count, raw_bits)`, which works because the
//! field stream is self-describing: each field carries its handle and length.
//!
//! ```text
//! packet  : sentinel-trimmed byte slice -> bit stream
//! bunch   : header + partial reassembly state machine
//! content : framing loop; header + payload per block
//! field   : self-describing handle/size stream
//! ```
//!
//! The caller injects export-group resolution (which names map to which
//! handles), typed field decoding, and NetGuidCache storage (through
//! [`net_guid::GuidPathSink`]).
//!
//! A malformed bunch is discarded and counted; it does not abort the replay,
//! and no discard goes uncounted.
//!
//! # Features
//!
//! `diagnostics` (default) adds only the per-failure event log in [`stats`]:
//! `DiagnosticEvent`, `SkipReason`, the two snapshot types and the two
//! `NetStats` fields that hold them. The counters are in every build -- a
//! build without them would lose data silently -- and nothing else is
//! optional: packets, bunches, content blocks and fields are one state machine.

#![forbid(unsafe_code)]

pub mod bunch;
pub mod content;
pub mod error;
pub mod field;
pub mod net_guid;
pub mod packet;
pub mod pipeline;
pub mod stats;
pub mod types;

pub use error::NetError;
pub use pipeline::{PLAYER_CONTROLLER_LEAF, ReplicationReader, ReplicationSink};
pub use stats::NetStats;

/// Bit writers shared by this crate's unit tests, appending bits in the order
/// the matching `BitReader` read consumes them (least significant first);
/// `pack` and `build_packet` turn the result into bytes.
#[cfg(test)]
mod test_bits {
    use crate::types::{ChannelCloseReason, MAX_PACKET_SIZE_BITS};

    /// Append `value` as an `IntPacked`: seven value bits per byte, above a
    /// low bit that says whether another byte follows.
    pub fn write_int_packed(bits: &mut Vec<bool>, mut value: u32) {
        loop {
            let mut next_byte = ((value & 0x7F) << 1) as u8;
            value >>= 7;
            if value != 0 {
                next_byte |= 1;
            }
            write_byte(bits, next_byte);
            if value == 0 {
                break;
            }
        }
    }

    /// Append `value` as a `SerializedInt` bounded by `max_value`.
    pub fn write_serialized_int(bits: &mut Vec<bool>, value: u32, max_value: u32) {
        let mut written_value = 0u32;
        let mut mask = 1u32;
        while written_value.saturating_add(mask) < max_value {
            let bit = (value & mask) != 0;
            bits.push(bit);
            if bit {
                written_value |= mask;
            }
            mask <<= 1;
        }
    }

    pub fn write_byte(bits: &mut Vec<bool>, byte: u8) {
        bits.extend((0..8).map(|i| (byte & (1 << i)) != 0));
    }

    /// Pack bits into bytes, leaving the last byte's unused high bits zero.
    pub fn pack(bits: &[bool]) -> Vec<u8> {
        let mut bytes = vec![0u8; bits.len().div_ceil(8)];
        for (i, &bit) in bits.iter().enumerate() {
            if bit {
                bytes[i >> 3] |= 1 << (i & 7);
            }
        }
        bytes
    }

    /// Pack `bits` as one packet: the data, then the sentinel bit.
    pub fn build_packet(bits: &[bool]) -> Vec<u8> {
        let mut with_sentinel = bits.to_vec();
        with_sentinel.push(true);
        pack(&with_sentinel)
    }

    /// The header flags one synthetic bunch varies. The default is a
    /// reliable bunch on channel 0 with every other flag clear.
    pub struct BunchSpec {
        pub ch_index: u32,
        pub b_open: bool,
        pub b_close: bool,
        /// Close reason Dormancy rather than Destroyed; used only with `b_close`.
        pub dormant: bool,
        pub b_reliable: bool,
        pub b_has_package_map_exports: bool,
        pub b_has_must_be_mapped_guids: bool,
        pub b_partial: bool,
        pub b_partial_initial: bool,
        pub b_partial_final: bool,
    }

    impl Default for BunchSpec {
        fn default() -> Self {
            Self {
                ch_index: 0,
                b_open: false,
                b_close: false,
                dormant: false,
                b_reliable: true,
                b_has_package_map_exports: false,
                b_has_must_be_mapped_guids: false,
                b_partial: false,
                b_partial_initial: false,
                b_partial_final: false,
            }
        }
    }

    /// Append one bunch header declaring `payload_bit_count` bits, in
    /// `parse_bunch_header` order. The channel FName, hardcoded index 1, is
    /// written when the bunch is reliable or opens its channel.
    pub fn write_bunch_header(bits: &mut Vec<bool>, spec: &BunchSpec, payload_bit_count: u32) {
        let b_control = spec.b_open || spec.b_close;
        bits.push(b_control);
        if b_control {
            bits.push(spec.b_open);
            bits.push(spec.b_close);
        }
        if spec.b_close {
            write_serialized_int(bits, u32::from(spec.dormant), ChannelCloseReason::MAX);
        }
        bits.push(false); // bIsReplicationPaused
        bits.push(spec.b_reliable);
        write_int_packed(bits, spec.ch_index);
        bits.push(spec.b_has_package_map_exports);
        bits.push(spec.b_has_must_be_mapped_guids);
        bits.push(spec.b_partial);
        bits.push(false); // VALORANT bit
        if spec.b_partial {
            bits.push(spec.b_partial_initial);
            bits.push(spec.b_partial_final);
        }
        if spec.b_reliable || spec.b_open {
            bits.push(true); // channel FName: isHardcoded
            write_int_packed(bits, 1); // FName index
        }
        write_serialized_int(bits, payload_bit_count, MAX_PACKET_SIZE_BITS);
    }

    /// Append one bunch: its header, then `payload`.
    pub fn write_bunch(bits: &mut Vec<bool>, spec: &BunchSpec, payload: &[bool]) {
        write_bunch_header(bits, spec, payload.len() as u32);
        bits.extend_from_slice(payload);
    }

    /// One bunch, one packet.
    pub fn build_bunch_packet(spec: &BunchSpec, payload: &[bool]) -> Vec<u8> {
        let mut bits = Vec::new();
        write_bunch(&mut bits, spec, payload);
        build_packet(&bits)
    }
}
