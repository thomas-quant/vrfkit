//! Scalar field readers: everything Unreal writes as a single primitive.
//!
//! Each function consumes exactly the field's payload and returns the value in
//! the slot the overlay writes it to. `decode_field` is what checks that the
//! payload was fully consumed, so nothing here needs to.

use vrf_bitio::{BitError, BitReader};

use super::{DecodeError, DecodedValue};

pub(super) fn decode_bool(r: &mut BitReader<'_>) -> Result<DecodedValue, DecodeError> {
    Ok(DecodedValue::Bool(r.read_bit()?))
}

/// Decode a byte-width enum or `uint8` property.
///
/// The width is taken from what the field payload actually carries rather than
/// fixed at 8 bits. Unreal writes only the significant bits for byte-sized
/// properties nested inside replicated arrays, so a 5-bit payload is normal and a
/// hard 8-bit read fails on it. The reference implementation does the same thing:
///
/// ```csharp
/// private static byte ReadByte(FBitArchive archive) =>
///     checked((byte)archive.ReadBitsToUInt64(checked((int)archive.BitsRemaining)));
/// ```
///
/// Concretely this is what makes `CombatReport` `AssistType` (5 bits) decode; a
/// fixed-width read left all 364 of its rows untyped.
///
/// Payloads wider than 8 bits are rejected rather than truncated: that means the
/// field is not really byte-sized and silently keeping the low byte would emit a
/// plausible wrong number.
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
    // The overlay stores integers as i64. A u64 with its high bit set cannot
    // be represented without a silent sign flip, so reject it loudly rather
    // than emit a plausible wrong (negative) number. The C# descriptors
    // declare the effect IDs UInt64, but the property is an `int64`
    // (FEffectID::EffectID; its compatible_checksum reproduces only with that
    // type), so apply_type_corrections.py retypes them Int64 and no shipped
    // entry reads UInt64 any more. This stays a defensive loud failure.
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

/// Legacy FText string-table reader, returning only the key for existing
/// `FieldType::FText` consumers such as `LocalizedStat`.
///
/// Its historical 33+8 split yields selector 5. The measured full layout is
/// 32 flag bits, history byte 11, then one inline-FName selector bit followed
/// by FString table name, i32 instance number, and FString key. Therefore 5
/// is a shifted selector, not the history enum value. Keeping this reader
/// preserves the existing key-string output and error behavior.
///
/// [`crate::decode_ftext_tree`] separately decodes strict complete trees,
/// including the measured formatted reward histories and their arguments.
pub(super) fn decode_ftext(r: &mut BitReader<'_>) -> Result<DecodedValue, DecodeError> {
    r.read_bits(33)?;
    let history_type = r.read_bits(8)? as u8;
    if history_type != 5 {
        return Err(DecodeError::UnsupportedTextHistory { history_type });
    }
    let _table_path = r.read_fstring(64 * 1024)?;
    let _number = r.read_bits(32)?;
    Ok(DecodedValue::Str(r.read_fstring(64 * 1024)?))
}

/// FName on the wire: 1 bit `isHardcoded`, then one of two shapes.
///
/// Mirrors `FArchive.ReadFNameCore` in the reference. When the bit is set the
/// name is an index into the engine's hardcoded name table, sent as a single
/// IntPacked and rendered as its decimal value; there is no string. When it is
/// clear the name is inline: FString plus an i32 instance number, where 0
/// renders the bare name and `N != 0` renders `Name_{N-1}`. See docs/DATA.md
/// "FName instance numbers are part of the name" for the corpus-measured
/// collapse dropping that number used to cause.
///
/// The comment here used to assert "isHardcoded=false for replays" and the
/// code read the bit and discarded it, always taking the inline path. That is
/// false: 177 of the 581 `DamagedBone` payloads on 02d4d478 are 9 bits, which
/// is exactly the hardcoded shape (1 flag + one IntPacked byte). Reading them
/// as an FString ran off the end of the payload and produced mojibake, which
/// is why the field had to be forced to Raw in the type-correction pass.
pub(super) fn decode_fname(r: &mut BitReader<'_>) -> Result<DecodedValue, DecodeError> {
    read_fname(r, 64 * 1024).map(DecodedValue::Str)
}

/// [`decode_fname`]'s reader, generic over the caller's error type so the
/// struct blobs keep their own (`StructBlobError::BitIo` for a read,
/// `StructBlobError::Decode` for a bad instance number). `max_bytes` caps the
/// inline string: 64 KiB here, 1024 in the struct blobs. One reader means a
/// `WinningTeam` read there and an `FName` read here spell a name the same.
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

/// A byte enum that occupies whatever width the field payload carries.
///
/// # A zero-width payload really is zero, and is not fabricated
///
/// The zero-width arms return `I64(0)` without reading a bit, which looks like
/// a value invented out of nothing. It is not, and the reason is worth writing
/// down because the surrounding code refuses exactly this shape elsewhere
/// (`UnsignedOverflow`, `UnsupportedTextHistory`, and the no-exemption rule in
/// [`super::decode_field`]).
///
/// Unreal serialises this enum in `ceil(log2(variant_count))` bits, and the
/// number of bits needed to distinguish ONE possible variant is zero. A
/// zero-width payload is therefore not a truncated value: it is the complete
/// encoding of an enum whose only variant is the first, and reading it as 0 is
/// the encoding's own answer, not a guess at a missing one. That is different
/// in kind from a `NaN` component or an over-wide payload, where the bits are
/// present and say something the type cannot represent.
///
/// The corpus agrees: `overlay::apply_overlay_inner` already answers a zero-bit
/// `EnumRemainingBits` with `value_i64 = 0` and counts it `decoded_ok`, and the
/// 215-replay export reports zero decode errors under that behaviour. Turning
/// zero-width into an error or a null would move those rows to failures for a
/// value the format defines.
pub(super) fn decode_enum_remaining_bits(
    r: &mut BitReader<'_>,
    bit_count: u32,
) -> Result<DecodedValue, DecodeError> {
    // The single call site (`dispatch_decode`) hands us a reader freshly built
    // from exactly `bit_count` bits, before anything has been read from it, so
    // `bit_count == 0` and `r.bits_remaining() == 0` are the same condition
    // here. Testing `bit_count` once covers both.
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

/// Length-prefixed byte blob, rendered as lowercase hex into `value_str`.
///
/// The bytes are hex-encoded as they are read rather than collected first: the
/// previous shape ran `format!("{b:02x}")` per byte, which is one heap
/// allocation and one formatting machine per byte of payload.
///
/// A count the payload cannot hold is refused before a byte is read, the way
/// `read_fstring` refuses a string's: the prefix is what is wrong, so it is
/// `InvalidLength` and the error report prints `Malformed`. The byte loop
/// used to find out by running into `Eof`, which printed `EOF` for the cause
/// an FString's prefix prints as `Malformed`. The table's cap is checked
/// first and keeps its own variant.
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
