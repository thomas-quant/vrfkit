//! Decoder tests: build a full RPC payload bit by bit, then decode it.
//!
//! These live in their own module because they exercise the whole stack --
//! `rpc` framing down through `moves` into `primitives` -- so they belong to
//! none of those modules individually. The `BitWriter` below is the inverse of
//! the reader under test and is deliberately written out rather than shared
//! with `vrf-bitio`: a bug mirrored in both would cancel out.
use vrf_bitio::BitReader;

use crate::error::MovementError;
use crate::moves::{MOVEMENT_MAGIC, next_marker};
use crate::primitives::{read_quantized_vector, read_signed_quantized_components};
use crate::rpc::{
    COMPONENT_DATA_STREAM_HANDLE, REMOTE_CHARACTER_UPDATES_HANDLE,
    SHOOTER_CHARACTER_NET_GUID_HANDLE, decode_movement_rpc,
};

/// Helper: build a bit vector from individual bit values, then convert to bytes.
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
    w.write_bit(variant1); // moveType
    w.write_u8(2); // rotationYawMultiplier
    w.write_u8(3); // movementState
    w.write_u8(0); // unused

    // FixedVector rotationInput: 3 x u16 = 48 bits (all zero = center)
    w.write_serialized_int(0x8000, 0x10000);
    w.write_serialized_int(0x8000, 0x10000);
    w.write_serialized_int(0x8000, 0x10000);

    // Timestamp VLQ (= IntPacked)
    w.write_int_packed(timestamp);

    // Position: QuantizedVector(scaleFactor=100)
    // Use componentBits=0, extraInfo=0 -> 3 x f32
    w.write_serialized_int(0, 128); // info = 0 -> componentBits=0, extraInfo=0
    w.write_f32(x);
    w.write_f32(y);
    w.write_f32(z);

    // hasOptionalByte = false
    w.write_bit(false);

    // 33-bit flag+packedAngles: flag48(1) + packedAngles(32)
    w.write_bit(false); // flag48
    w.write_u32(0); // packedAngles (pitch=0, yaw=0)

    if variant1 {
        // variant1Flag + quantized velocity
        w.write_bit(true);
        // QuantizedVector(scaleFactor=10): componentBits=10, extraInfo=1
        let info = 10u32 | (1 << 6); // componentBits=10, extraInfo=1
        w.write_serialized_int(info, 128);
        // 3 signed components of 10 bits each = 30 bits total
        // velocity = (4.0, 5.0, 6.0) -> scaled by 10 = (40, 50, 60)
        let vx = 40i64 as u64 & 0x3FF;
        let vy = 50i64 as u64 & 0x3FF;
        let vz = 60i64 as u64 & 0x3FF;
        let packed = vx | (vy << 10) | (vz << 20);
        w.write_bits_u64(packed, 30);
    } else {
        // variant0: 33-bit flag+angles
        w.write_bit(false); // hasExternalCharacterRef = false
        w.write_u32(0); // variant0PackedAngles
    }

    // errorSentinel = false
    w.write_bit(false);

    w
}

/// Bits after the envelope of a byte-wrapped stream. The decoder never reads
/// them; the value only has to be non-zero.
const ENVELOPE_TRAILER_BITS: u32 = 24;

/// Build a ComponentDataStream in the shape real replays use: a byte-wrapped
/// envelope whose inner movementBitCount is 0, a section that ends in a `000`
/// terminator plus non-zero bits, and a trailer after the envelope.
///
/// Counted with temporary counters over `vrfkit validate` on 02d4d478
/// (13.01), 02eef9e2 (13.06) and one 11.06 replay, 6,167,472 streams: every
/// one is byte-wrapped with inner movementBitCount 0, its last move is
/// followed by 11 to 26 bits that start with `000` and are non-zero after it,
/// and exactly 24 bits follow the envelope.
fn build_component_data_stream(moves: &[BitWriter]) -> BitWriter {
    byte_wrapped(&open_section_payload(moves), ENVELOPE_TRAILER_BITS)
}

/// The envelope's payload: the inner u16 movementBitCount 0, so the section
/// runs to the end of the envelope, then the magic, `moves` under the usual
/// marker sequence, a `000` terminator and 8 to 15 non-zero bits that end the
/// payload on a byte boundary. After the last move 11 to 18 bits remain,
/// inside the 31 the decoder stops at without reading another marker.
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

/// Build a ComponentDataStream in the direct form: no envelope, and the u16
/// is the section's own bit count. The decoder accepts it, as the C#
/// reference does; none of the streams counted above uses it.
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
    // Terminal marker = 0 (only if we haven't hit padding)
    if !moves.is_empty() {
        movement.write_bits_u64(0, 3);
    }

    let mut payload = BitWriter::new();
    payload.write_u16(movement.bit_count() as u16); // movementBitCount
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
    array.write_int_packed(1); // updateCount = 1
    array.write_int_packed(1); // encodedIndex = 1 -> index 0
    array.write_other(update);
    array.write_int_packed(0); // array terminator
    array
}

/// Wrap a RemoteCharacterUpdates array in the RPC envelope. The framing
/// tests below deform the array itself and call this directly.
fn wrap_updates_array(array: &BitWriter) -> BitWriter {
    let mut rpc = BitWriter::new();
    rpc.write_bit(false); // first bit, consumed and discarded (C# TryReadBit(out _))
    rpc.write_int_packed(REMOTE_CHARACTER_UPDATES_HANDLE + 1);
    rpc.write_int_packed(array.bit_count());
    rpc.write_other(array);
    rpc.write_int_packed(0); // terminator
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
    // updateCount is 1, so index 2 addresses an update the array never
    // declared. The rest of the window cannot be located from there, so
    // skipping it stays the right move -- but it used to be invisible:
    // `Ok(update_count: 1, total_moves: 0, error_count: 0)` is exactly what a
    // batch of empty-but-well-formed updates returns.
    let mut array = BitWriter::new();
    array.write_int_packed(1); // updateCount = 1
    array.write_int_packed(3); // encodedIndex = 3 -> index 2, out of range
    array.write_u8(0xAA); // whatever follows is discarded

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
    // A field declaring 128 payload bits with 8 left in the array means the
    // framing no longer describes this payload. The decoder gives up on the
    // window -- correctly, since it cannot find the next handle -- but used to
    // report the give-up as a successful end-of-update, losing the second
    // declared update with it.
    let mut array = BitWriter::new();
    array.write_int_packed(2); // updateCount = 2
    array.write_int_packed(1); // encodedIndex = 1 -> index 0
    array.write_int_packed(6); // encodedHandle 6 -> handle 5, not decoded here
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
    // A 31-bit GUID field is one bit short of the u32 it must carry, so the
    // GUID stays `None` -- and the component stream that follows is then
    // consumed without being decoded, because there is no character to
    // attribute its moves to. Two separate losses, previously neither counted
    // nor visible: the update returned successfully with zero moves.
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
    // Same loss from the other ordering: handle 3 arrives before handle 2, so
    // the stream is consumed with no GUID in hand. The decoder is single-pass
    // and cannot rewind, so dropping the stream is the honest outcome --
    // reporting it as a clean zero-move update was not.
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
    // The stream is a length-delimited field, and the decoder is past all of
    // it before it reads the first bit, so a failure inside cannot misplace
    // the next handle. The C# reference records the error and seeks to the
    // field's end. This decoder gave up on the rest of the array instead, so
    // one bad stream cost every update queued behind it.
    let mut bad_magic = BitWriter::new();
    bad_magic.write_u16(0);
    bad_magic.write_u8(0x00); // not MOVEMENT_MAGIC
    let bad_magic = byte_wrapped(&bad_magic, ENVELOPE_TRAILER_BITS);
    let mut short_header = BitWriter::new();
    short_header.write_u8(0x52); // fewer than the 16 header bits
    let good = build_component_data_stream(&[build_move(true, 7, 1.0, 2.0, 3.0)]);

    for (name, bad) in [("bad magic", bad_magic), ("short header", short_header)] {
        let mut array = BitWriter::new();
        array.write_int_packed(2); // updateCount = 2
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
    // After the array terminator exactly 8 bits remain, so the decoder spends
    // them on an IntPacked. This one's continuation bit demands a sixth byte
    // that the window does not have. The read's error was dropped with
    // `let _ =`, so a payload whose tail does not parse reported success.
    let mut array = BitWriter::new();
    array.write_int_packed(0); // updateCount = 0
    array.write_int_packed(0); // encodedIndex = 0 -> array terminator
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
    // The other round trips go through the byte-wrapped envelope. This is the
    // one that pins the direct form, where the first u16 is the section's own
    // bit count.
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
    // Zero bits -> empty
    let (result, moves) = decode(&BitWriter::new());
    assert_eq!(result.total_moves, 0);
    assert!(moves.is_empty());
}

#[test]
fn invalid_magic_returns_error() {
    // A direct-form stream whose 8-bit section holds the wrong magic. The
    // stream is counted as a loss inside the update, so the RPC itself is Ok.
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

/// Wrap `section` in a direct (not byte-wrapped) component stream. `sized`
/// declares the section's own length as `movementBitCount`; otherwise the
/// count is 0, which makes the section run to the end of the stream.
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
    // After the move 43 bits remain, more than the 31 bits of padding the
    // grammar allows, so the decoder reads another marker; it is 0 and the
    // section ends with 40 bits unread. That returned Ok with nothing counted,
    // the same shape `decode_movement_rpc` already counts one layer up. A
    // tally, not an error: it must not change which batches keep raw bits.
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
    // Five bits cannot hold the 8-bit magic. The C# reference reports
    // "Missing movement magic"; this decoder returned Ok with no trace.
    let mut section = BitWriter::new();
    section.write_bits_u64(0b10110, 5);
    let (result, moves) = decode(&build_rpc_payload(77, &component_stream(&section, true)));

    assert!(moves.is_empty());
    assert_eq!(result.error_count, 0);
    assert_eq!(tails(&result), (1, 5, 0, 0));
}

#[test]
fn a_window_too_short_for_the_first_marker_is_tallied() {
    // The magic, then two bits: not enough for a 3-bit marker ("Missing first
    // movement marker" in the C# reference).
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
    // With movementBitCount 0 the section runs to the end of the component
    // stream, so what follows a zero marker may be other component data
    // rather than lost moves. Counted, but kept apart so the two readings
    // are never summed into one number.
    let stream = component_stream(&section_with_zero_marker_and_tail(1, 40), false);
    let (result, moves) = decode(&build_rpc_payload(77, &stream));

    assert_eq!(moves.len(), 1);
    assert_eq!(result.error_count, 0);
    assert_eq!(tails(&result), (0, 0, 1, 40));
}

#[test]
fn padding_after_the_last_move_and_an_empty_window_are_not_tails() {
    // The grammar's own end: at most 31 bits after a decoded move. The
    // builder's `000` terminator and the bits after it are 11 to 18 of them
    // and are never read.
    let stream = build_component_data_stream(&[
        build_move(false, 42, 1.0, 2.0, 3.0),
        build_move(true, 84, 10.0, 11.0, 12.0),
    ]);
    let (result, moves) = decode(&build_rpc_payload(9999, &stream));
    assert_eq!(moves.len(), 2);
    assert_eq!(tails(&result), (0, 0, 0, 0));

    // An open window with no bits at all leaves nothing unread. (The C#
    // reference still reports "Missing movement magic" here; this tally
    // measures unread bits, not missing fields.)
    let mut empty = BitWriter::new();
    empty.write_u16(0);
    let (result, moves) = decode(&build_rpc_payload(9999, &empty));
    assert!(moves.is_empty());
    assert_eq!(result.error_count, 0);
    assert_eq!(tails(&result), (0, 0, 0, 0));

    // A section that ends exactly on its zero marker leaves nothing either,
    // whether the marker follows a move (read as padding) or the magic (read
    // and taken as the end).
    for moves_before in [1, 0] {
        let exact = component_stream(&section_with_zero_marker_and_tail(moves_before, 0), true);
        let (result, moves) = decode(&build_rpc_payload(9999, &exact));
        assert_eq!(moves.len(), moves_before);
        assert_eq!(result.error_count, 0);
        assert_eq!(tails(&result), (0, 0, 0, 0), "{moves_before} move(s)");
    }
}

// --- QuantizedVector component widths ---------------------------------------

/// Build a QuantizedVector payload: the `SerializedInt(128)` header followed
/// by three components of `component_bits` each.
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
    // 63 is the largest value `info & 63` can produce, and every part of
    // reading it is in range: `read_bits(63)` is legal and `sign_extend`'s
    // sign bit lands at `1 << 62`. The old bound of 62 fabricated a
    // world-origin `(0, 0, 0)` here *without consuming the 189 bits the
    // header declared*, so the move still decoded, `error_count` stayed 0,
    // and every field after it came from the wrong bit offset.
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
    // Regression guard, not TDD credit: 62 already worked. It pins the
    // boundary that used to separate "read" from "fabricated" so a future
    // bound change has to break a test rather than a corpus.
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
    // Replacing the old `> 62` bound removed a *total* guard, and a
    // `debug_assert` does not restore it: `[profile.release]` in the
    // workspace manifest does not enable debug assertions, so the binary
    // that exports the corpus compiles it away. Above 64 the arithmetic
    // does not merely mis-answer, it goes out of range --
    // `mask_u64(65)` is `u64::MAX >> (64 - 65)`, an over-wide shift that
    // release masks into a nonsense mask rather than trapping.
    //
    // This is the call-site-bug shape `copy_bits_to` already treats as a
    // hard assert rather than a recoverable error, for the same reason:
    // the only caller masks the width to six bits, so reaching here means
    // a programming error and not malformed input. Run this file with
    // `-C debug-assertions=off` to see the guard actually hold.
    let data = [0xFFu8; 32];
    let mut r = BitReader::with_bit_len(&data, 256).unwrap();
    let _ = read_signed_quantized_components(&mut r, 64);
}

#[test]
fn a_truncated_63_bit_vector_reports_eof_rather_than_a_zero_vector() {
    // The other half of the same fix: refusing to fabricate means a short
    // payload has to fail, not quietly return the origin.
    let mut w = quantized(63, 0, [1, 1, 1]);
    w.bits.truncate(7 + 100);
    let bytes = w.to_bytes();
    let mut r = BitReader::with_bit_len(&bytes, u64::from(w.bit_count())).unwrap();

    assert!(matches!(
        read_quantized_vector(&mut r, 100),
        Err(MovementError::Bit(_))
    ));
}
