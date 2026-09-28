//! Decoder tests: build a whole RPC payload bit by bit, then decode it. The
//! `BitWriter` is written out rather than shared with `vrf-bitio`, so a bug
//! mirrored in both cannot cancel out.
use vrf_bitio::BitReader;

use crate::error::MovementError;
use crate::moves::{MOVEMENT_MAGIC, next_marker};
use crate::primitives::{read_quantized_vector, read_signed_quantized_components};
use crate::rpc::{
    COMPONENT_DATA_STREAM_HANDLE, REMOTE_CHARACTER_UPDATES_HANDLE,
    SHOOTER_CHARACTER_NET_GUID_HANDLE, decode_movement_rpc,
};

struct BitWriter {
    bits: Vec<bool>,
}

impl BitWriter {
    fn new() -> Self {
        Self { bits: Vec::new() }
    }

    fn write_bit(&mut self, v: bool) {
        self.bits.push(v);
    }

    fn write_bits_u64(&mut self, value: u64, count: u32) {
        for i in 0..count {
            self.bits.push((value >> i) & 1 != 0);
        }
    }

    fn write_u8(&mut self, v: u8) {
        self.write_bits_u64(u64::from(v), 8);
    }

    fn write_u16(&mut self, v: u16) {
        self.write_bits_u64(u64::from(v), 16);
    }

    fn write_u32(&mut self, v: u32) {
        self.write_bits_u64(u64::from(v), 32);
    }

    fn write_f32(&mut self, v: f32) {
        self.write_u32(v.to_bits());
    }

    fn write_int_packed(&mut self, mut value: u32) {
        loop {
            let mut next_byte = ((value & 0x7F) << 1) as u8;
            value >>= 7;
            if value != 0 {
                next_byte |= 1;
            }
            self.write_u8(next_byte);
            if value == 0 {
                break;
            }
        }
    }

    fn write_serialized_int(&mut self, value: u32, max_value: u32) {
        let mut written_value = 0u32;
        let mut mask = 1u32;
        while written_value.saturating_add(mask) < max_value {
            let bit = (value & mask) != 0;
            self.write_bit(bit);
            if bit {
                written_value |= mask;
            }
            mask <<= 1;
        }
    }

    fn write_other(&mut self, other: &BitWriter) {
        self.bits.extend_from_slice(&other.bits);
    }

    fn bit_count(&self) -> u32 {
        self.bits.len() as u32
    }

    fn to_bytes(&self) -> Vec<u8> {
        let byte_count = self.bits.len().div_ceil(8);
        let mut bytes = vec![0u8; byte_count];
        for (i, &bit) in self.bits.iter().enumerate() {
            if bit {
                bytes[i >> 3] |= 1 << (i & 7);
            }
        }
        bytes
    }
}

/// Build a single move payload (variant 0 or variant 1).
fn build_move(variant1: bool, timestamp: u32, x: f32, y: f32, z: f32) -> BitWriter {
    let mut w = BitWriter::new();

    // 25-bit header: moveType(1) + rotationYawMultiplier(8) + movementState(8) + unused(8)
    w.write_bit(variant1);
    w.write_u8(2);
    w.write_u8(3);
    w.write_u8(0);

    // rotationInput: 3 x u16 at the centre, 0x8000
    w.write_serialized_int(0x8000, 0x10000);
    w.write_serialized_int(0x8000, 0x10000);
    w.write_serialized_int(0x8000, 0x10000);

    w.write_int_packed(timestamp);

    // Position: info 0 (componentBits 0, extraInfo 0) selects 3 x f32
    w.write_serialized_int(0, 128);
    w.write_f32(x);
    w.write_f32(y);
    w.write_f32(z);

    w.write_bit(false); // hasOptionalByte

    // 33-bit flag48(1) + packedAngles(32)
    w.write_bit(false);
    w.write_u32(0);

    if variant1 {
        // variant1Flag, then velocity (4, 5, 6) at scale 10: componentBits 10,
        // extraInfo 1, components 40, 50, 60
        w.write_bit(true);
        let info = 10u32 | (1 << 6);
        w.write_serialized_int(info, 128);
        let vx = 40i64 as u64 & 0x3FF;
        let vy = 50i64 as u64 & 0x3FF;
        let vz = 60i64 as u64 & 0x3FF;
        let packed = vx | (vy << 10) | (vz << 20);
        w.write_bits_u64(packed, 30);
    } else {
        // variant 0: hasExternalCharacterRef 0, then 32 bits of angles
        w.write_bit(false);
        w.write_u32(0);
    }

    w.write_bit(false); // errorSentinel

    w
}

/// Bits after the envelope of a byte-wrapped stream. The decoder never reads
/// them; the value only has to be non-zero.
const ENVELOPE_TRAILER_BITS: u32 = 24;

/// Build a ComponentDataStream in the shape real replays use (crate docs,
/// "Measured on real replays"): byte-wrapped, inner movementBitCount 0, a
/// section ending in a `000` terminator plus non-zero bits, then a trailer.
/// Counted independently with temporary counters over `vrfkit validate` on
/// 02d4d478 (13.01), 02eef9e2 (13.06) and one 11.06 replay: all 6,167,472
/// streams, with 11 to 26 bits after the last move (`000`, then non-zero)
/// and 24 after the envelope.
fn build_component_data_stream(moves: &[BitWriter]) -> BitWriter {
    byte_wrapped(&open_section_payload(moves), ENVELOPE_TRAILER_BITS)
}

/// The envelope's payload: inner movementBitCount 0 (the section runs to the
/// envelope's end), the magic, `moves` under the marker sequence, a `000`
/// terminator and 8 to 15 non-zero bits ending on a byte boundary. The 11 to
/// 18 bits after the last move are within the 31 the decoder leaves unread.
fn open_section_payload(moves: &[BitWriter]) -> BitWriter {
    assert!(
        !moves.is_empty(),
        "a real section carries at least one move"
    );
    let mut payload = BitWriter::new();
    payload.write_u16(0);
    payload.write_u8(MOVEMENT_MAGIC);
    let mut marker: u8 = 1;
    for mv in moves {
        payload.write_bits_u64(u64::from(marker), 3);
        payload.write_other(mv);
        marker = next_marker(marker);
    }
    payload.write_bits_u64(0, 3);
    let tail = 8 + (8 - (payload.bit_count() + 8) % 8) % 8;
    for i in 0..tail {
        payload.write_bit(i % 2 == 0);
    }
    payload
}

/// Wrap `payload`, a whole number of bytes, in the envelope: a u16 byte
/// count, the payload, then `trailer_bits` non-zero bits outside it.
fn byte_wrapped(payload: &BitWriter, trailer_bits: u32) -> BitWriter {
    assert_eq!(payload.bit_count() % 8, 0, "an envelope holds whole bytes");
    let mut stream = BitWriter::new();
    stream.write_u16((payload.bit_count() / 8) as u16);
    stream.write_other(payload);
    for i in 0..trailer_bits {
        stream.write_bit(i % 3 == 0);
    }
    stream
}

/// Build a ComponentDataStream in the direct form: no envelope, the u16 being
/// the section's own bit count. Accepted, as by the C#; never seen in replays.
fn build_direct_component_data_stream(moves: &[BitWriter]) -> BitWriter {
    let mut movement = BitWriter::new();
    movement.write_u8(MOVEMENT_MAGIC);

    let mut marker: u8 = 1;
    for (i, mv) in moves.iter().enumerate() {
        movement.write_bits_u64(u64::from(marker), 3);
        movement.write_other(mv);
        if i + 1 < moves.len() {
            marker = next_marker(marker);
        }
    }
    // A 0 marker, 3 bits after the last move: the decoder stops before it.
    if !moves.is_empty() {
        movement.write_bits_u64(0, 3);
    }

    let mut payload = BitWriter::new();
    payload.write_u16(movement.bit_count() as u16);
    payload.write_other(&movement);
    payload
}

/// Build a full RPC payload with one character update.
fn build_rpc_payload(shooter_guid: u32, component_stream: &BitWriter) -> BitWriter {
    wrap_updates_array(&single_update_array(&update_with_stream(
        shooter_guid,
        component_stream,
    )))
}

/// One update carrying the shooter GUID field (handle 2, 32 bits) and then
/// `stream` as the ComponentDataStream (handle 3).
fn update_with_stream(shooter_guid: u32, stream: &BitWriter) -> BitWriter {
    let mut update = BitWriter::new();
    update.write_int_packed(SHOOTER_CHARACTER_NET_GUID_HANDLE + 1);
    update.write_int_packed(32);
    update.write_u32(shooter_guid);
    update.write_int_packed(COMPONENT_DATA_STREAM_HANDLE + 1);
    update.write_int_packed(stream.bit_count());
    update.write_other(stream);
    update.write_int_packed(0);
    update
}

/// An updates array that declares one update and holds `update` at index 0.
fn single_update_array(update: &BitWriter) -> BitWriter {
    let mut array = BitWriter::new();
    array.write_int_packed(1);
    array.write_int_packed(1);
    array.write_other(update);
    array.write_int_packed(0);
    array
}

/// Wrap a RemoteCharacterUpdates array in the RPC envelope. The framing
/// tests below deform the array itself and call this directly.
fn wrap_updates_array(array: &BitWriter) -> BitWriter {
    let mut rpc = BitWriter::new();
    rpc.write_bit(false); // first bit, discarded
    rpc.write_int_packed(REMOTE_CHARACTER_UPDATES_HANDLE + 1);
    rpc.write_int_packed(array.bit_count());
    rpc.write_other(array);
    rpc.write_int_packed(0);
    rpc
}

/// Decode a built payload, returning the result and the moves it emitted.
fn decode(
    rpc: &BitWriter,
) -> (
    crate::types::RpcDecodeResult,
    Vec<crate::types::MovementMove>,
) {
    let bytes = rpc.to_bytes();
    let mut reader = BitReader::with_bit_len(&bytes, u64::from(rpc.bit_count())).unwrap();
    let mut moves = Vec::new();
    let result = decode_movement_rpc(&mut reader, |m| moves.push(m)).unwrap();
    (result, moves)
}

#[test]
fn an_out_of_range_update_index_is_counted_not_discarded_in_silence() {
    // Uncounted, the skipped window looks like well-formed empty updates.
    let mut array = BitWriter::new();
    array.write_int_packed(1); // updateCount
    array.write_int_packed(3); // index 2, out of range
    array.write_u8(0xAA);

    let (result, moves) = decode(&wrap_updates_array(&array));

    assert_eq!(result.update_count, 1);
    assert_eq!(result.total_moves, 0);
    assert!(moves.is_empty());
    assert_eq!(
        result.error_count, 1,
        "the discarded array tail must reach the summary"
    );
}

#[test]
fn a_field_longer_than_the_update_window_is_counted() {
    // The framing no longer describes the payload, and the second declared
    // update goes with the window.
    let mut array = BitWriter::new();
    array.write_int_packed(2); // updateCount
    array.write_int_packed(1); // index 0
    array.write_int_packed(6); // handle 5, not decoded here
    array.write_int_packed(128); // ... declaring 128 bits
    array.write_u8(0); // ... with only 8 left

    let (result, moves) = decode(&wrap_updates_array(&array));

    assert_eq!(result.update_count, 2);
    assert_eq!(result.total_moves, 0);
    assert!(moves.is_empty());
    assert_eq!(
        result.error_count, 1,
        "an over-long field declaration must reach the summary"
    );
}

#[test]
fn an_undersized_shooter_guid_field_and_its_orphaned_stream_are_counted() {
    // A 31-bit GUID field leaves no GUID, so the stream after it is consumed
    // undecoded: two losses.
    let stream = build_component_data_stream(&[build_move(false, 7, 1.0, 2.0, 3.0)]);

    let mut update = BitWriter::new();
    update.write_int_packed(SHOOTER_CHARACTER_NET_GUID_HANDLE + 1);
    update.write_int_packed(31); // one bit short of a u32
    update.write_bits_u64(0, 31);
    update.write_int_packed(COMPONENT_DATA_STREAM_HANDLE + 1);
    update.write_int_packed(stream.bit_count());
    update.write_other(&stream);
    update.write_int_packed(0);

    let (result, moves) = decode(&wrap_updates_array(&single_update_array(&update)));

    assert_eq!(result.total_moves, 0);
    assert!(moves.is_empty());
    assert_eq!(
        result.error_count, 2,
        "the undersized GUID and the stream it orphaned are counted separately"
    );
}

#[test]
fn a_component_stream_ahead_of_its_guid_is_counted() {
    // Handle 3 before handle 2: a single pass cannot rewind to the GUID, so
    // the stream is dropped, and counted.
    let stream = build_component_data_stream(&[build_move(false, 7, 1.0, 2.0, 3.0)]);

    let mut update = BitWriter::new();
    update.write_int_packed(COMPONENT_DATA_STREAM_HANDLE + 1);
    update.write_int_packed(stream.bit_count());
    update.write_other(&stream);
    update.write_int_packed(SHOOTER_CHARACTER_NET_GUID_HANDLE + 1);
    update.write_int_packed(32);
    update.write_u32(4321);
    update.write_int_packed(0);

    let (result, moves) = decode(&wrap_updates_array(&single_update_array(&update)));

    assert_eq!(result.total_moves, 0);
    assert!(moves.is_empty());
    assert_eq!(result.error_count, 1, "the orphaned stream must be counted");
}

#[test]
fn a_component_stream_shorter_than_its_u16_header_is_not_a_valid_empty_update() {
    let mut short = BitWriter::new();
    short.write_u8(0x52); // fewer than the mandatory 16 header bits
    let (result, moves) = decode(&build_rpc_payload(4321, &short));

    assert!(moves.is_empty());
    assert_eq!(result.error_count, 1, "short component must be a loss");
}

#[test]
fn a_stream_that_fails_to_decode_does_not_drop_the_updates_after_it() {
    // The decoder is past the whole length-delimited stream before reading
    // it, so a failure inside costs only that stream, as in the C#.
    let mut bad_magic = BitWriter::new();
    bad_magic.write_u16(0);
    bad_magic.write_u8(0x00); // not MOVEMENT_MAGIC
    let bad_magic = byte_wrapped(&bad_magic, ENVELOPE_TRAILER_BITS);
    let mut short_header = BitWriter::new();
    short_header.write_u8(0x52); // fewer than the 16 header bits
    let good = build_component_data_stream(&[build_move(true, 7, 1.0, 2.0, 3.0)]);

    for (name, bad) in [("bad magic", bad_magic), ("short header", short_header)] {
        let mut array = BitWriter::new();
        array.write_int_packed(2); // updateCount
        array.write_int_packed(1); // index 0
        array.write_other(&update_with_stream(1111, &bad));
        array.write_int_packed(2); // index 1
        array.write_other(&update_with_stream(2222, &good));
        array.write_int_packed(0);

        let (result, moves) = decode(&wrap_updates_array(&array));

        assert_eq!(result.update_count, 2, "{name}");
        assert_eq!(result.error_count, 1, "{name}: the failed stream, once");
        assert_eq!(result.total_moves, 1, "{name}: the next update's move");
        assert_eq!(moves.len(), 1, "{name}");
        assert_eq!(moves[0].shooter_character_net_guid, 2222, "{name}");
    }
}

#[test]
fn a_malformed_trailing_padding_byte_is_counted() {
    // 8 bits after the array terminator are read as an IntPacked, and this
    // one's continuation bit asks for a byte that is not there.
    let mut array = BitWriter::new();
    array.write_int_packed(0); // updateCount
    array.write_int_packed(0); // array terminator
    array.write_u8(0x01); // continuation set, nothing follows

    let (result, moves) = decode(&wrap_updates_array(&array));

    assert_eq!(result.total_moves, 0);
    assert!(moves.is_empty());
    assert_eq!(
        result.error_count, 1,
        "a trailing byte that does not parse must reach the summary"
    );
}

#[test]
fn decodes_single_variant0_move() {
    let stream = build_component_data_stream(&[build_move(false, 42, 1.25, 2.5, 3.75)]);
    let (result, moves) = decode(&build_rpc_payload(1234, &stream));

    assert_eq!(result.total_moves, 1);
    assert_eq!(result.update_count, 1);
    assert_eq!(result.error_count, 0);
    assert_eq!(moves.len(), 1);
    assert_eq!(moves[0].shooter_character_net_guid, 1234);
    assert_eq!(moves[0].move_type, 0);
    assert_eq!(moves[0].timestamp, 42);
    assert!((moves[0].pos_x - 1.25).abs() < 0.001);
    assert!((moves[0].pos_y - 2.5).abs() < 0.001);
    assert!((moves[0].pos_z - 3.75).abs() < 0.001);
    assert_eq!(moves[0].vel_x, 0.0);
    assert_eq!(moves[0].vel_y, 0.0);
    assert_eq!(moves[0].vel_z, 0.0);
}

#[test]
fn decodes_single_variant1_move_with_velocity() {
    let stream = build_component_data_stream(&[build_move(true, 42, 1.25, 2.5, 3.75)]);
    let (result, moves) = decode(&build_rpc_payload(5678, &stream));

    assert_eq!(result.total_moves, 1);
    assert_eq!(moves[0].move_type, 1);
    assert!((moves[0].vel_x - 4.0).abs() < 0.001);
    assert!((moves[0].vel_y - 5.0).abs() < 0.001);
    assert!((moves[0].vel_z - 6.0).abs() < 0.001);
}

#[test]
fn decodes_two_moves_in_one_update() {
    let stream = build_component_data_stream(&[
        build_move(false, 42, 1.0, 2.0, 3.0),
        build_move(false, 84, 10.0, 11.0, 12.0),
    ]);
    let (result, moves) = decode(&build_rpc_payload(9999, &stream));

    assert_eq!(result.total_moves, 2);
    assert_eq!(moves.len(), 2);
    assert_eq!(moves[0].timestamp, 42);
    assert!((moves[0].pos_x - 1.0).abs() < 0.001);
    assert_eq!(moves[1].timestamp, 84);
    assert!((moves[1].pos_x - 10.0).abs() < 0.001);
}

#[test]
fn decodes_the_direct_component_form() {
    // The one round trip through the direct form.
    let stream = build_direct_component_data_stream(&[
        build_move(false, 42, 1.0, 2.0, 3.0),
        build_move(true, 84, 10.0, 11.0, 12.0),
    ]);
    let (result, moves) = decode(&build_rpc_payload(9999, &stream));

    assert_eq!(result.error_count, 0);
    assert_eq!(result.total_moves, 2);
    assert_eq!(moves.len(), 2);
    assert_eq!((moves[0].move_type, moves[0].timestamp), (0, 42));
    assert!((moves[0].pos_x - 1.0).abs() < 0.001);
    assert_eq!((moves[1].move_type, moves[1].timestamp), (1, 84));
    assert!((moves[1].pos_z - 12.0).abs() < 0.001);
    assert!((moves[1].vel_y - 5.0).abs() < 0.001);
}

#[test]
fn empty_rpc_returns_zero() {
    let (result, moves) = decode(&BitWriter::new());
    assert_eq!(result.total_moves, 0);
    assert!(moves.is_empty());
}

#[test]
fn invalid_magic_returns_error() {
    // A lost stream inside the update: the RPC itself is Ok.
    let mut payload = BitWriter::new();
    payload.write_u16(8); // movementBitCount
    payload.write_u8(0x00); // wrong magic

    let (result, moves) = decode(&build_rpc_payload(1234, &payload));
    assert!(moves.is_empty());
    assert_eq!(result.error_count, 1);
}

/// A movement section: the magic, then `moves` variant-1 moves under the
/// usual marker sequence, then an explicit 3-bit zero marker and `tail_bits`
/// set bits the decoder has no reason to read.
fn section_with_zero_marker_and_tail(moves: usize, tail_bits: u32) -> BitWriter {
    let mut section = BitWriter::new();
    section.write_u8(MOVEMENT_MAGIC);
    let mut marker: u8 = 1;
    for i in 0..moves {
        section.write_bits_u64(u64::from(marker), 3);
        section.write_other(&build_move(true, 10 + i as u32, 1.0, 2.0, 3.0));
        marker = next_marker(marker);
    }
    section.write_bits_u64(0, 3);
    for _ in 0..tail_bits {
        section.write_bit(true);
    }
    section
}

/// Wrap `section` in a direct component stream. `sized` declares its length
/// as `movementBitCount`; otherwise the count is 0 and the window is open.
fn component_stream(section: &BitWriter, sized: bool) -> BitWriter {
    let mut payload = BitWriter::new();
    payload.write_u16(if sized { section.bit_count() as u16 } else { 0 });
    payload.write_other(section);
    payload
}

/// `(sized tails, sized tail bits, open tails, open tail bits)`.
fn tails(result: &crate::types::RpcDecodeResult) -> (u32, u64, u32, u64) {
    (
        result.sized_section_tails,
        result.sized_section_tail_bits,
        result.open_section_tails,
        result.open_section_tail_bits,
    )
}

#[test]
fn a_zero_marker_with_bits_left_in_a_sized_section_is_tallied() {
    // 43 bits after the move (more than 31), so another marker is read: 0,
    // leaving 40. A tally, not an error: it must not change which batches
    // keep raw bits.
    let stream = component_stream(&section_with_zero_marker_and_tail(1, 40), true);
    let (result, moves) = decode(&build_rpc_payload(77, &stream));

    assert_eq!(result.total_moves, 1);
    assert_eq!(moves.len(), 1);
    assert_eq!(result.error_count, 0, "a tally, not an error");
    assert_eq!(tails(&result), (1, 40, 0, 0));
}

#[test]
fn a_sized_section_that_ends_right_after_its_first_marker_is_tallied() {
    let stream = component_stream(&section_with_zero_marker_and_tail(0, 40), true);
    let (result, moves) = decode(&build_rpc_payload(77, &stream));

    assert!(moves.is_empty());
    assert_eq!(result.error_count, 0);
    assert_eq!(tails(&result), (1, 40, 0, 0));
}

#[test]
fn a_sized_window_too_short_for_the_magic_is_tallied() {
    // Five bits cannot hold the magic: the C#'s "Missing movement magic".
    let mut section = BitWriter::new();
    section.write_bits_u64(0b10110, 5);
    let (result, moves) = decode(&build_rpc_payload(77, &component_stream(&section, true)));

    assert!(moves.is_empty());
    assert_eq!(result.error_count, 0);
    assert_eq!(tails(&result), (1, 5, 0, 0));
}

#[test]
fn a_window_too_short_for_the_first_marker_is_tallied() {
    // The magic, then two bits: the C#'s "Missing first movement marker".
    let mut section = BitWriter::new();
    section.write_u8(MOVEMENT_MAGIC);
    section.write_bits_u64(0b11, 2);
    let (result, moves) = decode(&build_rpc_payload(77, &component_stream(&section, true)));

    assert!(moves.is_empty());
    assert_eq!(result.error_count, 0);
    assert_eq!(tails(&result), (1, 2, 0, 0));
}

#[test]
fn an_open_window_tail_is_tallied_apart_from_a_sized_one() {
    // In an open window what follows a zero marker may be other component
    // data, so it is counted apart and never summed with the sized kind.
    let stream = component_stream(&section_with_zero_marker_and_tail(1, 40), false);
    let (result, moves) = decode(&build_rpc_payload(77, &stream));

    assert_eq!(moves.len(), 1);
    assert_eq!(result.error_count, 0);
    assert_eq!(tails(&result), (0, 0, 1, 40));
}

#[test]
fn padding_after_the_last_move_and_an_empty_window_are_not_tails() {
    // The section's end: the builder's terminator and 8 to 15 bits after it,
    // within 31 of the last move and never read.
    let stream = build_component_data_stream(&[
        build_move(false, 42, 1.0, 2.0, 3.0),
        build_move(true, 84, 10.0, 11.0, 12.0),
    ]);
    let (result, moves) = decode(&build_rpc_payload(9999, &stream));
    assert_eq!(moves.len(), 2);
    assert_eq!(tails(&result), (0, 0, 0, 0));

    // An empty open window leaves nothing unread, although the C# reports
    // "Missing movement magic": the tally counts unread bits.
    let mut empty = BitWriter::new();
    empty.write_u16(0);
    let (result, moves) = decode(&build_rpc_payload(9999, &empty));
    assert!(moves.is_empty());
    assert_eq!(result.error_count, 0);
    assert_eq!(tails(&result), (0, 0, 0, 0));

    // Nor does a section ending exactly on its zero marker, after a move
    // (left unread at the end) or after the magic (read as the end).
    for moves_before in [1, 0] {
        let exact = component_stream(&section_with_zero_marker_and_tail(moves_before, 0), true);
        let (result, moves) = decode(&build_rpc_payload(9999, &exact));
        assert_eq!(moves.len(), moves_before);
        assert_eq!(result.error_count, 0);
        assert_eq!(tails(&result), (0, 0, 0, 0), "{moves_before} move(s)");
    }
}

// --- QuantizedVector component widths ---------------------------------------

/// A QuantizedVector: the `SerializedInt(128)` header, then three components.
fn quantized(component_bits: u32, extra_info: u64, comps: [u64; 3]) -> BitWriter {
    let mut w = BitWriter::new();
    // `read_serialized_int(128)` spends `128.ilog2() == 7` bits and never
    // the extra one, because `value + 128 >= 128` holds for every value.
    w.write_bits_u64((extra_info << 6) | u64::from(component_bits), 7);
    for c in comps {
        w.write_bits_u64(c, component_bits);
    }
    w
}

#[test]
fn component_bits_of_63_reads_all_189_declared_bits() {
    // The widest width the header can express. A bound of 62 once returned
    // (0, 0, 0) here without consuming the 189 declared bits.
    let most_negative = 1u64 << 62; // -2^62 in 63-bit two's complement
    let minus_one = (1u64 << 63) - 1; // all 63 bits set
    let w = quantized(63, 0, [1, minus_one, most_negative]);
    let bytes = w.to_bytes();
    let mut r = BitReader::with_bit_len(&bytes, u64::from(w.bit_count())).unwrap();

    let (x, y, z) = read_quantized_vector(&mut r, 100).unwrap();

    assert_eq!(x, 1.0);
    assert_eq!(y, -1.0);
    assert_eq!(z, -(2f64.powi(62)));
    assert_eq!(r.position(), 7 + 189, "all three components must be read");
    assert!(r.at_end());
}

#[test]
fn component_bits_of_62_still_reads_its_186_bits() {
    // 62 always worked; this pins the old boundary so a bound change breaks
    // a test, not a corpus.
    let w = quantized(62, 0, [7, (1u64 << 62) - 1, 1u64 << 61]);
    let bytes = w.to_bytes();
    let mut r = BitReader::with_bit_len(&bytes, u64::from(w.bit_count())).unwrap();

    let (x, y, z) = read_quantized_vector(&mut r, 100).unwrap();

    assert_eq!(x, 7.0);
    assert_eq!(y, -1.0);
    assert_eq!(z, -(2f64.powi(61)));
    assert_eq!(r.position(), 7 + 186);
}

#[test]
#[should_panic(expected = "component_bits must be 1..=63")]
fn a_width_the_header_cannot_express_is_refused_even_without_debug_assertions() {
    // A real assert, so it holds in release, which has no debug assertions
    // (run with `-C debug-assertions=off` to see it).
    let data = [0xFFu8; 32];
    let mut r = BitReader::with_bit_len(&data, 256).unwrap();
    let _ = read_signed_quantized_components(&mut r, 64);
}

#[test]
fn a_truncated_63_bit_vector_reports_eof_rather_than_a_zero_vector() {
    // Refusing to fabricate: a short payload fails instead of returning the
    // origin.
    let mut w = quantized(63, 0, [1, 1, 1]);
    w.bits.truncate(7 + 100);
    let bytes = w.to_bytes();
    let mut r = BitReader::with_bit_len(&bytes, u64::from(w.bit_count())).unwrap();

    assert!(matches!(
        read_quantized_vector(&mut r, 100),
        Err(MovementError::Bit(_))
    ));
}
