//! Raw packet reading: sentinel-based bit sizing and bunch header extraction.
//!
//! # Packet bit size
//!
//! Unreal pads packets to byte boundaries but marks the true end with a
//! sentinel: one `1` bit followed by zero-padding to the byte boundary. The
//! reader finds this sentinel by scanning the last byte from MSB downward.
//! See `compute_bit_size` below for the walk and the proof that it cannot
//! underflow.
//!
//! If the last byte is zero the packet is malformed (no sentinel exists).

use vrf_bitio::BitReader;

use crate::bunch::RawBunchHeader;
use crate::error::{PartialSequenceKind, Result};
use crate::types::{ChannelCloseReason, MAX_ACTIVE_CHANNELS, MAX_PACKET_SIZE_BITS};

use std::collections::HashMap;

/// Result of reading one packet.
///
/// [`crate::ReplicationReader`] reads `is_malformed` and nothing else. The
/// other three fields exist for direct callers of
/// [`RawPacketReader::read_packet`], which is published API: an extractor that
/// stopped at this reader, bypassing the pipeline, is recorded in
/// docs/TRANSPORT_PRESERVATION.md. No code in this workspace reads them.
#[derive(Debug, Clone)]
pub struct PacketReadResult {
    /// Number of bunches successfully parsed from this packet.
    pub bunch_count: u32,
    /// Whether the packet was malformed (last byte zero or payload overrun).
    pub is_malformed: bool,
    /// Partial-bunch sequence errors found by this reader's own partial
    /// tracker (`track_partial_bunch`).
    ///
    /// Not the count any output reports. The pipeline's reassembly
    /// accumulator is the one partial authority and its count is
    /// [`crate::NetStats::partial_errors`]; the two trackers can disagree --
    /// this one keeps an assembly the accumulator refused -- so the pipeline
    /// neither sums this (doing so counted every error twice) nor lets the
    /// tracker's header flags reach reassembly.
    pub partial_error_count: u32,
    /// Bunches refused because per-channel state could not be admitted or advanced.
    pub channel_limit_count: u32,
}

/// Per-channel state for partial bunch tracking within the packet reader.
#[derive(Debug, Clone)]
struct PartialState {
    ch_sequence: i32,
    reliable: bool,
    is_complete: bool,
}

/// Stateful packet reader that tracks partial bunches and reliable sequences.
///
/// One instance lives for the duration of the replay stream. It accumulates
/// per-channel partial-bunch state and a per-channel reliable sequence counter
/// -- Unreal's `ReliableSequence` is per channel, so a single global counter
/// diverges when two channels interleave reliable bunches.
///
/// The partial-bunch tracking is advisory. Its outputs -- `has_partial_error`
/// and `is_partial_completed` on the headers handed to the `read_packet`
/// callback, and [`PacketReadResult::partial_error_count`] -- are read by
/// nothing in this workspace: the pipeline strips the flags before its
/// reassembly accumulator, the one partial authority, sees the header (see
/// `process_bunch`). The reliable sequence, by contrast, feeds that
/// accumulator through `ch_sequence`.
///
/// The tracker was considered for deletion on 2026-09-28 and kept: those
/// outputs are published API, so removing it would either delete
/// `partial_error_count` or leave it a permanent 0 -- a counter that cannot
/// move -- for any direct caller of `read_packet`.
pub struct RawPacketReader {
    partial_bunches: HashMap<u32, PartialState>,
    in_reliable_sequence: HashMap<u32, i32>,
    max_channels: usize,
}

impl RawPacketReader {
    /// Create a fresh reader with no channel state.
    #[must_use]
    pub fn new() -> Self {
        Self::with_max_channels(MAX_ACTIVE_CHANNELS)
    }

    pub(crate) fn with_max_channels(max_channels: usize) -> Self {
        Self {
            partial_bunches: HashMap::new(),
            in_reliable_sequence: HashMap::new(),
            max_channels,
        }
    }

    /// Parse all bunches from a single packet's byte slice.
    ///
    /// For each successfully parsed bunch, `callback` is invoked with the
    /// parsed header and a sub-reader over the bunch's payload bits.
    /// The callback receives ownership of the payload reader so it can
    /// forward it to content-block framing.
    pub fn read_packet<F>(
        &mut self,
        packet_data: &[u8],
        packet_id: i32,
        mut callback: F,
    ) -> PacketReadResult
    where
        F: FnMut(&mut RawBunchHeader, BitReader<'_>),
    {
        // These three early-outs share the same all-zero counters and differ
        // only in `is_malformed`: nothing has been parsed yet in any of them.
        let empty_result = |is_malformed: bool| PacketReadResult {
            bunch_count: 0,
            is_malformed,
            partial_error_count: 0,
            channel_limit_count: 0,
        };

        if packet_data.is_empty() {
            return empty_result(false);
        }

        let last_byte = packet_data[packet_data.len() - 1];
        if last_byte == 0 {
            return empty_result(true);
        }

        let bit_size = compute_bit_size(packet_data, last_byte);
        let Ok(mut reader) = BitReader::with_bit_len(packet_data, bit_size as u64) else {
            return empty_result(true);
        };

        let mut bunch_count = 0u32;
        let mut partial_error_count = 0u32;
        let mut channel_limit_count = 0u32;

        while !reader.at_end() {
            let header = self.parse_bunch_header(&mut reader, packet_id);
            let header = match header {
                Ok(h) => h,
                Err(_) => {
                    return PacketReadResult {
                        bunch_count,
                        is_malformed: true,
                        partial_error_count,
                        channel_limit_count,
                    };
                }
            };

            let mut header = header;
            if header.has_channel_limit_error {
                channel_limit_count += 1;
            }

            if header.payload_bit_count as u64 > reader.bits_remaining() {
                return PacketReadResult {
                    bunch_count,
                    is_malformed: true,
                    partial_error_count,
                    channel_limit_count,
                };
            }

            if !header.has_channel_limit_error {
                self.track_partial_bunch(&mut header, &mut partial_error_count);
            }

            let payload = reader
                .sub_reader(header.payload_bit_count as u64)
                .expect("bounds already checked");

            callback(&mut header, payload);
            bunch_count += 1;
            if header.b_close && !header.b_dormant {
                self.retire_channel(header.ch_index);
            }
        }

        PacketReadResult {
            bunch_count,
            is_malformed: false,
            partial_error_count,
            channel_limit_count,
        }
    }

    /// Parse a single bunch header from the bit stream.
    ///
    /// ```text
    /// Bit layout (VALORANT replay):
    /// +--------------------------------------------------------------+
    /// | bControl           : 1 bit                                   |
    /// | [if bControl]                                                |
    /// |   bOpen            : 1 bit                                   |
    /// |   bClose           : 1 bit                                   |
    /// |   [if bClose]                                                |
    /// |     CloseReason    : SerializedInt(15)                       |
    /// | bIsReplicationPaused : 1 bit                                 |
    /// | bReliable          : 1 bit                                   |
    /// | ChIndex            : IntPacked                               |
    /// | bHasPackageMapExports : 1 bit                                |
    /// | bHasMustBeMappedGUIDs : 1 bit                                |
    /// | bPartial           : 1 bit                                   |
    /// | <VALORANT>         : 1 bit (meaning unknown; discarded)      |
    /// | [if bPartial]                                                |
    /// |   bPartialInitial  : 1 bit                                   |
    /// |   bPartialFinal    : 1 bit                                   |
    /// | [if bReliable || bOpen]                                      |
    /// |   ChName           : FName (1 bit isHardcoded + IntPacked)   |
    /// | PayloadBitCount    : SerializedInt(16384)                    |
    /// +--------------------------------------------------------------+
    /// ```
    fn parse_bunch_header(
        &mut self,
        reader: &mut BitReader<'_>,
        packet_id: i32,
    ) -> Result<RawBunchHeader> {
        let mut header = RawBunchHeader {
            packet_id,
            ..Default::default()
        };

        let b_control = reader.read_bit()?;
        if b_control {
            header.b_open = reader.read_bit()?;
            header.b_close = reader.read_bit()?;
        }

        if header.b_close {
            let raw = reader.read_serialized_int(ChannelCloseReason::MAX)?;
            header.close_reason = ChannelCloseReason::from_raw(raw);
            header.b_dormant = header.close_reason == ChannelCloseReason::Dormancy;
        }

        header.b_is_replication_paused = reader.read_bit()?;
        header.b_reliable = reader.read_bit()?;
        header.ch_index = reader.read_int_packed()?;
        header.b_has_package_map_exports = reader.read_bit()?;
        header.b_has_must_be_mapped_guids = reader.read_bit()?;
        header.b_partial = reader.read_bit()?;

        if header.b_reliable {
            // No wrap rule is established for this replay format. Refuse the
            // bunch at the representational boundary instead of panicking in
            // debug builds or silently inventing a wrapped sequence in release.
            header.ch_sequence = match self.in_reliable_sequence.get(&header.ch_index).copied() {
                Some(previous) => match previous.checked_add(1) {
                    Some(next) => next,
                    None => {
                        header.has_channel_limit_error = true;
                        previous
                    }
                },
                None => 1,
            };
        } else if header.b_partial {
            header.ch_sequence = packet_id;
        }

        // An extra bit is unconditionally present after bPartial in observed
        // VALORANT packets. Its meaning is not established. Keeping it
        // unnamed is deliberate; only its wire position is corpus-verified.
        let _valorant_bit = reader.read_bit()?;

        if header.b_partial {
            header.b_partial_initial = reader.read_bit()?;
            header.b_partial_final = reader.read_bit()?;
        }

        // Channel name (FName): present when reliable or opening.
        if header.b_reliable || header.b_open {
            // FName: 1 bit isHardcoded + IntPacked index.
            // We consume it but don't need the value for replication.
            let _is_hardcoded = reader.read_bit()?;
            let _name_index = reader.read_int_packed()?;
        }

        header.payload_bit_count = reader.read_serialized_int(MAX_PACKET_SIZE_BITS)? as i32;
        header.payload_bit_offset = reader.position() as i64;

        if header.b_reliable && !header.has_channel_limit_error {
            if !self.in_reliable_sequence.contains_key(&header.ch_index)
                && self.in_reliable_sequence.len() >= self.max_channels
            {
                header.has_channel_limit_error = true;
            } else {
                self.in_reliable_sequence
                    .insert(header.ch_index, header.ch_sequence);
            }
        }

        Ok(header)
    }

    /// Track partial bunch state across fragments.
    ///
    /// For direct callers of `read_packet` only; see [`RawPacketReader`] for
    /// why the pipeline ignores it and why it is kept.
    fn track_partial_bunch(&mut self, header: &mut RawBunchHeader, partial_error_count: &mut u32) {
        if !header.b_partial {
            return;
        }

        if header.b_partial_initial {
            let overlapping = self
                .partial_bunches
                .get(&header.ch_index)
                .is_some_and(|existing| !existing.is_complete);

            // An initial that is also final is a whole bunch: nothing is left in
            // flight, so no state is admitted or kept for it. This branch used
            // to insert the state and return before `b_partial_final` was ever
            // read, so the next such bunch on the channel was reported as an
            // overlapping initial.
            if header.b_partial_final {
                if overlapping {
                    *partial_error_count += 1;
                    header.has_partial_error = true;
                }
                self.partial_bunches.remove(&header.ch_index);
                header.is_partial_completed = !header.has_partial_error;
                return;
            }

            if !self.partial_bunches.contains_key(&header.ch_index)
                && self.partial_bunches.len() >= self.max_channels
            {
                *partial_error_count += 1;
                header.has_partial_error = true;
                return;
            }
            if overlapping {
                *partial_error_count += 1;
                header.has_partial_error = true;
            }

            self.partial_bunches.insert(
                header.ch_index,
                PartialState {
                    ch_sequence: header.ch_sequence,
                    reliable: header.b_reliable,
                    is_complete: false,
                },
            );
            return;
        }

        // Continuation or final
        let error = self.validate_continuation(header);
        if let Some(kind) = error {
            *partial_error_count += 1;
            header.has_partial_error = true;
            if kind == PartialSequenceKind::MismatchedContinuation {
                self.partial_bunches.remove(&header.ch_index);
            }
            let _ = kind; // consumed for the count
            return;
        }

        if let Some(state) = self.partial_bunches.get_mut(&header.ch_index) {
            state.ch_sequence = header.ch_sequence;

            if header.b_partial_final {
                state.is_complete = true;
                header.is_partial_completed = true;
            }
        }
        if header.b_partial_final {
            self.partial_bunches.remove(&header.ch_index);
        }
    }

    fn validate_continuation(&self, header: &RawBunchHeader) -> Option<PartialSequenceKind> {
        let state = match self.partial_bunches.get(&header.ch_index) {
            None => return Some(PartialSequenceKind::MissingInitial),
            Some(s) => s,
        };

        if state.is_complete {
            return Some(PartialSequenceKind::MissingInitial);
        }

        if state.reliable != header.b_reliable {
            return Some(PartialSequenceKind::MismatchedContinuation);
        }

        let seq_ok = if state.reliable {
            header.ch_sequence == state.ch_sequence + 1
        } else {
            header.ch_sequence == state.ch_sequence + 1 || header.ch_sequence == state.ch_sequence
        };

        if !seq_ok {
            return Some(PartialSequenceKind::MismatchedContinuation);
        }

        None
    }

    fn retire_channel(&mut self, ch_index: u32) {
        self.partial_bunches.remove(&ch_index);
        self.in_reliable_sequence.remove(&ch_index);
    }
}

impl Default for RawPacketReader {
    fn default() -> Self {
        Self::new()
    }
}

/// Compute the true bit size of a packet by finding the sentinel bit.
///
/// The sentinel is the highest `1` bit in the last byte; all bits above it
/// (toward MSB) are padding. The sentinel itself is not data.
///
/// The reference walk is a shift loop:
///
/// ```text
/// bitSize = len*8 - 1
/// while (lastByte & 0x80) == 0: lastByte <<= 1; bitSize -= 1
/// ```
///
/// which runs once per packet and iterates once per padding bit. It counts
/// exactly the leading zeros of the last byte, so `leading_zeros` gives the
/// same answer without the loop. `last_byte` is non-zero here -- the caller
/// rejects a zero last byte as a packet with no sentinel -- so the count is at
/// most 7 and the result never underflows.
///
/// # Panics
///
/// Panics in debug builds if `last_byte` is zero, which would mean the caller
/// skipped the malformed-packet check.
fn compute_bit_size(packet: &[u8], last_byte: u8) -> i32 {
    debug_assert!(last_byte != 0, "caller must reject a zero last byte");
    (packet.len() as i32) * 8 - 1 - last_byte.leading_zeros() as i32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bunch::RawBunchHeader;
    use crate::test_bits::{
        BunchSpec, build_bunch_packet, build_packet, write_bunch, write_bunch_header,
    };

    /// An unreliable bunch on `ch_index` with no other flag set: no control
    /// bits and no channel name.
    fn unreliable(ch_index: u32) -> BunchSpec {
        BunchSpec {
            ch_index,
            b_reliable: false,
            ..Default::default()
        }
    }

    /// A reliable bunch on `ch_index` with no other flag set.
    fn reliable(ch_index: u32) -> BunchSpec {
        BunchSpec {
            ch_index,
            ..Default::default()
        }
    }

    /// A reliable partial fragment on `ch_index`.
    fn fragment(ch_index: u32, initial: bool, last: bool) -> BunchSpec {
        BunchSpec {
            ch_index,
            b_partial: true,
            b_partial_initial: initial,
            b_partial_final: last,
            ..Default::default()
        }
    }

    #[test]
    fn last_byte_zero_returns_malformed() {
        let mut reader = RawPacketReader::new();
        let result = reader.read_packet(&[0x00, 0x00, 0x00], 0, |_, _| {});
        assert!(result.is_malformed);
        assert_eq!(result.bunch_count, 0);
    }

    #[test]
    fn empty_data_returns_zero_bunches() {
        let mut reader = RawPacketReader::new();
        let result = reader.read_packet(&[], 0, |_, _| {});
        assert_eq!(result.bunch_count, 0);
        assert!(!result.is_malformed);
    }

    #[test]
    fn single_bunch_parses_header_fields() {
        let packet = build_bunch_packet(&unreliable(7), &[]);

        let mut reader = RawPacketReader::new();
        let mut captured: Option<RawBunchHeader> = None;
        reader.read_packet(&packet, 2, |h, _| captured = Some(h.clone()));

        let h = captured.unwrap();
        assert_eq!(h.packet_id, 2);
        assert_eq!(h.ch_index, 7);
        assert!(!h.b_open);
        assert!(!h.b_close);
        assert!(!h.b_reliable);
        assert!(!h.b_partial);
        assert_eq!(h.payload_bit_count, 0);
    }

    #[test]
    fn control_bunch_with_close_parses_close_reason() {
        let close = BunchSpec {
            b_close: true,
            dormant: true,
            ..unreliable(3)
        };
        let packet = build_bunch_packet(&close, &[]);

        let mut reader = RawPacketReader::new();
        let mut captured: Option<RawBunchHeader> = None;
        reader.read_packet(&packet, 1, |h, _| captured = Some(h.clone()));

        let h = captured.unwrap();
        assert!(!h.b_open);
        assert!(h.b_close);
        assert!(h.b_dormant);
        assert_eq!(h.close_reason, ChannelCloseReason::Dormancy);
    }

    #[test]
    fn multiple_bunches_parsed() {
        let mut bits = Vec::new();
        write_bunch(&mut bits, &unreliable(0), &[]);
        write_bunch(&mut bits, &unreliable(1), &[]);
        let packet = build_packet(&bits);

        let mut reader = RawPacketReader::new();
        let mut indices = Vec::new();
        reader.read_packet(&packet, 3, |h, _| indices.push(h.ch_index));
        assert_eq!(indices, vec![0, 1]);
    }

    #[test]
    fn captured_partial_headers_assign_boundary_bits_in_wire_order() {
        // Literal header prefixes from packets 790-792 of replay
        // 00e5adab... (13.05). The initial prefix is also byte-identical in
        // packet 1 of replay 02d4d478... (13.01). These bytes were captured
        // before this parser interprets them; the fixture is intentionally not
        // produced by the synthetic header writer below.
        let fixtures = [
            (&[0x10, 0xa0, 0x90, 0x7b][..], true, false, 15_816),
            (&[0x10, 0x20, 0x90, 0x7b][..], false, false, 15_816),
            (&[0x10, 0x20, 0x67, 0x93][..], false, true, 2_483),
        ];

        for (packet_id, (bytes, initial, is_final, payload_bits)) in
            (790..).zip(fixtures.into_iter())
        {
            let mut bits = BitReader::with_bit_len(bytes, 31).unwrap();
            let mut packet_reader = RawPacketReader::new();
            let header = packet_reader
                .parse_bunch_header(&mut bits, packet_id)
                .unwrap();
            assert!(header.b_partial);
            assert_eq!(header.b_partial_initial, initial);
            assert_eq!(header.b_partial_final, is_final);
            assert_eq!(header.payload_bit_count, payload_bits);
            assert_eq!(header.payload_bit_offset, 31);
        }
    }

    #[test]
    fn partial_initial_then_final_completes() {
        let mut bits = Vec::new();
        write_bunch(&mut bits, &fragment(2, true, false), &[false; 8]);
        write_bunch(&mut bits, &fragment(2, false, true), &[false; 4]);
        let packet = build_packet(&bits);

        let mut reader = RawPacketReader::new();
        let mut headers = Vec::new();
        let result = reader.read_packet(&packet, 0, |h, _| headers.push(h.clone()));

        assert_eq!(headers.len(), 2);
        assert_eq!(result.partial_error_count, 0);
        assert!(headers[0].b_partial_initial);
        assert!(!headers[0].has_partial_error);
        assert!(headers[1].b_partial_final);
        assert!(headers[1].is_partial_completed);
        assert_eq!(
            reader.partial_bunches.len(),
            0,
            "completed packet-level partial state must be retired"
        );
    }

    /// A partial that is both initial and final is a whole bunch: nothing is
    /// left in flight after it. The tracker kept its state (it returned from
    /// the initial branch before looking at `b_partial_final`), so the next
    /// such bunch on the channel was reported as an overlapping initial.
    #[test]
    fn an_initial_final_partial_leaves_no_tracker_state() {
        let mut bits = Vec::new();
        for _ in 0..2 {
            write_bunch(&mut bits, &fragment(2, true, true), &[false; 8]);
        }
        let packet = build_packet(&bits);

        let mut reader = RawPacketReader::new();
        let mut headers = Vec::new();
        let result = reader.read_packet(&packet, 0, |h, _| headers.push(h.clone()));

        assert_eq!(headers.len(), 2);
        assert_eq!(result.partial_error_count, 0);
        assert!(!headers[1].has_partial_error, "not an overlapping initial");
        assert!(headers[0].is_partial_completed && headers[1].is_partial_completed);
        assert!(reader.partial_bunches.is_empty());
    }

    #[test]
    fn continuation_without_initial_reports_error() {
        let packet = build_bunch_packet(&fragment(5, false, true), &[]);

        let mut reader = RawPacketReader::new();
        let mut headers = Vec::new();
        let result = reader.read_packet(&packet, 2, |h, _| headers.push(h.clone()));
        assert_eq!(result.partial_error_count, 1);
        assert!(headers[0].has_partial_error);
    }

    #[test]
    fn reliability_mismatch_reports_error() {
        let mut bits = Vec::new();
        write_bunch(&mut bits, &fragment(2, true, false), &[]);
        let unreliable_final = BunchSpec {
            b_reliable: false,
            ..fragment(2, false, true)
        };
        write_bunch(&mut bits, &unreliable_final, &[]);
        let packet = build_packet(&bits);

        let mut reader = RawPacketReader::new();
        let mut headers = Vec::new();
        let result = reader.read_packet(&packet, 2, |h, _| headers.push(h.clone()));
        assert_eq!(result.partial_error_count, 1);
        assert!(headers[1].has_partial_error);
    }

    #[test]
    fn reliable_sequence_advances_per_channel_not_globally() {
        // Two channels interleaving reliable bunches. Unreal's ReliableSequence
        // is per channel, so each channel numbers its own bunches 1 then 2. A
        // single global counter hands out 1, 2, 3, 4 instead, and a later
        // continuation check would reject the valid bunch as a mismatch.
        let mut bits = Vec::new();
        write_bunch(&mut bits, &reliable(2), &[]);
        write_bunch(&mut bits, &reliable(5), &[]);
        write_bunch(&mut bits, &reliable(2), &[]);
        write_bunch(&mut bits, &reliable(5), &[]);
        let packet = build_packet(&bits);

        let mut reader = RawPacketReader::new();
        let mut seen = Vec::new();
        reader.read_packet(&packet, 0, |h, _| seen.push((h.ch_index, h.ch_sequence)));

        assert_eq!(seen, vec![(2, 1), (5, 1), (2, 2), (5, 2)]);
    }

    #[test]
    fn reliable_sequence_state_refuses_new_channel_keys_past_its_budget() {
        let mut reader = RawPacketReader::with_max_channels(1);
        let mut bits = Vec::new();
        write_bunch(&mut bits, &reliable(2), &[]);
        write_bunch(&mut bits, &reliable(5), &[]);
        let packet = build_packet(&bits);
        let mut headers = Vec::new();
        let result = reader.read_packet(&packet, 0, |header, _| headers.push(header.clone()));

        assert_eq!(reader.in_reliable_sequence.len(), 1);
        assert_eq!(result.channel_limit_count, 1);
        assert!(!headers[0].has_channel_limit_error);
        assert!(headers[1].has_channel_limit_error);
    }

    #[test]
    fn reliable_sequence_overflow_fails_closed_without_panicking() {
        let mut reader = RawPacketReader::new();
        reader.in_reliable_sequence.insert(2, i32::MAX);
        let mut headers = Vec::new();
        let result = reader.read_packet(&build_bunch_packet(&reliable(2), &[]), 0, |header, _| {
            headers.push(header.clone())
        });

        assert_eq!(result.channel_limit_count, 1);
        assert!(headers[0].has_channel_limit_error);
        assert_eq!(reader.in_reliable_sequence.get(&2), Some(&i32::MAX));
    }

    #[test]
    fn destroying_then_reusing_a_reliable_channel_restarts_its_state() {
        let mut reader = RawPacketReader::new();
        let mut seen = Vec::new();
        reader.read_packet(&build_bunch_packet(&reliable(2), &[]), 0, |header, _| {
            seen.push(header.ch_sequence)
        });

        let destroy = BunchSpec {
            b_close: true,
            ..reliable(2)
        };
        reader.read_packet(&build_bunch_packet(&destroy, &[]), 1, |header, _| {
            seen.push(header.ch_sequence)
        });

        reader.read_packet(&build_bunch_packet(&reliable(2), &[]), 2, |header, _| {
            seen.push(header.ch_sequence)
        });
        assert_eq!(seen, [1, 2, 1]);
    }

    #[test]
    fn payload_overrun_returns_malformed() {
        let mut bits = Vec::new();
        write_bunch_header(&mut bits, &unreliable(0), 17); // claims 17 bits payload
        // but we don't write any payload bits
        let packet = build_packet(&bits);

        let mut reader = RawPacketReader::new();
        let mut count = 0;
        let result = reader.read_packet(&packet, 0, |_, _| count += 1);
        assert!(result.is_malformed);
        assert_eq!(count, 0);
    }

    #[test]
    fn payload_bits_consumed_stream_stays_aligned() {
        let packet = build_bunch_packet(&unreliable(0), &[false; 17]);

        let mut reader = RawPacketReader::new();
        let mut count = 0;
        reader.read_packet(&packet, 0, |_, _| count += 1);
        assert_eq!(count, 1);
    }
}
