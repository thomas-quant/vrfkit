//! Decoder for the shot-effect RepLayoutDynamicArray blobs: the RPC
//! parameters `FloatValues` (`EffectDataFloat`: ammo, projectile count, random
//! seed, tracer option, burst, yaw switch), `ObjectValues` (`EffectDataObject`:
//! firing player state, firing state, equippable) and `VectorValues`
//! (`EffectDataVector`: one attack direction per projectile).
//!
//! # Where this runs
//!
//! vrfkit's `sink/rpc.rs` calls [`decode_effect_blob_json`] for a parameter
//! with one of those names when the overlay produced no value, and puts the
//! JSON into `value_str` **in addition to** `raw_bits`. Like the type overlay
//! it is additive: a failure leaves `value_str` null, keeps the bits and
//! counts. The shot RPC `ReplayPlayContinuousEffectAtLocation` takes this path
//! too; `tools/to_valplay_bundle.py` still reads its shot inputs from the raw
//! bits, so the added JSON changes nothing it consumes.
//!
//! Where the Python port can return partial elements on malformed input, this
//! decoder rejects the whole array (underfilled member windows, missing or
//! nonzero terminators, residual bits). A 2026-09-08 differential over 91,827
//! real shot-array blobs from eight exports (two each from 13.01, 13.02, 13.04
//! and 13.05) matched every structure, tag and numeric bit pattern, with zero
//! Rust errors; that scope does not cover malformed input or future builds.
//! The pinned tests and `tools/check_effect_decoder.py` (the Python port) are
//! fixture checks and no substitute for such a differential.
//!
//! # Wire layout (C# reference, corpus-validated)
//!
//! ```text
//! [IntPacked: element_count]
//! repeat {
//!     [IntPacked: encoded_index]       // 0 = terminator, else index = encoded - 1
//!     repeat {                         // per element, until handle 0
//!         [IntPacked: encoded_handle]  // 0 = end, else handle = encoded - 1
//!         [IntPacked: payload_bits]
//!         [bits: payload]
//!     }
//! }
//! ```
//!
//! An element is a gameplay-tag index (IntPacked) and a value: an f32, an
//! IntPacked net GUID or three f64s, under a handle pair that depends on the
//! function ([`EffectHandles`](crate::effect::EffectHandles)). Checked against
//! the C# parser's `events.ndjson` for `02d4d478` (2,647 shots): floats match
//! at f32 precision, net GUIDs exactly, vectors bit-exact at f64. Tag indices
//! are replay-specific (`NetworkGameplayTagNodeIndex`); the wiring layer
//! resolves them to names like `FiringState.AmmoRemaining`. Derived from the
//! reference's `EffectData{Float,Object,Vector}.cs`,
//! `ReplayPlayContinuousEffectAtLocationParameters.cs` and
//! `RepLayoutArrayDecoders.cs`.
//!
//! Submodules: `framing` (the shared framing and
//! [`scan_element_handles`](crate::effect::scan_element_handles), which derives
//! the handle pair), `elements` (the element types and their decoders) and
//! `json` (the export's rendering).

mod elements;
mod framing;
mod json;
#[cfg(test)]
mod tests;

pub use elements::{
    EffectDataFloat, EffectDataObject, EffectDataVector, decode_effect_floats,
    decode_effect_floats_at, decode_effect_objects, decode_effect_objects_at,
    decode_effect_vectors, decode_effect_vectors_at,
};
pub use framing::scan_element_handles;
pub use json::decode_effect_blob_json;

/// Errors that can occur while decoding an effect array blob.
#[derive(Debug, Clone, thiserror::Error)]
pub enum EffectBlobError {
    /// The underlying bit reader hit EOF or produced a malformed primitive.
    #[error("bit read: {0}")]
    BitIo(#[from] vrf_bitio::BitError),

    /// The declared array element count exceeds a sane maximum.
    #[error("array count {count} exceeds maximum {max}")]
    ArrayCountTooLarge { count: u32, max: u32 },

    /// An element index is out of bounds relative to the declared count.
    #[error("element index {index} >= declared count {count}")]
    IndexOutOfBounds { index: u32, count: u32 },

    /// A field payload declared more bits than remain in the stream.
    #[error("field payload {bits} bits exceeds remaining {remaining}")]
    PayloadTooLarge { bits: u32, remaining: u64 },

    /// Too many fields in a single element (guard against infinite loops).
    #[error("too many fields in element ({context})")]
    TooManyFields { context: &'static str },

    /// The declared bit length is longer than the buffer that carries it.
    #[error("declared {bits} bits but buffer holds {available}")]
    BitLengthExceedsBuffer { bits: u32, available: u64 },

    /// Bits left in the declared window after the terminator. A blob in this
    /// format consumes its window exactly (all 61,617 on `02d4d478` end at 0
    /// bits); a payload that is not this format usually parses into something
    /// plausible and leaves a tail. Every leftover bit counts, a sub-byte tail
    /// included: `bit_count` is the declared payload length, padding excluded.
    #[error("{remaining} bits left after terminator")]
    ResidualBits { remaining: u64 },

    /// A float element decoded to NaN or an infinity. JSON has no literal for
    /// either and `null` or `0` would fabricate, so the blob is rejected. Zero
    /// occurrences in the 24,000 float blobs on `02d4d478`.
    #[error("element {index} is a non-finite float")]
    NonFiniteFloat { index: usize },

    /// A value field declared a width its type cannot occupy. Checked before
    /// the read, which would otherwise run past a short field into the next.
    #[error("{context}: expected a {expected}-bit payload, found {found}")]
    UnexpectedPayloadWidth {
        context: &'static str,
        expected: u32,
        found: u32,
    },

    /// An element carried a number of fields other than two. All 128,000
    /// elements on `02d4d478` carry a tag and a value, and two is what makes
    /// the handle pair derivable.
    #[error("element has {found} field(s), expected 2")]
    ElementFieldCount { found: u32 },

    /// An element's two field handles were not adjacent.
    #[error("field handles {first} and {second} are not adjacent")]
    NonAdjacentHandles { first: u32, second: u32 },

    /// Two elements of one array disagreed about the handle base.
    #[error("handle base {found} contradicts {expected} seen earlier")]
    InconsistentHandleBase { expected: u32, found: u32 },

    /// A field's type consumed more bits than the field declared.
    #[error("field declared {declared} bits but its type read {consumed}")]
    PayloadOverread { declared: u32, consumed: u64 },

    /// A field's type consumed FEWER bits than the field declared; see
    /// `settle_field` in `effect/framing.rs`.
    #[error("field declared {declared} bits but its type read only {consumed}")]
    PayloadUnderread { declared: u32, consumed: u64 },

    /// The array or an element ran out of bits before its zero terminator.
    /// The array is sparse, so without this check a truncated payload would
    /// render its missing elements as absent and pass for a complete one.
    #[error("{context} ended without its terminator")]
    MissingTerminator { context: &'static str },

    /// The trailing byte after the array terminator was not zero; see
    /// `consume_trailing_terminator` in `effect/framing.rs`.
    #[error("trailing terminator byte is {value}, expected 0")]
    NonZeroTerminator { value: u32 },
}

/// Convenience alias.
pub type Result<T> = core::result::Result<T, EffectBlobError>;

/// The pair of RepLayout field handles an `FEffectData*` element uses: one for
/// the gameplay tag, one for the value.
///
/// Not a constant: Unreal numbers a dynamic array's element handles from the
/// array's own handle in the parent layout, so the same struct arrives under a
/// different pair in every function. Measured on `02d4d478` over ten functions
/// -- a measurement, not an enumeration: another build can declare a base none
/// of these use, which is why the pair is derived rather than tabulated.
///
/// | Function | FloatValues | ObjectValues | VectorValues |
/// |----------|-------------|--------------|--------------|
/// | `ReplayPlayContinuousEffectAtLocation` | 7/8 | 15/16 | 11/12 |
/// | `ClientPlayOneShotEffectAtLocation` | 3/4 | 11/12 | -- |
/// | `MulticastPlayContinuousEffect` | 3/4 | 11/12 | 7/8 |
/// | `MulticastPlayContinuousEffectFromClient` | 4/5 | 12/13 | -- |
/// | `MulticastPlayOneShotEffect` | 3/4 | 11/12 | -- |
/// | `MulticastPlayOneShotEffectFromClient` | 4/5 | 12/13 | -- |
/// | `MulticastUpdateContinuousEffect` | 6/7 | -- | -- |
/// | `ReplayPlayOneShotEffectAtLocation` | 3/4 | 11/12 | 7/8 |
/// | `ReplayRecordOneShotEffect` | -- | 11/12 | -- |
/// | `ReplayRecordContinuousEffect` | 3/4 | 11/12 | -- |
///
/// Treating the first row as universal decodes every other function's elements
/// to all-`None`, and worse: `MulticastUpdateContinuousEffect`'s float value
/// sits at handle 7, the first row's tag slot, so 298 elements decoded a
/// 32-bit float payload as a tag index -- confident, wrong numbers.
/// [`scan_element_handles`] derives the pair from the blob instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectHandles {
    /// Handle of the `FGameplayTag` member. Always the lower of the two.
    pub tag: u32,
    /// Handle of the value member. Always `tag + 1`.
    pub value: u32,
}

impl EffectHandles {
    /// The pair whose tag member is at `base`; saturates rather than leave a
    /// debug-build overflow panic on the export path.
    #[must_use]
    pub const fn from_base(base: u32) -> Self {
        Self {
            tag: base,
            value: base.saturating_add(1),
        }
    }
}

/// `FEffectDataFloat` handles in `ReplayPlayContinuousEffectAtLocation`, as the
/// C# descriptors state them and the pinned vectors were captured under.
const FLOAT_HANDLES: EffectHandles = EffectHandles::from_base(7);

/// `FEffectDataObject` handles in `ReplayPlayContinuousEffectAtLocation`.
const OBJECT_HANDLES: EffectHandles = EffectHandles::from_base(15);

/// `FEffectDataVector` handles in `ReplayPlayContinuousEffectAtLocation`.
const VECTOR_HANDLES: EffectHandles = EffectHandles::from_base(11);

/// Which `FEffectData*` element type a blob carries. The three share one
/// framing and differ only in the value decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectArrayKind {
    /// `TArray<FEffectDataFloat>`, declared as `FloatValues`.
    Float,
    /// `TArray<FEffectDataObject>`, declared as `ObjectValues`.
    Object,
    /// `TArray<FEffectDataVector>`, declared as `VectorValues`.
    Vector,
}

impl EffectArrayKind {
    /// Map an RPC parameter's declared name to its element type, `None` for
    /// any other name. By name, not handle: the handle is the parameter's
    /// index within its function and differs across the ten functions.
    #[must_use]
    pub fn from_param_name(name: &str) -> Option<Self> {
        match name {
            "FloatValues" => Some(Self::Float),
            "ObjectValues" => Some(Self::Object),
            "VectorValues" => Some(Self::Vector),
            _ => None,
        }
    }
}
