//! Scalar field readers: everything Unreal writes as a single primitive.
//! `decode_field` checks that the payload was fully consumed, so none of
//! these needs to.

use vrf_bitio::{BitError, BitReader};

use super::{DecodeError, DecodedValue};

pub(super) fn decode_bool(r: &mut BitReader<'_>) -> Result<DecodedValue, DecodeError> {
    Ok(DecodedValue::Bool(r.read_bit()?))
}

/// A byte-width enum or `uint8`, as wide as the payload rather than a fixed 8
/// bits: Unreal writes only the significant bits of byte properties nested in
/// replicated arrays. `CombatReport` `AssistType` is 5 bits; a fixed 8-bit
/// read left all 364 of its rows untyped. Wider than 8 bits is refused, not
/// truncated: the field is not
/// byte-sized, and its low byte would be a plausible wrong number.
pub(super) fn decode_byte(r: &mut BitReader<'_>) -> Result<DecodedValue, DecodeError> {
    let width = r.bits_remaining();
    if width == 0 || width > 8 {
        // Fall back to the nominal width so the error names a concrete read.
        return Ok(DecodedValue::I64(i64::from(r.read_u8()?)));
    }
    let raw = r.read_bits(width as u32)?;
    Ok(DecodedValue::I64(raw as i64))
}

pub(super) fn decode_i32(r: &mut BitReader<'_>) -> Result<DecodedValue, DecodeError> {
    Ok(DecodedValue::I64(i64::from(r.read_i32()?)))
}

pub(super) fn decode_u32(r: &mut BitReader<'_>) -> Result<DecodedValue, DecodeError> {
    Ok(DecodedValue::I64(i64::from(r.read_u32()?)))
}

/// A little-endian two's-complement 64-bit integer. Every bit pattern is a
/// value, so unlike [`decode_u64`] there is nothing to refuse.
pub(super) fn decode_i64(r: &mut BitReader<'_>) -> Result<DecodedValue, DecodeError> {
    Ok(DecodedValue::I64(r.read_u64()? as i64))
}

pub(super) fn decode_u64(r: &mut BitReader<'_>) -> Result<DecodedValue, DecodeError> {
    // Refused past i64::MAX (`UnsignedOverflow`). No shipped entry reads
    // UInt64 since the effect IDs became Int64 (`FieldType::Int64`); this
    // stays a defensive loud failure.
    let value = r.read_u64()?;
    if value > i64::MAX as u64 {
        return Err(DecodeError::UnsignedOverflow { value });
    }
    Ok(DecodedValue::I64(value as i64))
}

pub(super) fn decode_float(r: &mut BitReader<'_>) -> Result<DecodedValue, DecodeError> {
    Ok(DecodedValue::F64(f64::from(r.read_f32()?)))
}

pub(super) fn decode_double(r: &mut BitReader<'_>) -> Result<DecodedValue, DecodeError> {
    Ok(DecodedValue::F64(r.read_f64()?))
}

pub(super) fn decode_fstring(r: &mut BitReader<'_>) -> Result<DecodedValue, DecodeError> {
    Ok(DecodedValue::Str(r.read_fstring(64 * 1024)?))
}

/// FName: an `isHardcoded` bit, then either one IntPacked index into the engine's
/// hardcoded name table (rendered as its decimal) or an inline FString plus an
/// i32 instance number (0 renders the bare name, `N` renders `Name_{N-1}`; see
/// docs/DATA.md "FName instance numbers are part of the name"). Replays do send
/// the hardcoded shape: 177 of the 581 `DamagedBone` payloads on 02d4d478 are
/// 9 bits (flag + one IntPacked byte), which an always-inline read turned into
/// mojibake.
pub(super) fn decode_fname(r: &mut BitReader<'_>) -> Result<DecodedValue, DecodeError> {
    read_fname(r, 64 * 1024).map(DecodedValue::Str)
}

/// [`decode_fname`]'s reader, generic over the error type so the struct blobs
/// keep their `StructBlobError` variants. `max_bytes` caps the inline string
/// (64 KiB here, 1024 there); one reader spells a name the same in both.
pub(crate) fn read_fname<E: From<BitError> + From<DecodeError>>(
    r: &mut BitReader<'_>,
    max_bytes: i64,
) -> Result<String, E> {
    if r.read_bit()? {
        return Ok(r.read_int_packed()?.to_string());
    }
    let name = r.read_fstring(max_bytes)?;
    let number = r.read_i32()?;
    Ok(render_fname(name, number)?)
}

/// Apply an `FName`'s instance number to its string, Unreal's way.
///
/// `number == 0` is the bare name; otherwise the displayed suffix is
/// `number - 1`. A negative number has no valid display form and is rejected;
/// in particular, `i32::MIN - 1` must not wrap into a plausible positive name.
fn render_fname(name: String, number: i32) -> Result<String, DecodeError> {
    if number < 0 {
        return Err(DecodeError::InvalidFNameNumber { number });
    }
    match number {
        0 => Ok(name),
        n => Ok(format!("{name}_{}", n - 1)),
    }
}

/// Both an object NetGUID and a gameplay tag are wire IntPacked values;
/// only the declared type says which wire concept they are.
pub(super) fn decode_int_packed(r: &mut BitReader<'_>) -> Result<DecodedValue, DecodeError> {
    Ok(DecodedValue::I64(i64::from(r.read_int_packed()?)))
}

/// 128-bit GUID: 4 x u32 LE -> formatted as standard hex GUID.
pub(super) fn decode_guid(r: &mut BitReader<'_>) -> Result<DecodedValue, DecodeError> {
    use core::fmt::Write as _;
    let a = r.read_u32()?;
    let b = r.read_u32()?;
    let c = r.read_u32()?;
    let d = r.read_u32()?;
    // 8 + 4 + 4 + 4 + 12 digits plus four dashes: always exactly 36 bytes.
    let mut s = String::with_capacity(36);
    let _ = write!(
        s,
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        a,
        (b >> 16) & 0xFFFF,
        b & 0xFFFF,
        (c >> 16) & 0xFFFF,
        (u64::from(c & 0xFFFF) << 32) | u64::from(d)
    );
    Ok(DecodedValue::Str(s))
}

pub(super) fn decode_serialized_int(
    r: &mut BitReader<'_>,
    max: u32,
) -> Result<DecodedValue, DecodeError> {
    Ok(DecodedValue::I64(i64::from(r.read_serialized_int(max)?)))
}

/// A byte enum as wide as its payload (at most 32 bits are read).
///
/// A zero-width payload is `I64(0)`, not a fabricated value: Unreal writes
/// `ceil(log2(variant_count))` bits, so zero bits is the complete encoding of a
/// one-variant enum -- unlike a `NaN` or an over-wide payload, whose bits say
/// something the type cannot hold. The overlay counts it `decoded_ok`; the
/// 215-replay export reports zero decode errors under that rule.
pub(super) fn decode_enum_remaining_bits(
    r: &mut BitReader<'_>,
    bit_count: u32,
) -> Result<DecodedValue, DecodeError> {
    // `dispatch_decode` builds the reader from exactly `bit_count` bits, so
    // this is `r.bits_remaining() == 0`.
    if bit_count == 0 {
        return Ok(DecodedValue::I64(0));
    }
    let bits_left = r.bits_remaining();
    let to_read = bits_left.min(32);
    Ok(DecodedValue::I64(i64::from(
        r.read_bits(to_read as u32)? as u32
    )))
}

/// Lowercase hex digits, indexed by nibble.
const HEX_DIGITS: [u8; 16] = *b"0123456789abcdef";

/// Length-prefixed byte blob, hex-encoded into `value_str` as it is read (no
/// per-byte `format!`). The table's cap is checked first and keeps its own
/// variant; then a count the payload cannot hold is refused before a byte is
/// read, as `read_fstring` refuses a string's: the prefix is at fault, so it is
/// `InvalidLength` (`Malformed` in the error report), not `Eof`.
pub(super) fn decode_byte_array(
    r: &mut BitReader<'_>,
    max_bytes: u32,
) -> Result<DecodedValue, DecodeError> {
    let start = r.position();
    let count = r.read_int_packed()?;
    if count > max_bytes {
        return Err(DecodeError::ByteArrayLengthCapExceeded {
            declared: count,
            max: max_bytes,
        });
    }
    if u64::from(count) * 8 > r.bits_remaining() {
        return Err(DecodeError::BitIo(BitError::InvalidLength {
            position: start,
            length: i64::from(count),
        }));
    }
    let mut hex = String::with_capacity(count as usize * 2);
    for _ in 0..count {
        let byte = r.read_u8()?;
        hex.push(HEX_DIGITS[(byte >> 4) as usize] as char);
        hex.push(HEX_DIGITS[(byte & 0x0F) as usize] as char);
    }
    Ok(DecodedValue::Str(hex))
}
