//! [`FieldType`] and raw bits in, [`DecodedValue`] or [`DecodeError`] out. The
//! readers live in [`scalar`] (primitives) and [`geometry`] (vectors, rotators,
//! transforms). `table.rs` is generated against `crate::decode::FieldType`, so
//! this module keeps its path.

mod geometry;
pub(crate) mod scalar;

use vrf_bitio::BitReader;

/// Every primitive type the overlay can decode. Parametric variants carry
/// their configuration inline, so the overlay table needs no side data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FieldType {
    Bool,
    Byte,
    EnumByte,
    Int32,
    UInt32,
    /// A signed 64-bit integer. `FEffectID::EffectID` is one: its
    /// `compatible_checksum` reproduces only with the C++ type `int64`, so
    /// `tools/apply_type_corrections.py` retypes the descriptors' `UInt64`.
    Int64,
    UInt64,
    Float,
    Double,
    FString,
    /// See the internal `scalar::decode_ftext` reader: a string-table key,
    /// and every other history refused.
    FText,
    /// A whole `FText` history tree ([`crate::decode_ftext_tree`]'s measured
    /// forms) as JSON. `FText` cannot stand in: it keeps only a string-table
    /// key and refuses the empty history 255, most of what a text property sends.
    FTextTree,
    FName,
    ObjectNetGuid,
    Guid,
    SerializedInt {
        max: u32,
    },
    EnumRemainingBits,
    GameplayTag,
    ByteArray {
        max_bytes: u32,
    },
    VectorFloat,
    VectorDouble,
    VectorNetQuantize {
        scale: u32,
    },
    VectorNetQuantizeNormal,
    RotationShort,
    RotationByte,
    Transform,
    /// `FRepMovement`. Both parameters are per-class choices the wire does not
    /// carry: [`crate::types::RotatorQuantization`] and
    /// [`crate::types::VectorQuantization`].
    RepMovement {
        rotation: crate::types::RotatorQuantization,
        location: crate::types::VectorQuantization,
    },
    /// Dynamic arrays and custom decoders -- not decoded, raw_bits suffices.
    Raw,
    /// Explicitly skipped fields (`.Ignore()` in descriptors).
    Skip,
}

/// The result of a successful decode. Exactly one variant is populated;
/// the caller maps it to the appropriate `value_*` column.
#[derive(Debug, Clone, PartialEq)]
pub enum DecodedValue {
    I64(i64),
    F64(f64),
    Bool(bool),
    Str(String),
}

/// Decode failure. Non-fatal: the field stays as raw_bits only.
#[derive(Debug, Clone, thiserror::Error)]
pub enum DecodeError {
    #[error("bit read error: {0}")]
    BitIo(#[from] vrf_bitio::BitError),
    #[error("not fully consumed: {remaining} bits left after decode")]
    NotFullyConsumed { remaining: u64 },
    #[error("field type is Raw/Skip -- no decode attempted")]
    RawOrSkip,
    /// A `UInt64` above `i64::MAX`. The overlay stores `i64`, so this is
    /// refused rather than sign-flipped into a plausible negative number.
    #[error("unsigned value {value} (0x{value:016X}) exceeds i64::MAX")]
    UnsignedOverflow { value: u64 },
    /// A geometry component decoded to `NaN` or an infinity; see
    /// [`crate::types::FRepMovement`]'s `Display` for why it is not coerced.
    #[error("{context} component is not finite")]
    NonFiniteComponent { context: &'static str },

    /// A `VectorNetQuantize` descriptor supplied a zero divisor.
    #[error("quantized vector scale must be non-zero, got {scale}")]
    InvalidQuantizationScale { scale: u32 },

    /// An inline FName carried a negative instance number, which Unreal does
    /// not define a display spelling for.
    #[error("FName instance number must be non-negative, got {number}")]
    InvalidFNameNumber { number: i32 },

    /// The legacy key-only FText reader rejected its post-33-bit selector.
    /// Its accepted selector 5 is a shifted view of history byte 11 plus the
    /// inline-name bit. Full history trees use `FTextTreeError` separately.
    #[error("FText history type {history_type} is not one this decoder has seen")]
    UnsupportedTextHistory { history_type: u8 },

    /// A `ByteArray`'s `IntPacked` count exceeded the table's `max_bytes`. It
    /// fires before any payload byte is read, so it is not a layout mismatch
    /// ([`Self::NotFullyConsumed`]): the fix is raising the table constant.
    #[error("byte array declared {declared} bytes, exceeding the {max} configured for this field")]
    ByteArrayLengthCapExceeded { declared: u32, max: u32 },

    /// The full-tree FText reader refused the payload; see
    /// [`crate::FTextTreeError`] for the reason.
    #[error("FText tree: {0}")]
    FTextTree(crate::FTextTreeError),
}

/// Decode raw bits according to the given [`FieldType`].
///
/// `Raw` and `Skip` return `Err(DecodeError::RawOrSkip)`; the caller leaves
/// `value_*` null. Bits left after decoding are
/// `Err(DecodeError::NotFullyConsumed)`: a layout mismatch, e.g. version drift.
pub fn decode_field(
    field_type: FieldType,
    data: &[u8],
    bit_count: u32,
) -> Result<DecodedValue, DecodeError> {
    if matches!(field_type, FieldType::Raw | FieldType::Skip) {
        return Err(DecodeError::RawOrSkip);
    }
    let mut reader = BitReader::with_bit_len(data, u64::from(bit_count))?;
    let value = dispatch_decode(field_type, &mut reader, bit_count)?;
    let remaining = reader.bits_remaining();
    // No exemption, not even for `EnumRemainingBits` (it reads at most 32
    // bits): leftover bits are an error.
    if remaining != 0 {
        return Err(DecodeError::NotFullyConsumed { remaining });
    }
    Ok(value)
}

fn dispatch_decode(
    ft: FieldType,
    r: &mut BitReader<'_>,
    bit_count: u32,
) -> Result<DecodedValue, DecodeError> {
    match ft {
        FieldType::Bool => scalar::decode_bool(r),
        FieldType::Byte | FieldType::EnumByte => scalar::decode_byte(r),
        FieldType::Int32 => scalar::decode_i32(r),
        FieldType::UInt32 => scalar::decode_u32(r),
        FieldType::Int64 => scalar::decode_i64(r),
        FieldType::UInt64 => scalar::decode_u64(r),
        FieldType::Float => scalar::decode_float(r),
        FieldType::Double => scalar::decode_double(r),
        FieldType::FString => scalar::decode_fstring(r),
        FieldType::FText => scalar::decode_ftext(r),
        FieldType::FTextTree => crate::ftext::decode_ftext_tree_from(r)
            .map(|tree| DecodedValue::Str(tree.to_json()))
            .map_err(DecodeError::FTextTree),
        FieldType::FName => scalar::decode_fname(r),
        FieldType::ObjectNetGuid | FieldType::GameplayTag => scalar::decode_int_packed(r),
        FieldType::Guid => scalar::decode_guid(r),
        FieldType::SerializedInt { max } => scalar::decode_serialized_int(r, max),
        FieldType::EnumRemainingBits => scalar::decode_enum_remaining_bits(r, bit_count),
        FieldType::ByteArray { max_bytes } => scalar::decode_byte_array(r, max_bytes),
        FieldType::VectorFloat => Ok(render(geometry::read_float_vector(r)?)),
        FieldType::VectorDouble => Ok(render(geometry::read_double_vector(r)?)),
        FieldType::VectorNetQuantize { scale } => geometry::decode_vector_net_quantize(r, scale),
        FieldType::VectorNetQuantizeNormal => Ok(render(geometry::read_fixed_vector_normal(r)?)),
        FieldType::RotationShort => Ok(render(geometry::read_rotation(r, 16)?)),
        FieldType::RotationByte => Ok(render(geometry::read_rotation(r, 8)?)),
        FieldType::Transform => Ok(render(geometry::read_transform(r)?)),
        FieldType::RepMovement { rotation, location } => {
            Ok(render(geometry::read_rep_movement(r, rotation, location)?))
        }
        FieldType::Raw | FieldType::Skip => Err(DecodeError::RawOrSkip),
    }
}

/// Render a `Display` value into [`DecodedValue::Str`], deliberately with
/// plain `to_string()`: pre-reserving 32 bytes (vector) or 256
/// (`ReplicatedMovement`) measured no faster end to end and raised peak RSS
/// by ~3 MB, dead space in the row buffer for every short value like `(0,0,0)`.
fn render(value: impl core::fmt::Display) -> DecodedValue {
    DecodedValue::Str(value.to_string())
}
