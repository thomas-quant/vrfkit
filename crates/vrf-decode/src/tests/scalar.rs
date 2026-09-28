//! Scalar primitive decoders, ported from the C# reference's
//! `PrimitiveDecodersScalarTests.cs`.

use crate::decode::{DecodeError, DecodedValue, FieldType, decode_field};
use crate::test_bits::BitWriter;

fn str_value(s: &str) -> DecodedValue {
    DecodedValue::Str(s.to_owned())
}

#[test]
fn double_reads_eight_byte_float() {
    let data = 123.5_f64.to_bits().to_le_bytes();
    let result = decode_field(FieldType::Double, &data, 64).unwrap();
    assert_eq!(result, DecodedValue::F64(123.5));
}

#[test]
fn fstring_reads_unreal_string() {
    // Length = 6 (5 chars + null)
    let mut data = 6i32.to_le_bytes().to_vec();
    data.extend_from_slice(b"Spike\0");
    let result = decode_field(FieldType::FString, &data, data.len() as u32 * 8).unwrap();
    assert_eq!(result, str_value("Spike"));
}

#[test]
fn fname_hardcoded_reads_a_packed_index() {
    // FArchive.ReadFNameCore: when the leading bit is set the name is a
    // hardcoded table index sent as IntPacked, and the reference renders
    // it as the decimal index -- there is no FString to read.
    //
    // Ignoring that branch is why MulticastNotifyDamage_Point.DamagedBone
    // had to be forced to Raw: 177 of its 581 payloads on 02d4d478 are
    // 9 bits (1 flag + one IntPacked byte), far too short for the
    // FString path, which read past the end and produced mojibake.
    //
    // 9-bit payload: bit0 = 1 (hardcoded), then IntPacked 0 = byte 0x00.
    let result = decode_field(FieldType::FName, &[0x01, 0x00], 9).unwrap();
    assert_eq!(result, str_value("0"));
}

/// Build the inline (`isHardcoded = 0`) FName shape: a leading zero bit, then
/// an FString, then the i32 instance number.
fn inline_fname_bits(name: &str, number: i32) -> (Vec<u8>, u32) {
    let mut bits = BitWriter::new();
    bits.bits(0, 1).i32(i32::try_from(name.len() + 1).unwrap());
    for byte in name.bytes().chain([0]) {
        bits.bits(u64::from(byte), 8);
    }
    bits.i32(number).finish()
}

#[test]
fn fname_reads_inline_name() {
    // FString "Bomb" (len=5 including null) + i32 suffix = 0
    let (bits, bit_count) = inline_fname_bits("Bomb", 0);
    let result = decode_field(FieldType::FName, &bits, bit_count).unwrap();
    assert_eq!(result, str_value("Bomb"));
}

/// The FName instance number is part of the name's identity, so two fields that
/// differ only in it must not decode to the same string.
///
/// Unreal stores the number as `displayed suffix + 1`: 0 means the bare name,
/// and N != 0 displays as `Name_{N-1}`. Discarding it made `Source_1` and
/// `Source_2` both read as `Source`.
#[test]
fn fname_inline_numbers_do_not_collide() {
    for (number, want) in [(1, "Source_0"), (2, "Source_1")] {
        let (bits, bit_count) = inline_fname_bits("Source", number);
        let result = decode_field(FieldType::FName, &bits, bit_count).unwrap();
        assert_eq!(result, str_value(want), "number {number}");
    }
}

#[test]
fn fname_negative_instance_numbers_are_rejected() {
    for number in [-1, i32::MIN] {
        let (bits, bit_count) = inline_fname_bits("Source", number);
        let err = decode_field(FieldType::FName, &bits, bit_count)
            .expect_err("a negative FName number has no valid display spelling");
        assert!(
            matches!(err, DecodeError::InvalidFNameNumber { number: n } if n == number),
            "got {err:?} for {number}"
        );
    }
}

#[test]
fn byte_array_reads_packed_count_and_bytes() {
    // IntPacked 3 = byte (3 << 1) = 0x06
    let data = [0x06u8, 0x10, 0x20, 0x30];
    let result = decode_field(FieldType::ByteArray { max_bytes: 8 }, &data, 32).unwrap();
    assert_eq!(result, str_value("102030"));
}

/// A declared count over the table's `max_bytes` is refused with a dedicated
/// variant, not `NotFullyConsumed` -- no payload byte has been read yet at
/// this point, so a "bits left over after decode" label would be meaningless.
#[test]
fn byte_array_over_the_length_cap_is_refused_distinctly() {
    // IntPacked 10 = byte (10 << 1) = 0x14. No payload bytes needed: the
    // count alone must be enough to refuse before any are read.
    let err = decode_field(FieldType::ByteArray { max_bytes: 8 }, &[0x14], 8)
        .expect_err("a declared count over max_bytes must be refused");
    assert!(
        matches!(
            err,
            DecodeError::ByteArrayLengthCapExceeded {
                declared: 10,
                max: 8
            }
        ),
        "got {err:?}"
    );
}

#[test]
fn guid_reads_four_le_words() {
    let data: Vec<u8> = [0x00112233u32, 0x44556677, 0x8899AABB, 0xCCDDEEFF]
        .iter()
        .flat_map(|word| word.to_le_bytes())
        .collect();
    let result = decode_field(FieldType::Guid, &data, 128).unwrap();
    assert_eq!(result, str_value("00112233-4455-6677-8899-aabbccddeeff"));
}

#[test]
fn serialized_int_reads_value_using_known_maximum() {
    // max=16 -> value_bits = 4. value 5 = 0b0101 in 4 bits.
    let result = decode_field(FieldType::SerializedInt { max: 16 }, &[0x05], 4).unwrap();
    assert_eq!(result, DecodedValue::I64(5));
}

/// A `UInt64` with its sign bit set cannot fit in the `i64` the overlay stores
/// without a silent wrap to a negative number. It must be rejected loudly
/// instead. Values at or below `i64::MAX` decode exactly as before.
#[test]
fn uint64_above_i64_max_is_rejected_not_wrapped() {
    // High bit set: i64::MAX + 1 = 0x8000_0000_0000_0000.
    let over = i64::MAX as u64 + 1;
    let result = decode_field(FieldType::UInt64, &over.to_le_bytes(), 64);
    assert!(matches!(
        result,
        Err(DecodeError::UnsignedOverflow { value }) if value == over
    ));
    // In range, up to the boundary i64::MAX itself: the positive I64.
    for value in [0x0102030405060708u64, i64::MAX as u64] {
        let result = decode_field(FieldType::UInt64, &value.to_le_bytes(), 64).unwrap();
        assert_eq!(result, DecodedValue::I64(value as i64));
    }
}

/// `Int64` reads the same 64 bits as `UInt64` but as two's complement: the
/// pattern `UInt64` refuses is a negative number here, not an error, and a
/// value below `i64::MAX` is the same either way -- which is why retyping the
/// effect IDs changes no exported value on data that never sets bit 63.
/// Anything but exactly 64 bits is refused like every fixed-width read.
#[test]
fn int64_reads_eight_byte_twos_complement() {
    for value in [0x0102030405060708i64, -2, i64::MIN] {
        let result = decode_field(FieldType::Int64, &value.to_le_bytes(), 64).unwrap();
        assert_eq!(result, DecodedValue::I64(value));
    }
    assert!(matches!(
        decode_field(FieldType::UInt64, &i64::MIN.to_le_bytes(), 64),
        Err(DecodeError::UnsignedOverflow { .. })
    ));
    let data = [0u8; 9];
    assert!(decode_field(FieldType::Int64, &data, 72).is_err());
    assert!(decode_field(FieldType::Int64, &data, 32).is_err());
}

/// `EnumRemainingBits` reads the whole payload, up to and including 32 bits.
///
/// A payload too wide for the type must not come back as its low 32 bits.
///
/// `decode_enum_remaining_bits` read `min(bits_left, 32)` and returned, and
/// `decode_field` exempted this one type from the not-fully-consumed check --
/// so the bits above 32 were dropped without reaching any counter, any error,
/// or the `skipped_bits` tally. The C# reference throws here. This follows
/// `UnsignedOverflow`'s rule instead: a value that cannot be represented is an
/// error, not a plausible wrong number.
///
/// Latent on this corpus -- handles 215/216 reach 47 at most across 71
/// replays, so nothing triggers it today. That is exactly why it needs a test.
#[test]
fn enum_remaining_bits_reads_the_payload_and_refuses_over_32() {
    // 3 bits = value 3 (low 3 bits of 0b011)
    let result = decode_field(FieldType::EnumRemainingBits, &[0b0000_0011], 3).unwrap();
    assert_eq!(result, DecodedValue::I64(3));
    // The boundary still decodes: 32 bits is representable, 33 is not.
    let result = decode_field(FieldType::EnumRemainingBits, &[0xFF; 4], 32).unwrap();
    assert_eq!(result, DecodedValue::I64(u32::MAX as i64));
    let err = decode_field(FieldType::EnumRemainingBits, &[0xFF; 5], 40).unwrap_err();
    assert!(
        matches!(err, DecodeError::NotFullyConsumed { remaining: 8 }),
        "expected the leftover to be reported, got {err:?}"
    );
}

#[test]
fn gameplay_tag_reads_packed_index() {
    // IntPacked 252: 252 = 0b11111100, split: chunk0=252&0x7F=124, chunk1=252>>7=1
    // byte0 = (124 << 1) | 1 = 249, byte1 = (1 << 1) | 0 = 2
    let result = decode_field(FieldType::GameplayTag, &[249, 2], 16).unwrap();
    assert_eq!(result, DecodedValue::I64(252));
}

#[test]
fn bool_reads_single_bit() {
    for (byte, want) in [(0x01u8, true), (0x00, false)] {
        let result = decode_field(FieldType::Bool, &[byte], 1).unwrap();
        assert_eq!(result, DecodedValue::Bool(want));
    }
}

#[test]
fn int32_reads_signed() {
    let result = decode_field(FieldType::Int32, &(-42i32).to_le_bytes(), 32).unwrap();
    assert_eq!(result, DecodedValue::I64(-42));
}

#[test]
fn float_reads_ieee754_single() {
    let result = decode_field(FieldType::Float, &1.25f32.to_le_bytes(), 32).unwrap();
    assert_eq!(result, DecodedValue::F64(1.25));
}

#[test]
fn object_net_guid_reads_int_packed() {
    // IntPacked value 0x3F: byte = (0x3F << 1) | 0 = 0x7E
    let result = decode_field(FieldType::ObjectNetGuid, &[0x7E], 8).unwrap();
    assert_eq!(result, DecodedValue::I64(0x3F));
}

/// `FText` decodes to the statistic's name, which nothing else in the export
/// carries.
///
/// `LocalizedStat` was typed `FString` once and produced null on every row,
/// because the wire is an `FText`. It was left untyped on the reasoning that
/// the sibling `Statistic` enum already said the same thing -- but `Statistic`
/// decodes to a bare integer and this repository ships no table mapping those
/// integers to names. It has them only in a comment. So this is in fact the
/// only machine-readable source of `EnemiesBlinded` and the other 28.
///
/// The layout, confirmed on 4,341 of 4,341 rows with zero residual bits: 41
/// header bits ending in a history-type discriminator of 5, then the string
/// table's asset path as an `FString`, then that `FName`'s numeric suffix,
/// then the key. The key is the statistic name.
#[test]
fn ftext_decodes_a_string_table_entry_to_its_key() {
    let vectors: [(u32, &[u8], &str); 3] = [
        // Kills
        (
            785u32,
            &[
                0x00, 0x00, 0x00, 0x00, 0x0B, 0x96, 0x00, 0x00, 0x00, 0x5E, 0x8E, 0xC2, 0xDA, 0xCA,
                0x5E, 0xAA, 0x92, 0x5E, 0x92, 0xDC, 0x8E, 0xC2, 0xDA, 0xCA, 0x5E, 0x86, 0xDE, 0xDA,
                0xC4, 0xC2, 0xE8, 0xA4, 0xCA, 0xE0, 0xDE, 0xE4, 0xE8, 0x5E, 0x82, 0xC4, 0xD2, 0xD8,
                0xD2, 0xE8, 0xF2, 0x8A, 0xCC, 0xCC, 0xCA, 0xC6, 0xE8, 0xE6, 0xBE, 0xA6, 0xE8, 0xE4,
                0xD2, 0xDC, 0xCE, 0xE6, 0x5C, 0x82, 0xC4, 0xD2, 0xD8, 0xD2, 0xE8, 0xF2, 0x8A, 0xCC,
                0xCC, 0xCA, 0xC6, 0xE8, 0xE6, 0xBE, 0xA6, 0xE8, 0xE4, 0xD2, 0xDC, 0xCE, 0xE6, 0x00,
                0x00, 0x00, 0x00, 0x00, 0x0C, 0x00, 0x00, 0x00, 0x96, 0xD2, 0xD8, 0xD8, 0xE6, 0x00,
                0x00,
            ][..],
            "Kills",
        ),
        // EnemiesBlinded
        (
            969u32,
            &[
                0x00, 0x00, 0x00, 0x00, 0x0B, 0xB2, 0x00, 0x00, 0x00, 0x5E, 0x8E, 0xC2, 0xDA, 0xCA,
                0x5E, 0x86, 0xD0, 0xC2, 0xE4, 0xC2, 0xC6, 0xE8, 0xCA, 0xE4, 0xE6, 0x5E, 0x8E, 0xD8,
                0xDE, 0xC4, 0xC2, 0xD8, 0x5E, 0xA6, 0xE8, 0xE4, 0xD2, 0xDC, 0xCE, 0xA8, 0xC2, 0xC4,
                0xD8, 0xCA, 0xE6, 0x5E, 0x86, 0xD0, 0xC2, 0xE4, 0xC2, 0xC6, 0xE8, 0xCA, 0xE4, 0xE6,
                0xBE, 0x8E, 0xD8, 0xDE, 0xC4, 0xC2, 0xD8, 0xBE, 0xA6, 0xE8, 0xE4, 0xD2, 0xDC, 0xCE,
                0xE6, 0x5C, 0x86, 0xD0, 0xC2, 0xE4, 0xC2, 0xC6, 0xE8, 0xCA, 0xE4, 0xE6, 0xBE, 0x8E,
                0xD8, 0xDE, 0xC4, 0xC2, 0xD8, 0xBE, 0xA6, 0xE8, 0xE4, 0xD2, 0xDC, 0xCE, 0xE6, 0x00,
                0x00, 0x00, 0x00, 0x00, 0x1E, 0x00, 0x00, 0x00, 0x8A, 0xDC, 0xCA, 0xDA, 0xD2, 0xCA,
                0xE6, 0x84, 0xD8, 0xD2, 0xDC, 0xC8, 0xCA, 0xC8, 0x00, 0x00,
            ][..],
            "EnemiesBlinded",
        ),
        // AbilityStat_ShotsFired
        (
            1033u32,
            &[
                0x00, 0x00, 0x00, 0x00, 0x0B, 0xB2, 0x00, 0x00, 0x00, 0x5E, 0x8E, 0xC2, 0xDA, 0xCA,
                0x5E, 0x86, 0xD0, 0xC2, 0xE4, 0xC2, 0xC6, 0xE8, 0xCA, 0xE4, 0xE6, 0x5E, 0x8E, 0xD8,
                0xDE, 0xC4, 0xC2, 0xD8, 0x5E, 0xA6, 0xE8, 0xE4, 0xD2, 0xDC, 0xCE, 0xA8, 0xC2, 0xC4,
                0xD8, 0xCA, 0xE6, 0x5E, 0x86, 0xD0, 0xC2, 0xE4, 0xC2, 0xC6, 0xE8, 0xCA, 0xE4, 0xE6,
                0xBE, 0x8E, 0xD8, 0xDE, 0xC4, 0xC2, 0xD8, 0xBE, 0xA6, 0xE8, 0xE4, 0xD2, 0xDC, 0xCE,
                0xE6, 0x5C, 0x86, 0xD0, 0xC2, 0xE4, 0xC2, 0xC6, 0xE8, 0xCA, 0xE4, 0xE6, 0xBE, 0x8E,
                0xD8, 0xDE, 0xC4, 0xC2, 0xD8, 0xBE, 0xA6, 0xE8, 0xE4, 0xD2, 0xDC, 0xCE, 0xE6, 0x00,
                0x00, 0x00, 0x00, 0x00, 0x2E, 0x00, 0x00, 0x00, 0x82, 0xC4, 0xD2, 0xD8, 0xD2, 0xE8,
                0xF2, 0xA6, 0xE8, 0xC2, 0xE8, 0xBE, 0xA6, 0xD0, 0xDE, 0xE8, 0xE6, 0x8C, 0xD2, 0xE4,
                0xCA, 0xC8, 0x00, 0x00,
            ][..],
            "AbilityStat_ShotsFired",
        ),
    ];
    for (bit_count, raw, expected) in vectors {
        assert_eq!(
            decode_field(FieldType::FText, raw, bit_count).unwrap(),
            str_value(expected),
            "{expected}"
        );
    }
}

/// A history type the layout was never observed under is refused, not guessed.
///
/// Every sample carries 5. Another value means a different `ETextHistory`
/// variant with a different payload after the header, and reading it as this
/// one would produce a plausible wrong string -- the failure this type was
/// removed for in the first place.
#[test]
fn ftext_refuses_an_unobserved_history_type() {
    // Zeroed except the history-type discriminator, so if the guard were
    // removed the rest of the layout would decode cleanly (empty table path,
    // number 0, empty key) rather than erroring out on a short buffer -- the
    // guard is the only thing standing between this input and `Ok`.
    let mut raw = vec![0u8; 18];
    raw[4] = 0x0C; // shifts a history type of 6 into place, not 5
    let err = decode_field(FieldType::FText, &raw, 137).unwrap_err();
    assert!(
        matches!(err, DecodeError::UnsupportedTextHistory { history_type: 6 }),
        "got {err:?}"
    );
}
