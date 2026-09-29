//! RepLayout DynamicArray decoder -- parses nested struct arrays from raw bits.
//!
//! # Wire format (confirmed against C# `RepLayoutArrayDecoders.cs`)
//!
//! ```text
//! DynamicArray:
//!   elementCount = IntPacked         // declared capacity
//!   loop:
//!     encodedIndex = IntPacked       // 0 = terminator
//!     index = encodedIndex - 1
//!     if index >= elementCount:
//!       skip remaining bits -> break
//!     [element payload]:
//!       if primitive array:
//!         single-value decode (e.g. ObjectNetGuid per element)
//!       if struct array:
//!         loop:                      // same handle/payloadBits framing as RepLayout
//!           encodedHandle = IntPacked  // 0 = end of element
//!           handle = encodedHandle - 1
//!           payloadBits = IntPacked
//!           if payloadBits == 0: continue
//!           field data = payloadBits bits
//!           -> recurse if this field is itself an array
//! ```
//!
//! # Limits
//!
//! The C# parser uses `MaxItems = 256` elements and `MaxFields = 128` fields per
//! element (`CombatRoundReportsDecoder`). Ours: [`MAX_ELEMENTS`] 4096 (real data
//! peaks ~50), [`MAX_FIELDS_PER_ELEMENT`] 128 and [`MAX_RECURSION_DEPTH`] 12 (real
//! data is 4-5 deep). A limit hit keeps the remaining raw bits and counts a
//! truncation; nothing panics.
//!
//! # Output
//!
//! One [`FlattenedField`] per leaf, with a path such as
//! `Rounds[3].Reports[1].DamageDealt`, or `Rounds[0]._h18` where no name is
//! known: filterable with `LIKE`, cheap under Parquet dictionary encoding,
//! trivial to parse. Each leaf owns one `String` and one `Vec<u8>`; the path is
//! built in one reused buffer.

mod schema;

use std::fmt::Write as _;

use vrf_bitio::BitReader;

pub use schema::{
    ABILITY_CASTS_SCHEMA, ABILITY_EFFECTS_SCHEMA, ArrayFieldSchema, COMBAT_ROUNDS_SCHEMA,
    LIFE_CHANGE_BY_SECTION_SCHEMA, LIFE_CHANGE_DAMAGE_SCHEMA, LIFE_CHANGE_SECTION_SCHEMA,
};

/// Maximum number of elements per array level.
pub const MAX_ELEMENTS: u32 = 4096;
/// Maximum number of fields per struct element.
pub const MAX_FIELDS_PER_ELEMENT: u32 = 128;
/// Maximum recursion depth for nested arrays.
pub const MAX_RECURSION_DEPTH: u32 = 12;

/// Starting capacity for the emitted-field vector. The reference replay's
/// CombatReport rounds run to a few hundred leaves per blob; this skips the
/// first handful of doublings without over-reserving for a small array.
const INITIAL_FIELD_CAPACITY: usize = 32;

/// A single flattened field emitted from array decoding.
#[derive(Debug, Clone)]
pub struct FlattenedField {
    /// Dotted path from the array root, e.g. `"[0].RoundNumber"` or
    /// `"[2].Reports[1].DamageDealt"`. The caller prepends the parent field
    /// name (e.g. `"Rounds"`) to form the final `field_name`.
    pub path: String,
    /// Handle of the leaf field within its containing struct.
    pub handle: u32,
    /// Number of payload bits.
    pub bit_count: u32,
    /// Raw payload bytes (ceil(bit_count/8) bytes).
    pub raw_bits: Vec<u8>,
}

/// Statistics from one array decode pass.
#[derive(Debug, Clone, Default)]
pub struct ArrayDecodeStats {
    /// Total elements decoded across all array levels.
    pub elements_decoded: u64,
    /// Total leaf fields emitted.
    pub fields_emitted: u64,
    /// Number of times a limit was hit (element count, field count, or depth).
    pub truncations: u64,
    /// BitIo read failures or declared-vs-available overruns the walker
    /// recovered from by abandoning the rest of the stream. The caller still
    /// emits the parent's raw bits, so this is the only sign leaves were lost.
    pub errors: u64,
    /// Declared bits inside a nested window the walk stepped past without
    /// reading or moving another counter: what a nested array left in its own
    /// window, and in the object-reference walker the payload of any field
    /// after an element's first. A tally, not an error (`decode_struct_fields`).
    pub unconsumed_nested_bits: u64,
    /// Bits the walk left in the root window: only those after the array's
    /// explicit index terminator (a nonzero trailer included). Every stop that
    /// moves `errors` or `truncations`, at the array level or in an element,
    /// consumes the rest of the stream, so it is that one anomaly and is not
    /// counted again here; a stop that moves no other counter must leave its
    /// bits here, or it would go unreported. They stay in the parent's raw
    /// bits; the nested equivalent is [`Self::unconsumed_nested_bits`].
    pub unconsumed_root_bits: u64,
    /// Times an element's field loop or an array level ended because the reader
    /// ran out instead of reading its explicit `0` terminator: the only sign
    /// that a payload cut short mid-stream is not a complete one. A tally, not
    /// an error, in case a well-formed window loses its last terminator to byte
    /// padding -- a hypothetical case: `tools/fixtures/build_verification.json`
    /// records 0 on all 24 builds (1,018 replays, main and checkpoint, 259ed10).
    pub implicit_terminations: u64,
}

impl ArrayDecodeStats {
    /// Add every counter of `other` into `self`: the one place a walk's
    /// counters are summed. The destructure has no `..`, so a new field does
    /// not compile until it is summed here.
    pub fn merge_from(&mut self, other: &Self) {
        let Self {
            elements_decoded,
            fields_emitted,
            truncations,
            errors,
            unconsumed_nested_bits,
            unconsumed_root_bits,
            implicit_terminations,
        } = other;
        self.elements_decoded += elements_decoded;
        self.fields_emitted += fields_emitted;
        self.truncations += truncations;
        self.errors += errors;
        self.unconsumed_nested_bits += unconsumed_nested_bits;
        self.unconsumed_root_bits += unconsumed_root_bits;
        self.implicit_terminations += implicit_terminations;
    }

    /// Whether a walk recorded none of the five failure shapes: measured routes
    /// emit typed children only from a clean walk. The destructure has no `..`,
    /// so a new field must be classified as work (`_`) or failure first.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        let Self {
            elements_decoded: _,
            fields_emitted: _,
            truncations,
            errors,
            unconsumed_nested_bits,
            unconsumed_root_bits,
            implicit_terminations,
        } = self;
        *truncations == 0
            && *errors == 0
            && *unconsumed_nested_bits == 0
            && *unconsumed_root_bits == 0
            && *implicit_terminations == 0
    }
}

/// Everything one walk carries down through the recursion, the same at every
/// level.
struct Walk<'a, 'd> {
    allow_trailing_int_packed: bool,
    /// Names the REPLAY declares for this group's handles, indexed by handle.
    declared: &'a [Option<&'d str>],
    /// Path under construction, reused across every leaf.
    path: String,
    output: Vec<FlattenedField>,
}

/// Decode a DynamicArray of structs from raw bits into flattened leaves.
///
/// `schema` says which handles at each depth are themselves arrays (recursed
/// into); `None` emits every field as a leaf. `declared[h]` is the name the
/// REPLAY declares for handle `h` (`&[]` when there is no declaration). A leaf
/// is labelled declaration -> schema -> `_h{handle}`, a container segment by
/// the schema alone (see the internal `push_leaf_label`).
pub fn decode_struct_array(
    data: &[u8],
    bit_count: u32,
    schema: Option<&ArrayFieldSchema>,
    declared: &[Option<&str>],
    stats: &mut ArrayDecodeStats,
) -> Vec<FlattenedField> {
    decode_struct_array_window(data, bit_count, schema, declared, stats, true)
}

/// Walk a flat struct array without accepting the legacy optional trailer.
///
/// Newly measured array routes have an explicit index terminator and no
/// trailer. Remaining bits are reported in `unconsumed_root_bits`; callers
/// must check all diagnostics before accepting the returned leaves.
pub fn decode_struct_array_exact(
    data: &[u8],
    bit_count: u32,
    declared: &[Option<&str>],
    stats: &mut ArrayDecodeStats,
) -> Vec<FlattenedField> {
    decode_struct_array_window(data, bit_count, None, declared, stats, false)
}

fn decode_struct_array_window(
    data: &[u8],
    bit_count: u32,
    schema: Option<&ArrayFieldSchema>,
    declared: &[Option<&str>],
    stats: &mut ArrayDecodeStats,
    allow_trailing_int_packed: bool,
) -> Vec<FlattenedField> {
    let Ok(mut reader) = BitReader::with_bit_len(data, u64::from(bit_count)) else {
        stats.errors += 1;
        return Vec::new();
    };
    let mut walk = Walk {
        allow_trailing_int_packed,
        declared,
        path: String::with_capacity(64),
        output: Vec::with_capacity(INITIAL_FIELD_CAPACITY),
    };
    decode_array_level(&mut reader, &mut walk, schema, 0, stats);
    stats.unconsumed_root_bits += reader.bits_remaining();
    walk.output
}

/// Decode a RepLayout dynamic array of object references (`TArray<UObject*>`)
/// into `(wire index, NetGUID)` pairs, without diagnostics.
///
/// `MultiItemSlot.MultiContents` (C# `TArray<AAresItem*>`) is this shape: the
/// [`decode_struct_array`] framing with exactly one field per element, at
/// handle 2, holding the item actor's NetGUID as one IntPacked -- confirmed on
/// 245/245 wire payloads. Malformed input returns what was decoded so far; the
/// caller keeps the parent's raw bits.
pub fn decode_object_ref_array(data: &[u8], bit_count: u32) -> Vec<(u32, u32)> {
    let mut ignored = ArrayDecodeStats::default();
    decode_object_ref_array_with_stats(data, bit_count, &mut ignored)
}

/// [`decode_object_ref_array`] with every partial or malformed path counted in
/// `stats`: the production entry point for `MultiContents`.
///
/// Pairs, not a dense `Vec` in arrival order: the array is delta-replicated per
/// element (a re-send of a 3-slot array can carry only wire index 1), so the
/// wire index is the only thing that says which slot a GUID belongs in.
pub fn decode_object_ref_array_with_stats(
    data: &[u8],
    bit_count: u32,
    stats: &mut ArrayDecodeStats,
) -> Vec<(u32, u32)> {
    let Ok(mut reader) = BitReader::with_bit_len(data, u64::from(bit_count)) else {
        stats.errors += 1;
        return Vec::new();
    };
    let out = decode_object_ref_array_reader(&mut reader, stats);
    stats.unconsumed_root_bits += reader.bits_remaining();
    out
}

/// Every stop that moves `errors` or `truncations` skips the rest of the
/// stream, as in `decode_array_level`: the caller tallies what is left as
/// `unconsumed_root_bits`, which would count the same stop twice.
fn decode_object_ref_array_reader(
    reader: &mut BitReader<'_>,
    stats: &mut ArrayDecodeStats,
) -> Vec<(u32, u32)> {
    let mut out = Vec::new();

    let Ok(element_count) = reader.read_int_packed() else {
        stats.errors += 1;
        reader.skip_remaining();
        return out;
    };
    if element_count > MAX_ELEMENTS {
        stats.truncations += 1;
        reader.skip_remaining();
        return out;
    }

    // An explicit `0` index ends the array; EOF instead is accepted but
    // tallied, as in `decode_array_level` (in a delta array, slots after a cut
    // would pass for slots not re-sent). An element that runs out of bits has
    // tallied itself and leaves through the `element_complete` break instead.
    let mut elements_seen = 0u32;
    loop {
        if reader.at_end() {
            stats.implicit_terminations += 1;
            break;
        }
        if elements_seen == MAX_ELEMENTS {
            match take_zero_int_packed(reader) {
                Ok(true) => consume_optional_trailing_int_packed(reader, stats),
                Ok(false) => {
                    stats.truncations += 1;
                    reader.skip_remaining();
                }
                Err(_) => {
                    stats.errors += 1;
                    reader.skip_remaining();
                }
            }
            return out;
        }
        let Ok(encoded_index) = reader.read_int_packed() else {
            stats.errors += 1;
            reader.skip_remaining();
            break;
        };
        if encoded_index == 0 {
            consume_optional_trailing_int_packed(reader, stats);
            break;
        }
        let index = encoded_index - 1;
        if index >= element_count {
            stats.errors += 1;
            reader.skip_remaining();
            break;
        }
        elements_seen += 1;
        stats.elements_decoded += 1;

        // The first populated field is the item's IntPacked NetGUID. Later
        // fields are stepped over (bounded, to stay aligned) and their payload
        // goes to `unconsumed_nested_bits`: in the 1,018-replay audit corpus
        // each of the 273,386 elements (all main stream; every checkpoint
        // `MultiContents` array is empty) carries exactly one populated field.
        // A zero-width field has no payload and is skipped untallied; an
        // element of only those yields no row (none occur in that corpus).
        let mut guid = None;
        // Keyed on position, not on `guid`: a first field that fails to decode
        // is still the first, and the next one must not stand in for it.
        let mut first_field = true;
        let mut element_complete = false;
        for field_idx in 0..=MAX_FIELDS_PER_ELEMENT {
            if reader.at_end() {
                stats.implicit_terminations += 1;
                break;
            }
            if field_idx == MAX_FIELDS_PER_ELEMENT {
                match take_zero_int_packed(reader) {
                    Ok(true) => element_complete = true,
                    Ok(false) => stats.truncations += 1,
                    Err(_) => stats.errors += 1,
                }
                break;
            }
            let Ok(encoded_handle) = reader.read_int_packed() else {
                stats.errors += 1;
                break;
            };
            if encoded_handle == 0 {
                element_complete = true;
                break;
            }
            let Ok(payload_bits) = reader.read_int_packed() else {
                stats.errors += 1;
                break;
            };
            if payload_bits == 0 {
                continue;
            }
            if u64::from(payload_bits) > reader.bits_remaining() {
                stats.errors += 1;
                reader.skip_remaining();
                break;
            }
            // `sub_reader` advances the parent past the whole window, so the
            // reader stays aligned however many bits the NetGUID spends.
            let Ok(mut sub) = reader.sub_reader(u64::from(payload_bits)) else {
                stats.errors += 1;
                break;
            };
            if first_field {
                first_field = false;
                match sub.read_int_packed() {
                    Ok(v) if sub.at_end() => guid = Some(v),
                    Ok(_) | Err(_) => stats.errors += 1,
                }
            } else {
                stats.unconsumed_nested_bits += u64::from(payload_bits);
            }
        }

        if let Some(g) = guid {
            out.push((index, g));
            stats.fields_emitted += 1;
        }
        if !element_complete {
            reader.skip_remaining();
            break;
        }
    }

    out
}

/// Recursively decode one array level.
///
/// Every stop that moves `errors` or `truncations` consumes the rest of the
/// stream, skipped or kept as a `._raw` leaf: the caller tallies what is left
/// as `unconsumed_root_bits` (`unconsumed_nested_bits` in a nested window),
/// which would count the same stop twice.
fn decode_array_level(
    reader: &mut BitReader<'_>,
    walk: &mut Walk<'_, '_>,
    schema: Option<&ArrayFieldSchema>,
    depth: u32,
    stats: &mut ArrayDecodeStats,
) {
    if reader.at_end() {
        return;
    }

    let Ok(element_count) = reader.read_int_packed() else {
        stats.errors += 1;
        reader.skip_remaining();
        return;
    };

    if element_count > MAX_ELEMENTS {
        stats.truncations += 1;
        emit_remaining_raw(reader, walk, stats);
        return;
    }

    // An explicit `0` index ends the array; EOF instead is accepted but
    // tallied (`implicit_terminations`).
    let mut elements_seen = 0u32;
    loop {
        if reader.at_end() {
            stats.implicit_terminations += 1;
            break;
        }
        if elements_seen == MAX_ELEMENTS {
            match take_zero_int_packed(reader) {
                Ok(true) if walk.allow_trailing_int_packed => {
                    consume_optional_trailing_int_packed(reader, stats);
                }
                Ok(true) => {}
                Ok(false) => {
                    stats.truncations += 1;
                    emit_remaining_raw(reader, walk, stats);
                }
                Err(_) => {
                    stats.errors += 1;
                    reader.skip_remaining();
                }
            }
            break;
        }
        let Ok(encoded_index) = reader.read_int_packed() else {
            stats.errors += 1;
            reader.skip_remaining();
            break;
        };

        if encoded_index == 0 {
            if walk.allow_trailing_int_packed {
                consume_optional_trailing_int_packed(reader, stats);
            }
            break;
        }

        let index = encoded_index - 1;
        if index >= element_count {
            stats.errors += 1;
            reader.skip_remaining();
            break;
        }

        elements_seen += 1;
        stats.elements_decoded += 1;
        let prefix_len = walk.path.len();
        let _ = write!(walk.path, "[{index}]");
        let closed = decode_struct_fields(reader, walk, schema, depth, stats);
        walk.path.truncate(prefix_len);
        // An element that did not close has tallied why; reading on would count
        // the same bits again, so, as in the object-reference walker, the rest
        // of the stream is abandoned: one anomaly, one counter.
        if !closed {
            reader.skip_remaining();
            break;
        }
    }
}

/// Decode the struct fields of one array element (handle/payloadBits loop).
///
/// Returns whether the element closed on its zero handle. Every other exit
/// has moved a counter first, and the caller stops the array on it.
fn decode_struct_fields(
    reader: &mut BitReader<'_>,
    walk: &mut Walk<'_, '_>,
    schema: Option<&ArrayFieldSchema>,
    depth: u32,
    stats: &mut ArrayDecodeStats,
) -> bool {
    for field_idx in 0..=MAX_FIELDS_PER_ELEMENT {
        if reader.at_end() {
            // EOF instead of the zero handle: the element was never closed,
            // which only this tally tells apart from a complete one.
            stats.implicit_terminations += 1;
            return false;
        }

        if field_idx == MAX_FIELDS_PER_ELEMENT {
            match take_zero_int_packed(reader) {
                Ok(true) => return true,
                Ok(false) => {
                    stats.truncations += 1;
                    emit_remaining_raw(reader, walk, stats);
                }
                Err(_) => stats.errors += 1,
            }
            return false;
        }

        let Ok(encoded_handle) = reader.read_int_packed() else {
            stats.errors += 1;
            return false;
        };
        if encoded_handle == 0 {
            return true;
        }
        let handle = encoded_handle - 1;

        let Ok(payload_bits) = reader.read_int_packed() else {
            stats.errors += 1;
            return false;
        };
        if payload_bits == 0 {
            continue;
        }
        if u64::from(payload_bits) > reader.bits_remaining() {
            // Declared more bits than available: the lost leaves are an error.
            stats.errors += 1;
            reader.skip_remaining();
            return false;
        }

        match schema.and_then(|s| s.sub_array(handle)) {
            // A nested array we are still allowed to descend into.
            Some(sub) if depth + 1 < MAX_RECURSION_DEPTH => {
                let Ok(mut sub_reader) = reader.sub_reader(u64::from(payload_bits)) else {
                    // Unreachable (bounds-checked above), and counted anyway so
                    // the stats can contradict that claim.
                    stats.errors += 1;
                    return false;
                };
                let prefix_len = walk.path.len();
                walk.path.push('.');
                push_field_label(&mut walk.path, schema, handle);
                decode_array_level(&mut sub_reader, walk, Some(sub), depth + 1, stats);
                // The parent is ALIGNED past the window whatever the child
                // CONSUMED, so leaves in an abandoned tail would vanish: tallied
                // like `truncations` (the bits survive in the parent's raw bits).
                stats.unconsumed_nested_bits += sub_reader.bits_remaining();
                walk.path.truncate(prefix_len);
            }
            // Same, but the nesting limit stops us -- keep the bits instead.
            Some(_) => {
                stats.truncations += 1;
                let Some(raw) = copy_payload(reader, payload_bits) else {
                    // Unreachable for the same reason; counted the same way.
                    stats.errors += 1;
                    return false;
                };
                emit(walk, stats, handle, payload_bits, raw, |path| {
                    push_field_label(path, schema, handle);
                });
            }
            None => {
                let Some(raw) = copy_payload(reader, payload_bits) else {
                    // Unreachable for the same reason; counted the same way.
                    stats.errors += 1;
                    return false;
                };
                let declared = walk.declared;
                emit(walk, stats, handle, payload_bits, raw, |path| {
                    push_leaf_label(path, declared, schema, handle);
                });
            }
        }
    }
    // Unreachable: the last iteration (`field_idx == MAX_FIELDS_PER_ELEMENT`)
    // always returns. Not closed, should that ever change.
    false
}

/// Consume the format's optional one-IntPacked trailer -- a ZERO one only.
///
/// The C# reference reads an IntPacked whenever exactly eight bits remain after
/// the index terminator and discards it, so any appended byte passes. Like
/// `consume_trailing_terminator` in `effect/framing.rs` this declines, but
/// tallies instead of rejecting (callers keep the leaves already emitted): `0`
/// is consumed silently; another value is left unread for the caller's residual
/// tally (`unconsumed_root_bits`, or `unconsumed_nested_bits` in a nested
/// window); a failed read is `errors` only. Over the 1,018-replay audit corpus
/// (main and checkpoint) a trailer is read 240 times, all zero, all in
/// `AbilityCastsThisRound`'s depth-2 `AffectedTargetsArray`.
fn consume_optional_trailing_int_packed(reader: &mut BitReader<'_>, stats: &mut ArrayDecodeStats) {
    if reader.bits_remaining() != 8 {
        return;
    }
    if take_zero_int_packed(reader).is_err() {
        stats.errors += 1;
        reader.skip_remaining();
    }
}

/// Consume the next IntPacked only if it is `0`: `Ok(true)` when it was, and
/// `Ok(false)` or `Err` -- another value, or a failed read -- with the reader
/// untouched. The one probe every limit check and the trailer share.
fn take_zero_int_packed(reader: &mut BitReader<'_>) -> vrf_bitio::Result<bool> {
    let mut probe = reader.clone();
    let zero = probe.read_int_packed()? == 0;
    if zero {
        *reader = probe;
    }
    Ok(zero)
}

/// Take `payload_bits` bits out of `reader` as owned bytes, or `None` when the
/// window cannot be opened (the caller then stops the element).
fn copy_payload(reader: &mut BitReader<'_>, payload_bits: u32) -> Option<Vec<u8>> {
    let mut raw = vec![0u8; (payload_bits as usize).div_ceil(8)];
    let mut sub_reader = reader.sub_reader(u64::from(payload_bits)).ok()?;
    let _ = sub_reader.copy_bits_to(&mut raw, u64::from(payload_bits));
    Some(raw)
}

/// Append `.<label>` to the walk's path, push one record, and restore the path.
fn emit(
    walk: &mut Walk<'_, '_>,
    stats: &mut ArrayDecodeStats,
    handle: u32,
    bit_count: u32,
    raw_bits: Vec<u8>,
    label: impl FnOnce(&mut String),
) {
    let prefix_len = walk.path.len();
    walk.path.push('.');
    label(&mut walk.path);
    walk.output.push(FlattenedField {
        path: walk.path.clone(),
        handle,
        bit_count,
        raw_bits,
    });
    stats.fields_emitted += 1;
    walk.path.truncate(prefix_len);
}

/// Emit all remaining bits as a single raw field, counted in `fields_emitted`
/// like [`emit`]: it becomes a row too.
fn emit_remaining_raw(
    reader: &mut BitReader<'_>,
    walk: &mut Walk<'_, '_>,
    stats: &mut ArrayDecodeStats,
) {
    let remaining = reader.bits_remaining();
    if remaining == 0 {
        return;
    }
    let mut raw = vec![0u8; (remaining as usize).div_ceil(8)];
    let _ = reader.copy_bits_to(&mut raw, remaining);
    let mut path = walk.path.clone();
    path.push_str("._raw");
    walk.output.push(FlattenedField {
        path,
        handle: u32::MAX,
        bit_count: remaining as u32,
        raw_bits: raw,
    });
    stats.fields_emitted += 1;
}

/// Append a LEAF handle's label: the replay's declared name, then the schema's,
/// then `_h{handle}`. The replay wins because the schema transcribes the C#
/// reference and can disagree with the wire: handle 3 is `RoundNum` on the
/// wire and `RoundNumber` in the schema, and Riot's typos (`DamageRecieved`,
/// `HitsRecieved`) were silently corrected there. Container segments keep
/// [`push_field_label`]: the schema decides the nesting, and handles 44 and 79
/// both declare `RegionalDamageInteractions`.
///
/// `declared` applies at every depth on purpose: Unreal flattens a `TArray` of
/// structs onto consecutive handles of the ENCLOSING group, so one handle space
/// spans the tree (`COMBAT_ROUNDS_SCHEMA`'s handles are disjoint by depth).
/// `DamageRecieved`/`HitsRecieved` are handles 20/21 at depth 2, and
/// `tools/to_valplay_bundle.py`'s test pins the path
/// `Rounds[0].Reports[0].Interactions[0].DamageRecieved`. The `LIFE_CHANGE_*`
/// schemas number per RPC parameter (10-13, 1-4, 2-5) but declare no
/// `sub_arrays`, and the export passes them `declared = &[]`.
fn push_leaf_label(
    path: &mut String,
    declared: &[Option<&str>],
    schema: Option<&ArrayFieldSchema>,
    handle: u32,
) {
    if let Some(name) = declared.get(handle as usize).copied().flatten() {
        path.push_str(name);
        return;
    }
    push_field_label(path, schema, handle);
}

/// Append a handle's label from the schema's name map, falling back to
/// `_h{handle}` when the schema cannot name it.
fn push_field_label(path: &mut String, schema: Option<&ArrayFieldSchema>, handle: u32) {
    match schema.and_then(|s| s.field_name(handle)) {
        Some(name) => path.push_str(name),
        None => {
            let _ = write!(path, "_h{handle}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_bits::BitWriter;

    /// Handle 4 at depth 0 is a sub-array, `Reports`, with no further nesting.
    static INNER: ArrayFieldSchema = ArrayFieldSchema {
        sub_arrays: &[],
        field_names: &[],
    };
    static OUTER: ArrayFieldSchema = ArrayFieldSchema {
        sub_arrays: &[(4, &INNER)],
        field_names: &[(4, "Reports")],
    };

    /// One element carrying one field per `(handle, payload width)`, each
    /// payload all ones, then the element and array terminators.
    fn one_element_with_handles(handles: &[(u32, u32)]) -> BitWriter {
        let mut bits = BitWriter::new();
        bits.int_packed(1); // elementCount
        bits.int_packed(1); // encodedIndex=1 -> index 0
        for &(handle, payload) in handles {
            bits.int_packed(handle + 1);
            bits.int_packed(payload);
            bits.repeat(true, payload as usize);
        }
        bits.int_packed(0); // end of element
        bits.int_packed(0); // array terminator
        bits
    }

    /// One outer element whose handle 4 (`Reports` under [`OUTER`]) is a
    /// nested window: `inner`, then `tail` one-bits the inner array never
    /// reads, inside the window's declared width.
    fn nested(inner: &BitWriter, tail: usize) -> (Vec<u8>, u32) {
        let mut bits = BitWriter::new();
        bits.int_packed(1); // outer elementCount
        bits.int_packed(1); // encodedIndex=1
        bits.int_packed(5); // encodedHandle=5 -> handle 4
        bits.int_packed(inner.bit_len() + tail as u32);
        bits.append(inner).repeat(true, tail);
        bits.int_packed(0); // end outer element
        bits.int_packed(0); // outer array terminator
        bits.finish()
    }

    #[test]
    fn decode_simple_struct_array() {
        // elementCount=2, element[0] has handle=3 with 32 bits, element[1]
        // has handle=5 with 8 bits, then terminators.
        let mut bits = BitWriter::new();
        bits.int_packed(2); // elementCount
        // Element 0 (encodedIndex=1): encodedHandle=4 (handle=3), payloadBits=32
        bits.int_packed(1);
        bits.int_packed(4);
        bits.int_packed(32);
        bits.repeat(true, 32); // payload
        bits.int_packed(0); // end of element 0
        // Element 1 (encodedIndex=2): encodedHandle=6 (handle=5), payloadBits=8
        bits.int_packed(2);
        bits.int_packed(6);
        bits.int_packed(8);
        bits.repeat(false, 8); // payload
        bits.int_packed(0); // end of element 1
        // Array terminator
        bits.int_packed(0);

        let (data, bit_count) = bits.finish();
        let mut stats = ArrayDecodeStats::default();
        let fields = decode_struct_array(&data, bit_count, None, &[], &mut stats);

        assert_eq!(stats.elements_decoded, 2);
        assert_eq!(stats.fields_emitted, 2);
        assert_eq!(stats.truncations, 0);
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].path, "[0]._h3");
        assert_eq!(fields[0].handle, 3);
        assert_eq!(fields[0].bit_count, 32);
        assert_eq!(fields[1].path, "[1]._h5");
        assert_eq!(fields[1].handle, 5);
        assert_eq!(fields[1].bit_count, 8);
    }

    #[test]
    fn decode_nested_struct_array() {
        // One outer element whose handle 4 is a nested array of one element
        // carrying handle 7 with 16 bits.
        let (data, bit_count) = nested(&one_element_with_handles(&[(7, 16)]), 0);
        let mut stats = ArrayDecodeStats::default();
        let fields = decode_struct_array(&data, bit_count, Some(&OUTER), &[], &mut stats);

        assert_eq!(stats.elements_decoded, 2); // 1 outer + 1 inner
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].path, "[0].Reports[0]._h7");
        assert_eq!(fields[0].handle, 7);
        assert_eq!(fields[0].bit_count, 16);
    }

    /// The replay's declaration outranks the hardcoded schema on a leaf.
    ///
    /// Handle 3 is the live case: `COMBAT_ROUNDS_SCHEMA` calls it `RoundNumber`
    /// and every replay declares it `RoundNum`.
    #[test]
    fn a_declared_leaf_name_beats_the_schema_name() {
        let (data, bit_count) = one_element_with_handles(&[(3, 32)]).finish();
        let mut declared: Vec<Option<&str>> = vec![None; 8];
        declared[3] = Some("RoundNum");

        let mut stats = ArrayDecodeStats::default();
        let fields = decode_struct_array(
            &data,
            bit_count,
            Some(&COMBAT_ROUNDS_SCHEMA),
            &declared,
            &mut stats,
        );

        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].path, "[0].RoundNum");
    }

    /// A handle the schema cannot name is labelled by the replay, not `_hN`.
    #[test]
    fn a_declared_leaf_name_replaces_the_handle_placeholder() {
        let (data, bit_count) = one_element_with_handles(&[(7, 32)]).finish();
        let mut declared: Vec<Option<&str>> = vec![None; 8];
        declared[7] = Some("StateRemainingTime");

        let mut stats = ArrayDecodeStats::default();
        let fields = decode_struct_array(
            &data,
            bit_count,
            Some(&COMBAT_ROUNDS_SCHEMA),
            &declared,
            &mut stats,
        );

        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].path, "[0].StateRemainingTime");
    }

    /// With no declaration the schema still names what it can, and an
    /// undeclared, unschematised handle still falls through to `_hN`.
    #[test]
    fn an_undeclared_leaf_falls_back_to_schema_then_placeholder() {
        let (data, bit_count) = one_element_with_handles(&[(3, 32), (7, 32)]).finish();

        let mut stats = ArrayDecodeStats::default();
        let fields = decode_struct_array(
            &data,
            bit_count,
            Some(&COMBAT_ROUNDS_SCHEMA),
            &[],
            &mut stats,
        );

        assert_eq!(fields.len(), 2);
        // Schema names handle 3; nothing names handle 7.
        assert_eq!(fields[0].path, "[0].RoundNumber");
        assert_eq!(fields[1].path, "[0]._h7");
    }

    /// A declaration shorter than the handle it is asked about must not panic
    /// and must fall through, not silently mislabel.
    #[test]
    fn a_handle_past_the_end_of_the_declaration_falls_back() {
        let (data, bit_count) = one_element_with_handles(&[(7, 32)]).finish();
        // Only handles 0..=3 declared; handle 7 is past the end.
        let declared: Vec<Option<&str>> = vec![None, None, None, Some("RoundNum")];

        let mut stats = ArrayDecodeStats::default();
        let fields = decode_struct_array(
            &data,
            bit_count,
            Some(&COMBAT_ROUNDS_SCHEMA),
            &declared,
            &mut stats,
        );

        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].path, "[0]._h7");
    }

    /// Container segments keep the schema's name even when the replay declares
    /// a different one, because the schema is what decides the nesting.
    #[test]
    fn a_container_segment_keeps_its_schema_name() {
        // Outer element 0 carries handle 4 (Reports, a sub-array) whose single
        // element carries handle 5.
        let (data, bit_count) = nested(&one_element_with_handles(&[(5, 32)]), 0);

        // Declare a DIFFERENT name for the container handle 4, and the real
        // declared name for the leaf handle 5.
        let mut declared: Vec<Option<&str>> = vec![None; 8];
        declared[4] = Some("NotReports");
        declared[5] = Some("RoundNumber");

        let mut stats = ArrayDecodeStats::default();
        let fields = decode_struct_array(
            &data,
            bit_count,
            Some(&COMBAT_ROUNDS_SCHEMA),
            &declared,
            &mut stats,
        );

        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].path, "[0].Reports[0].RoundNumber");
    }

    #[test]
    fn empty_array_emits_nothing() {
        let mut bits = BitWriter::new();
        bits.int_packed(0); // elementCount = 0
        bits.int_packed(0); // immediate terminator

        let (data, bit_count) = bits.finish();
        let mut stats = ArrayDecodeStats::default();
        let fields = decode_struct_array(&data, bit_count, None, &[], &mut stats);

        assert!(fields.is_empty());
        assert_eq!(stats.elements_decoded, 0);
        // A legitimately empty array must read as zero errors so a malformed
        // run cannot hide behind the same number.
        assert_eq!(stats.errors, 0);
    }

    /// A payload that declares an element but EOFs mid-field is an error, not
    /// an empty `Vec` that reads like a clean empty array.
    #[test]
    fn truncated_payload_mid_element_counts_error() {
        // elementCount=2, element 0 starts, its first field declares 32 bits
        // of payload but only 8 remain -> overrun.
        let mut bits = BitWriter::new();
        bits.int_packed(2); // elementCount = 2
        bits.int_packed(1); // encodedIndex = 1 -> element 0
        bits.int_packed(4); // encodedHandle = 4 -> handle 3
        bits.int_packed(32); // payloadBits = 32 (overruns)
        bits.repeat(true, 8); // only 8 bits of payload

        let (data, bit_count) = bits.finish();
        let mut stats = ArrayDecodeStats::default();
        let fields = decode_struct_array(&data, bit_count, None, &[], &mut stats);

        // One overrun, one counter: not also an implicit termination from the
        // array loop meeting the EOF the element already reported.
        assert_eq!(anomalies(&stats), [1, 0, 0, 0, 0], "{stats:?}");
        // No complete leaf was emitted; the caller still emits the parent row
        // from its own raw_bits, independent of this Vec.
        assert!(fields.is_empty());
    }

    /// Build the wire shape of `MultiItemSlot.MultiContents` and check the
    /// decoded NetGUIDs. The payload of every element is a single object
    /// reference at handle 2; IntPacked 812 occupies a 16-bit window, 25492 a
    /// 24-bit one, mirroring the two payload widths seen on real replays.
    #[test]
    fn decode_object_ref_array_extracts_item_netguids() {
        let mut bits = BitWriter::new();
        bits.int_packed(2); // elementCount
        // Element 0: NetGUID 812 in a 16-bit payload window.
        bits.int_packed(1); // encodedIndex -> index 0
        bits.int_packed(3); // encodedHandle -> handle 2
        bits.int_packed(16); // payloadBits
        bits.int_packed(812); // ObjectNetGuid payload
        bits.int_packed(0); // element terminator
        // Element 1: NetGUID 25492 in a 24-bit payload window.
        bits.int_packed(2); // encodedIndex -> index 1
        bits.int_packed(3); // handle 2
        bits.int_packed(24); // payloadBits
        bits.int_packed(25492); // ObjectNetGuid payload
        bits.int_packed(0); // element terminator
        bits.int_packed(0); // array terminator

        let (data, bit_count) = bits.finish();
        let guids = decode_object_ref_array(&data, bit_count);

        assert_eq!(guids, vec![(0, 812), (1, 25492)]);
    }

    /// A sparse re-send -- only wire index 1 of a 3-slot array present -- must
    /// keep that index, not relabel the lone GUID onto slot 0.
    #[test]
    fn decode_object_ref_array_preserves_a_sparse_wire_index() {
        let mut bits = BitWriter::new();
        bits.int_packed(3); // elementCount (3-slot array)
        bits.int_packed(2); // encodedIndex -> index 1 (only element sent)
        bits.int_packed(3); // encodedHandle -> handle 2
        bits.int_packed(16); // payloadBits
        bits.int_packed(5150); // ObjectNetGuid payload
        bits.int_packed(0); // element terminator
        bits.int_packed(0); // array terminator

        let (data, bit_count) = bits.finish();
        let guids = decode_object_ref_array(&data, bit_count);

        assert_eq!(guids, vec![(1, 5150)]);
    }

    /// An empty array (elementCount = 0, immediate terminator) decodes to nothing.
    #[test]
    fn decode_object_ref_array_empty() {
        let mut bits = BitWriter::new();
        bits.int_packed(0);
        bits.int_packed(0);

        let (data, bit_count) = bits.finish();
        let guids = decode_object_ref_array(&data, bit_count);

        assert!(guids.is_empty());
    }

    #[test]
    fn malformed_object_ref_array_exposes_its_failure_stats() {
        let mut bits = BitWriter::new();
        bits.int_packed(1);
        bits.int_packed(1);
        bits.int_packed(3);
        bits.int_packed(32); // only eight payload bits follow
        bits.repeat(false, 8);
        let (data, bit_count) = bits.finish();
        let mut stats = ArrayDecodeStats::default();

        let guids = decode_object_ref_array_with_stats(&data, bit_count, &mut stats);

        assert!(guids.is_empty());
        assert_eq!(stats.errors, 1, "{stats:?}");
    }

    /// Every counter a clean walk must leave at zero.
    fn anomalies(stats: &ArrayDecodeStats) -> [u64; 5] {
        [
            stats.errors,
            stats.truncations,
            stats.implicit_terminations,
            stats.unconsumed_root_bits,
            stats.unconsumed_nested_bits,
        ]
    }

    /// One `MultiContents` element at wire index 0 carrying NetGUID 5 in an
    /// 8-bit window, closed by its zero handle.
    fn push_one_closed_item(bits: &mut BitWriter) {
        bits.int_packed(1); // encodedIndex -> index 0
        bits.int_packed(3); // encodedHandle -> handle 2
        bits.int_packed(8); // payloadBits
        bits.int_packed(5); // ObjectNetGuid payload
        bits.int_packed(0); // element terminator
    }

    /// A payload cut short after a complete element, before the index
    /// terminator, must not read as the whole array: in a delta array the
    /// missing elements would pass for ones not re-sent.
    #[test]
    fn an_object_ref_array_ending_at_eof_is_not_a_clean_terminator() {
        let mut bits = BitWriter::new();
        bits.int_packed(2); // elementCount: two slots
        push_one_closed_item(&mut bits);
        // No second element and no array terminator: the payload just ends.
        let (data, bit_count) = bits.finish();
        let mut stats = ArrayDecodeStats::default();

        let guids = decode_object_ref_array_with_stats(&data, bit_count, &mut stats);

        assert_eq!(guids, vec![(0, 5)], "the complete element is still decoded");
        assert_eq!(stats.implicit_terminations, 1, "{stats:?}");
        assert_eq!(stats.errors, 0, "{stats:?}");
        assert_eq!(stats.unconsumed_root_bits, 0, "{stats:?}");
    }

    /// A window holding only the element count ends the same way.
    #[test]
    fn a_count_only_object_ref_window_is_an_implicit_termination() {
        let mut stats = ArrayDecodeStats::default();
        let guids = decode_object_ref_array_with_stats(&[0x02], 8, &mut stats);

        assert!(guids.is_empty());
        assert_eq!(stats.implicit_terminations, 1, "{stats:?}");
        assert_eq!(stats.errors, 0, "{stats:?}");
    }

    /// An element that runs out of bits is ONE implicit termination: the
    /// element's own tally already fires, and the array loop must not add a
    /// second for the same missing bits.
    #[test]
    fn an_object_ref_element_ending_at_eof_counts_once() {
        let mut bits = BitWriter::new();
        bits.int_packed(1); // elementCount
        bits.int_packed(1); // encodedIndex -> index 0
        bits.int_packed(3); // handle 2
        bits.int_packed(8); // payloadBits
        bits.int_packed(5); // NetGUID -- then EOF, no terminators
        let (data, bit_count) = bits.finish();
        let mut stats = ArrayDecodeStats::default();

        let guids = decode_object_ref_array_with_stats(&data, bit_count, &mut stats);

        assert_eq!(guids, vec![(0, 5)]);
        assert_eq!(stats.implicit_terminations, 1, "{stats:?}");
    }

    /// A second populated field (`TArray<AAresItem*>` elements have one) is
    /// stepped over and its payload bits tallied; the 16-bit window shows the
    /// tally counts bits, not fields.
    #[test]
    fn an_extra_field_in_an_object_ref_element_is_tallied() {
        let mut bits = BitWriter::new();
        bits.int_packed(1); // elementCount
        bits.int_packed(1); // encodedIndex -> index 0
        bits.int_packed(3); // handle 2
        bits.int_packed(8); // payloadBits
        bits.int_packed(5); // NetGUID 5
        bits.int_packed(4); // handle 3: a field the type does not have
        bits.int_packed(16); // payloadBits
        bits.int_packed(300); // two IntPacked bytes
        bits.int_packed(0); // element terminator
        bits.int_packed(0); // array terminator
        let (data, bit_count) = bits.finish();
        let mut stats = ArrayDecodeStats::default();

        let guids = decode_object_ref_array_with_stats(&data, bit_count, &mut stats);

        assert_eq!(guids, vec![(0, 5)], "the first field is still the item");
        assert_eq!(stats.unconsumed_nested_bits, 16, "{stats:?}");
        assert_eq!(stats.errors, 0, "{stats:?}");
        assert_eq!(stats.implicit_terminations, 0, "{stats:?}");
    }

    /// The first populated field is the item whether or not it decodes, so a
    /// first field that fails yields no item: the field after it is one the
    /// walker calls foreign, and must not be promoted into the item's place.
    #[test]
    fn an_object_ref_element_whose_first_field_fails_yields_no_item() {
        let mut bits = BitWriter::new();
        bits.int_packed(1); // elementCount
        bits.int_packed(1); // encodedIndex -> index 0
        bits.int_packed(3); // handle 2
        bits.int_packed(16); // payloadBits
        bits.int_packed(5); // NetGUID 5, leaving eight bits unread
        bits.repeat(false, 8);
        bits.int_packed(4); // handle 3: a field the type does not have
        bits.int_packed(16); // payloadBits
        bits.int_packed(777); // two IntPacked bytes
        bits.int_packed(0); // element terminator
        bits.int_packed(0); // array terminator
        let (data, bit_count) = bits.finish();
        let mut stats = ArrayDecodeStats::default();

        let guids = decode_object_ref_array_with_stats(&data, bit_count, &mut stats);

        assert!(guids.is_empty(), "second field promoted: {guids:?}");
        assert_eq!(anomalies(&stats), [1, 0, 0, 0, 16], "{stats:?}");
    }

    /// The counters above must stay silent on well-formed arrays, or they
    /// would flag every `MultiContents` blob in the corpus.
    #[test]
    fn terminated_object_ref_arrays_report_no_anomaly() {
        // Two populated slots.
        let mut two = BitWriter::new();
        two.int_packed(2);
        push_one_closed_item(&mut two);
        two.int_packed(2); // encodedIndex -> index 1
        two.int_packed(3);
        two.int_packed(24);
        two.int_packed(25492); // three IntPacked bytes
        two.int_packed(0);
        two.int_packed(0); // array terminator
        // An empty array: count, then the terminator.
        let mut empty = BitWriter::new();
        empty.int_packed(0);
        empty.int_packed(0);

        for (bits, want) in [(two, vec![(0, 5), (1, 25492)]), (empty, vec![])] {
            let (data, bit_count) = bits.finish();
            let mut stats = ArrayDecodeStats::default();
            let guids = decode_object_ref_array_with_stats(&data, bit_count, &mut stats);
            assert_eq!(guids, want);
            assert_eq!(anomalies(&stats), [0; 5], "{stats:?}");
        }
    }

    /// A BitIo read failure (fewer than 8 bits left for an IntPacked read) must
    /// also count as an error rather than returning silently.
    #[test]
    fn read_failure_mid_stream_counts_error() {
        // elementCount=1, element 0 starts, its field header declares a handle,
        // then the stream ends with too few bits for the payloadBits IntPacked.
        let mut bits = BitWriter::new();
        bits.int_packed(1); // elementCount = 1
        bits.int_packed(1); // encodedIndex = 1 -> element 0
        bits.int_packed(4); // encodedHandle = 4 -> handle 3
        // Three stray bits: not enough for an IntPacked payloadBits read.
        bits.repeat(false, 3);

        let (data, bit_count) = bits.finish();
        let mut stats = ArrayDecodeStats::default();
        let _fields = decode_struct_array(&data, bit_count, None, &[], &mut stats);

        // One failed read, one counter: the array loop must not read the same
        // three bits as an index and fail again, nor leave them as residual.
        assert_eq!(anomalies(&stats), [1, 0, 0, 0, 0], "{stats:?}");
    }

    /// A nested array that leaves bits inside its own window must say so: the
    /// parent's `sub_reader` keeps the walk aligned either way.
    #[test]
    fn a_nested_array_that_leaves_bits_reports_them() {
        // Sixteen bits the inner array will never look at. Not 8: exactly
        // eight bits after a terminator are read as the optional trailer,
        // which is a different path with its own tests.
        let (data, bit_count) = nested(&one_element_with_handles(&[(7, 16)]), 16);
        let mut stats = ArrayDecodeStats::default();
        let fields = decode_struct_array(&data, bit_count, Some(&OUTER), &[], &mut stats);

        // The walk still succeeds and stays aligned -- that is the point.
        assert_eq!(fields.len(), 1, "{fields:?}");
        assert_eq!(fields[0].path, "[0].Reports[0]._h7");
        assert_eq!(stats.errors, 0, "alignment held, so this is not an error");
        assert_eq!(
            stats.unconsumed_nested_bits, 16,
            "the 16 abandoned bits must be tallied"
        );
    }

    /// An element that runs out of bits instead of reading its zero handle is
    /// a truncated element, and must be distinguishable from a complete one.
    #[test]
    fn an_element_ending_at_eof_is_not_a_clean_terminator() {
        // elementCount=1, element 0, one complete 32-bit field, then nothing:
        // no element terminator and no array terminator.
        let mut bits = BitWriter::new();
        bits.int_packed(1);
        bits.int_packed(1);
        bits.int_packed(4); // handle 3
        bits.int_packed(32);
        bits.repeat(true, 32);

        let (data, bit_count) = bits.finish();
        let mut stats = ArrayDecodeStats::default();
        let fields = decode_struct_array(&data, bit_count, None, &[], &mut stats);

        // The field itself is complete and is still emitted.
        assert_eq!(fields.len(), 1);
        // One EOF, counted once: by the element, not again by the array loop.
        assert_eq!(anomalies(&stats), [0, 0, 1, 0, 0], "{stats:?}");
    }

    /// The counter must NOT fire on a well-formed array that closes with both
    /// its terminators, or it would flag every clean blob in the corpus.
    #[test]
    fn a_cleanly_terminated_array_reports_no_implicit_termination() {
        let (data, bit_count) = one_element_with_handles(&[(3, 32)]).finish();
        let mut stats = ArrayDecodeStats::default();
        let _ = decode_struct_array(&data, bit_count, None, &[], &mut stats);

        assert_eq!(stats.implicit_terminations, 0, "{stats:?}");
        assert_eq!(stats.unconsumed_nested_bits, 0, "{stats:?}");
        assert_eq!(stats.errors, 0, "{stats:?}");
    }

    #[test]
    fn bits_after_the_root_array_terminator_are_tallied() {
        let mut bits = BitWriter::new();
        bits.int_packed(0);
        bits.int_packed(0);
        bits.repeat(true, 16);
        let (data, bit_count) = bits.finish();
        let mut stats = ArrayDecodeStats::default();

        let _ = decode_struct_array(&data, bit_count, None, &[], &mut stats);

        assert_eq!(stats.unconsumed_root_bits, 16, "{stats:?}");
    }

    #[test]
    fn an_out_of_range_element_index_is_malformed_not_a_clean_empty_array() {
        let mut bits = BitWriter::new();
        bits.int_packed(1); // one declared element
        bits.int_packed(2); // index 1 is outside [0, 1)
        bits.repeat(true, 16);
        let (data, bit_count) = bits.finish();
        let mut stats = ArrayDecodeStats::default();

        let fields = decode_struct_array(&data, bit_count, None, &[], &mut stats);

        assert!(fields.is_empty());
        assert_eq!(stats.errors, 1, "{stats:?}");
    }

    #[test]
    fn a_truncated_trailing_int_packed_is_not_accepted() {
        let mut bits = BitWriter::new();
        bits.int_packed(0); // zero elements
        bits.int_packed(0); // array terminator
        // Exactly eight trailing bits activates the optional IntPacked read,
        // but the continuation flag asks for a byte that is not present.
        bits.bits(0x01, 8);
        let (data, bit_count) = bits.finish();
        let mut stats = ArrayDecodeStats::default();

        let _ = decode_struct_array(&data, bit_count, None, &[], &mut stats);

        assert_eq!(stats.errors, 1, "{stats:?}");
    }

    /// Every byte that can follow the root terminator as the optional trailer
    /// moves exactly one counter, or none for the one byte that IS a
    /// terminator (the C# reference discards any of them).
    #[test]
    fn every_root_trailer_byte_is_accounted_for() {
        for byte in 0..=u8::MAX {
            // Zero elements, the index terminator, then the one trailer byte.
            let data = [0x00, 0x00, byte];
            let mut stats = ArrayDecodeStats::default();

            let fields = decode_struct_array(&data, 24, None, &[], &mut stats);

            assert!(fields.is_empty(), "byte {byte:#04x}");
            let (root_bits, errors) = match byte {
                // The terminator: consumed, nothing to report.
                0 => (0, 0),
                // A continuation bit asking for a byte past the window.
                b if b & 1 == 1 => (0, 1),
                // A complete, nonzero IntPacked: not a terminator.
                _ => (8, 0),
            };
            assert_eq!(
                (stats.unconsumed_root_bits, stats.errors),
                (root_bits, errors),
                "byte {byte:#04x}: {stats:?}"
            );
            assert_eq!(stats.unconsumed_nested_bits, 0, "byte {byte:#04x}");
            assert_eq!(stats.implicit_terminations, 0, "byte {byte:#04x}");
        }
    }

    /// One nested window: a one-element inner array, its terminator, then one
    /// trailing byte holding IntPacked `trailer`.
    fn nested_window_with_trailer(trailer: u32) -> (Vec<u8>, u32) {
        nested(one_element_with_handles(&[(7, 16)]).int_packed(trailer), 0)
    }

    /// A nonzero trailer inside a nested array's window is the same anomaly as
    /// at the root, and the nested residual tally is what must report it: the
    /// parent's window advanced past it, so nothing else would.
    #[test]
    fn a_nonzero_trailer_inside_a_nested_window_is_tallied() {
        let (data, bit_count) = nested_window_with_trailer(1);
        let mut stats = ArrayDecodeStats::default();
        let fields = decode_struct_array(&data, bit_count, Some(&OUTER), &[], &mut stats);

        // The leaf before the trailer is still emitted -- only a counter moves.
        assert_eq!(fields.len(), 1, "{fields:?}");
        assert_eq!(fields[0].path, "[0].Reports[0]._h7");
        assert_eq!(stats.unconsumed_nested_bits, 8, "{stats:?}");
        assert_eq!(stats.errors, 0, "{stats:?}");
        assert_eq!(stats.unconsumed_root_bits, 0, "{stats:?}");

        // The same window closed by a ZERO trailer is the shape real data
        // carries (`AffectedTargetsArray`, depth 2) and must stay silent.
        let (data, bit_count) = nested_window_with_trailer(0);
        let mut stats = ArrayDecodeStats::default();
        let fields = decode_struct_array(&data, bit_count, Some(&OUTER), &[], &mut stats);
        assert_eq!(fields.len(), 1, "{fields:?}");
        assert_eq!(stats.unconsumed_nested_bits, 0, "{stats:?}");
        assert_eq!(stats.errors, 0, "{stats:?}");
    }

    /// The zero trailer is real: an `Effects[]` window off the wire, pinned so a
    /// tighter trailer rule has to face it. Carved from an
    /// `AbilityCastsThisRound` parent of 13.01 replay `b9d2fac4`: 168 bits, one
    /// effect element (`Value`, `Time`, then an `AffectedTargetsArray` whose
    /// 24-bit window is `02 00 00` -- capacity one, no changed element, the zero
    /// trailer). All 240 trailers in the 1,018-replay audit corpus are this.
    #[test]
    fn a_real_zero_trailer_is_consumed_and_a_flipped_one_is_not() {
        let mut raw = [
            0x02, 0x02, 0x22, 0x40, 0x00, 0x00, 0x80, 0x3f, 0x24, 0x40, 0x20, 0xee, 0x8d, 0x41,
            0x26, 0x30, 0x02, 0x00, 0x00, 0x00, 0x00,
        ];
        let mut stats = ArrayDecodeStats::default();
        let out = decode_struct_array(&raw, 168, Some(&ABILITY_EFFECTS_SCHEMA), &[], &mut stats);

        let paths: Vec<&str> = out.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["[0].Value", "[0].Time"], "{paths:?}");
        assert_eq!(stats.errors, 0, "{stats:?}");
        assert_eq!(stats.unconsumed_nested_bits, 0, "{stats:?}");
        assert_eq!(stats.unconsumed_root_bits, 0, "{stats:?}");
        assert_eq!(stats.implicit_terminations, 0, "{stats:?}");

        // Byte 18 is that trailer. As IntPacked 1 it is no terminator, and
        // the eight bits must surface in the nested window they sit in.
        raw[18] = 0x02;
        let mut stats = ArrayDecodeStats::default();
        let out = decode_struct_array(&raw, 168, Some(&ABILITY_EFFECTS_SCHEMA), &[], &mut stats);
        assert_eq!(out.len(), 2, "the leaves before it are still emitted");
        assert_eq!(stats.unconsumed_nested_bits, 8, "{stats:?}");
        assert_eq!(stats.errors, 0, "{stats:?}");
    }

    /// `MultiContents` goes through the same trailer helper, unconditionally.
    #[test]
    fn a_nonzero_trailer_after_an_object_ref_array_is_tallied() {
        let mut stats = ArrayDecodeStats::default();
        let guids = decode_object_ref_array_with_stats(&[0x00, 0x00, 0x02], 24, &mut stats);
        assert!(guids.is_empty());
        assert_eq!(stats.unconsumed_root_bits, 8, "{stats:?}");
        assert_eq!(stats.errors, 0, "{stats:?}");

        let mut stats = ArrayDecodeStats::default();
        let guids = decode_object_ref_array_with_stats(&[0x00, 0x00, 0x00], 24, &mut stats);
        assert!(guids.is_empty());
        assert_eq!(stats.unconsumed_root_bits, 0, "{stats:?}");
        assert_eq!(stats.errors, 0, "{stats:?}");
    }

    /// `n` fields at handle 0 with an empty payload: headers, no payload bits.
    fn zero_width_fields(bits: &mut BitWriter, n: u32) {
        for _ in 0..n {
            bits.int_packed(1).int_packed(0);
        }
    }

    /// The zero handle arriving exactly at the field limit closes the element,
    /// so the next element is still read: reported as unclosed, it would be
    /// dropped with every counter still at zero.
    #[test]
    fn exactly_max_fields_followed_by_a_terminator_is_not_truncated() {
        let mut bits = BitWriter::new();
        bits.int_packed(2); // elementCount
        bits.int_packed(1); // encodedIndex -> index 0
        zero_width_fields(&mut bits, MAX_FIELDS_PER_ELEMENT);
        bits.int_packed(0); // element terminator, read by the limit check
        bits.int_packed(2); // encodedIndex -> index 1
        bits.int_packed(4).int_packed(32).repeat(true, 32); // handle 3, 32 bits
        bits.int_packed(0); // element terminator
        bits.int_packed(0); // array terminator
        let (data, bit_count) = bits.finish();
        let mut stats = ArrayDecodeStats::default();

        let fields = decode_struct_array(&data, bit_count, None, &[], &mut stats);

        assert_eq!(stats.elements_decoded, 2, "{stats:?}");
        let paths: Vec<&str> = fields.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["[1]._h3"]);
        assert_eq!(anomalies(&stats), [0; 5], "{stats:?}");
    }

    /// A 129th field is one truncation: the rest of the window, from that
    /// field's handle on, is kept as one raw leaf, and the array stops there.
    #[test]
    fn a_field_past_the_field_limit_is_one_truncation() {
        let mut bits = BitWriter::new();
        bits.int_packed(1); // elementCount
        bits.int_packed(1); // encodedIndex -> index 0
        zero_width_fields(&mut bits, MAX_FIELDS_PER_ELEMENT + 1);
        bits.int_packed(0); // element terminator
        bits.int_packed(0); // array terminator
        let (data, bit_count) = bits.finish();
        let mut stats = ArrayDecodeStats::default();

        let fields = decode_struct_array(&data, bit_count, None, &[], &mut stats);

        assert_eq!(anomalies(&stats), [0, 1, 0, 0, 0], "{stats:?}");
        // The 129th field's 16-bit header and both terminators: the limit
        // check read that header without consuming it.
        let raw: Vec<(&str, u32)> = fields
            .iter()
            .map(|f| (f.path.as_str(), f.bit_count))
            .collect();
        assert_eq!(raw, [("[0]._raw", 32)]);
    }

    /// A read that fails at the field limit is one error: the array loop must
    /// not read the same bits again as an index, nor leave them as residual.
    #[test]
    fn a_failed_read_at_the_field_limit_is_one_error() {
        let mut bits = BitWriter::new();
        bits.int_packed(1); // elementCount
        bits.int_packed(1); // encodedIndex -> index 0
        zero_width_fields(&mut bits, MAX_FIELDS_PER_ELEMENT);
        bits.repeat(true, 3); // too few bits for the next handle
        let (data, bit_count) = bits.finish();
        let mut stats = ArrayDecodeStats::default();

        let fields = decode_struct_array(&data, bit_count, None, &[], &mut stats);

        assert!(fields.is_empty(), "{fields:?}");
        assert_eq!(anomalies(&stats), [1, 0, 0, 0, 0], "{stats:?}");
    }

    /// The object-reference walker at the same limit: a zero handle closes the
    /// element and the next one is still read; a 129th field is one
    /// truncation and a failed read one error, and either stops the array.
    #[test]
    fn the_object_ref_walker_reads_the_next_int_packed_at_the_field_limit() {
        let element_at_the_limit = || {
            let mut bits = BitWriter::new();
            bits.int_packed(2); // elementCount
            bits.int_packed(1); // encodedIndex -> index 0
            zero_width_fields(&mut bits, MAX_FIELDS_PER_ELEMENT);
            bits
        };
        let mut closed = element_at_the_limit();
        closed.int_packed(0); // element terminator, read by the limit check
        closed.int_packed(2); // encodedIndex -> index 1
        closed.int_packed(3).int_packed(8).int_packed(5); // handle 2: NetGUID 5
        closed.int_packed(0).int_packed(0); // element and array terminators
        let mut over = element_at_the_limit();
        zero_width_fields(&mut over, 1); // a 129th field
        over.int_packed(0).int_packed(0);
        let mut failed = element_at_the_limit();
        failed.repeat(true, 3); // too few bits for the next handle

        for (name, bits, want, counters) in [
            ("closed", closed, vec![(1, 5)], [0; 5]),
            ("129th field", over, vec![], [0, 1, 0, 0, 0]),
            ("failed read", failed, vec![], [1, 0, 0, 0, 0]),
        ] {
            let (data, bit_count) = bits.finish();
            let mut stats = ArrayDecodeStats::default();
            let guids = decode_object_ref_array_with_stats(&data, bit_count, &mut stats);
            assert_eq!(guids, want, "{name}");
            assert_eq!(anomalies(&stats), counters, "{name}: {stats:?}");
        }
    }

    #[test]
    fn repeated_element_indices_cannot_bypass_the_element_work_limit() {
        let mut bits = BitWriter::new();
        bits.int_packed(1); // one declared slot
        for _ in 0..=MAX_ELEMENTS {
            bits.int_packed(1); // repeat index 0
            bits.int_packed(0); // empty element
        }
        bits.int_packed(0); // array terminator
        let (data, bit_count) = bits.finish();
        let mut stats = ArrayDecodeStats::default();

        let _ = decode_struct_array(&data, bit_count, None, &[], &mut stats);

        assert_eq!(stats.elements_decoded, u64::from(MAX_ELEMENTS));
        assert_eq!(stats.truncations, 1, "{stats:?}");
    }

    /// After `MAX_ELEMENTS` elements both walkers read the next IntPacked in
    /// the limit check: the index terminator, then the optional trailer as
    /// after any terminator; another index, one truncation; or a failed read,
    /// one error. Only bits after the terminator are root residual: the other
    /// two are one anomaly each, the struct walker keeping a truncated tail as
    /// a `._raw` leaf.
    #[test]
    fn both_walkers_read_the_next_int_packed_at_the_element_limit() {
        // (the tail after the last element as `bits(value, width)`, the struct
        // walker's counters and `._raw` leaf width, the object-ref walker's
        // counters)
        let cases = [
            (0x0000, 16, [0; 5], None, [0; 5]), // terminator, zero trailer
            (0x0200, 16, [0, 0, 0, 8, 0], None, [0, 0, 0, 8, 0]), // terminator, trailer 1
            (0x0100, 16, [1, 0, 0, 0, 0], None, [1, 0, 0, 0, 0]), // terminator, cut trailer
            (0x0002, 16, [0, 1, 0, 0, 0], Some(16), [0, 1, 0, 0, 0]), // one element too many
            (0b111, 3, [1, 0, 0, 0, 0], None, [1, 0, 0, 0, 0]), // too few bits to read
        ];
        for (tail, width, struct_counters, raw_width, object_ref_counters) in cases {
            let case = format!("tail {tail:#06x}/{width}");
            let mut bits = BitWriter::new();
            bits.int_packed(1); // one declared slot
            for _ in 0..MAX_ELEMENTS {
                bits.int_packed(1).int_packed(0); // index 0 again, empty element
            }
            bits.bits(tail, width);
            let (data, bit_count) = bits.finish();

            let mut stats = ArrayDecodeStats::default();
            let fields = decode_struct_array(&data, bit_count, None, &[], &mut stats);
            let raw: Vec<(&str, u32)> = fields
                .iter()
                .map(|f| (f.path.as_str(), f.bit_count))
                .collect();
            let want: Vec<(&str, u32)> = raw_width.map(|n| ("._raw", n)).into_iter().collect();
            assert_eq!(raw, want, "{case}");
            assert_eq!(stats.elements_decoded, u64::from(MAX_ELEMENTS), "{case}");
            assert_eq!(anomalies(&stats), struct_counters, "{case}: {stats:?}");

            let mut stats = ArrayDecodeStats::default();
            let guids = decode_object_ref_array_with_stats(&data, bit_count, &mut stats);
            assert!(guids.is_empty(), "{case}: {guids:?}");
            assert_eq!(stats.elements_decoded, u64::from(MAX_ELEMENTS), "{case}");
            assert_eq!(anomalies(&stats), object_ref_counters, "{case}: {stats:?}");
        }
    }

    /// The other array-level stops that move `errors` or `truncations` are one
    /// anomaly each in both walkers as well: a failed element-count read, a
    /// declared count over `MAX_ELEMENTS` (the struct walker keeps the rest as
    /// a `._raw` leaf), a failed index read and an index out of range. The
    /// tail is neither root residual nor, in a nested window, nested residual,
    /// and what was decoded before the stop is kept.
    #[test]
    fn an_array_level_stop_counts_once_in_both_walkers() {
        let mut count_cut = BitWriter::new();
        count_cut.repeat(true, 3); // too few bits for the element count
        let mut over_limit = BitWriter::new();
        over_limit.int_packed(MAX_ELEMENTS + 1).repeat(true, 16);
        let mut index_cut = BitWriter::new();
        index_cut.int_packed(2); // elementCount
        push_one_closed_item(&mut index_cut);
        index_cut.repeat(true, 3); // too few bits for the next index
        let mut out_of_range = BitWriter::new();
        out_of_range.int_packed(1).int_packed(2).repeat(true, 16); // index 1 of 1

        // (case, bits, both walkers' counters, the struct walker's leaves, the
        // object-ref walker's items)
        let cases = [
            ("count read", count_cut, [1, 0, 0, 0, 0], vec![], vec![]),
            (
                "count over the limit",
                over_limit,
                [0, 1, 0, 0, 0],
                vec![("._raw", 16)],
                vec![],
            ),
            (
                "index read",
                index_cut,
                [1, 0, 0, 0, 0],
                vec![("[0]._h2", 8)],
                vec![(0, 5)],
            ),
            (
                "index out of range",
                out_of_range,
                [1, 0, 0, 0, 0],
                vec![],
                vec![],
            ),
        ];
        for (case, bits, counters, leaves, items) in cases {
            let (data, bit_count) = bits.finish();
            let mut stats = ArrayDecodeStats::default();
            let fields = decode_struct_array(&data, bit_count, None, &[], &mut stats);
            let got: Vec<(&str, u32)> = fields
                .iter()
                .map(|f| (f.path.as_str(), f.bit_count))
                .collect();
            assert_eq!(got, leaves, "{case}");
            assert_eq!(anomalies(&stats), counters, "{case}: {stats:?}");

            let mut stats = ArrayDecodeStats::default();
            let guids = decode_object_ref_array_with_stats(&data, bit_count, &mut stats);
            assert_eq!(guids, items, "{case}");
            assert_eq!(anomalies(&stats), counters, "{case}: {stats:?}");
        }

        // A nested array whose index read fails: one error, and the outer walk
        // still reads its own element and array terminators.
        let (data, bit_count) = nested(BitWriter::new().int_packed(1), 3);
        let mut stats = ArrayDecodeStats::default();
        let fields = decode_struct_array(&data, bit_count, Some(&OUTER), &[], &mut stats);
        assert!(fields.is_empty(), "{fields:?}");
        assert_eq!(anomalies(&stats), [1, 0, 0, 0, 0], "{stats:?}");
    }

    /// `AbilityCastsThisRound[].Effects[]` is a nested array only its schema
    /// reveals. The payload is real, the smallest carrying an
    /// `AffectedTargetsArray`: 128 bits, one effect element whose only field is
    /// handle 18, itself an 80-bit array. It also pins the residual tallies at
    /// zero on REAL data, not only on hand-built fixtures.
    #[test]
    fn the_ability_effects_array_descends_into_its_targets() {
        let raw = [
            0x02, 0x02, 0x26, 0xa0, 0x04, 0x04, 0x2a, 0x40, 0x7e, 0x16, 0x12, 0x3f, 0x00, 0x00,
            0x00, 0x00,
        ];
        let mut stats = ArrayDecodeStats::default();
        let out = decode_struct_array(&raw, 128, Some(&ABILITY_EFFECTS_SCHEMA), &[], &mut stats);
        assert_eq!(stats.errors, 0, "{stats:?}");
        assert_eq!(stats.unconsumed_nested_bits, 0, "{stats:?}");
        assert_eq!(stats.implicit_terminations, 0, "{stats:?}");

        // Only `Value` appears, not `AffectedPlayer`: replication is per
        // property, so an element re-sent because one member changed carries
        // only that member (carry the last value forward per element index).
        let paths: Vec<&str> = out.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["[0].AffectedTargetsArray[1].Value"], "{paths:?}");
    }

    /// The nesting the schema declares, checked against the replay's own
    /// declaration rather than against itself: `Effects` is handle 13 on a cast,
    /// its members are 14..18, and `AffectedTargetsArray` (18) holds 19 and 20.
    #[test]
    fn the_ability_effects_schema_matches_the_declared_handles() {
        assert!(ABILITY_CASTS_SCHEMA.sub_array(13).is_some(), "Effects");
        let effects = ABILITY_CASTS_SCHEMA.sub_array(13).unwrap();
        for (handle, name) in [
            (14, "Statistic"),
            (15, "LocalizedStat"),
            (16, "Value"),
            (17, "Time"),
            (18, "AffectedTargetsArray"),
        ] {
            assert_eq!(effects.field_name(handle), Some(name), "handle {handle}");
        }
        let targets = effects.sub_array(18).expect("AffectedTargetsArray");
        assert_eq!(targets.field_name(19), Some("AffectedPlayer"));
        assert_eq!(targets.field_name(20), Some("Value"));
        assert!(targets.sub_array(19).is_none(), "leaves stay leaves");
    }

    /// Every field, a distinct value, so a field summed into the wrong target
    /// or not summed at all fails here instead of cancelling out.
    fn distinct_stats(base: u64) -> ArrayDecodeStats {
        ArrayDecodeStats {
            elements_decoded: base + 1,
            fields_emitted: base + 2,
            truncations: base + 3,
            errors: base + 4,
            unconsumed_nested_bits: base + 5,
            unconsumed_root_bits: base + 6,
            implicit_terminations: base + 7,
        }
    }

    /// `merge_from` sums each counter into its own total.
    #[test]
    fn merge_from_sums_every_field_into_its_own_target() {
        let mut total = distinct_stats(0);
        total.merge_from(&distinct_stats(100));
        assert_eq!(total.elements_decoded, 1 + 101);
        assert_eq!(total.fields_emitted, 2 + 102);
        assert_eq!(total.truncations, 3 + 103);
        assert_eq!(total.errors, 4 + 104);
        assert_eq!(total.unconsumed_nested_bits, 5 + 105);
        assert_eq!(total.unconsumed_root_bits, 6 + 106);
        assert_eq!(total.implicit_terminations, 7 + 107);
    }

    /// `is_clean` gates whether a measured route may emit typed children from
    /// a walk. Each of the five failure counters must refuse on its own; the
    /// two work counters must not.
    #[test]
    fn is_clean_refuses_each_failure_counter_alone_and_ignores_work_counters() {
        let work_only = ArrayDecodeStats {
            elements_decoded: 3,
            fields_emitted: 9,
            ..ArrayDecodeStats::default()
        };
        assert!(work_only.is_clean(), "work counters are not failures");
        assert!(ArrayDecodeStats::default().is_clean());

        let one_failure_each = [
            (
                "truncations",
                ArrayDecodeStats {
                    truncations: 1,
                    ..work_only.clone()
                },
            ),
            (
                "errors",
                ArrayDecodeStats {
                    errors: 1,
                    ..work_only.clone()
                },
            ),
            (
                "unconsumed_nested_bits",
                ArrayDecodeStats {
                    unconsumed_nested_bits: 1,
                    ..work_only.clone()
                },
            ),
            (
                "unconsumed_root_bits",
                ArrayDecodeStats {
                    unconsumed_root_bits: 1,
                    ..work_only.clone()
                },
            ),
            (
                "implicit_terminations",
                ArrayDecodeStats {
                    implicit_terminations: 1,
                    ..work_only.clone()
                },
            ),
        ];
        for (name, stats) in one_failure_each {
            assert!(!stats.is_clean(), "{name} alone must make a walk unclean");
        }
    }

    /// The life-change array walks into its four members, on actual payloads
    /// from a 13.02 replay. The local handles differ per RPC (three schemas),
    /// and `MulticastNotifyHeal` names its parameter `LifeChangeBySection`, so a
    /// dispatch on the array's name alone would miss two of the five functions.
    #[test]
    fn the_life_change_array_walks_into_its_members() {
        // MulticastNotifyDamage_Point.LifeChangeEvents, one element.
        let damage: Vec<u8> = vec![
            0x02, 0x02, 0x16, 0x20, 0xE5, 0x3E, 0x18, 0x40, 0x00, 0x80, 0x0E, 0x44, 0x1A, 0x40,
            0x00, 0x00, 0xF0, 0xC1, 0x1C, 0x02, 0x01, 0x00, 0x00,
        ];
        // MulticastNotifyHeal.LifeChangeBySection, one element.
        let heal: Vec<u8> = vec![
            0x02, 0x02, 0x06, 0x20, 0x29, 0x14, 0x08, 0x40, 0xFF, 0x0F, 0x08, 0x42, 0x0A, 0x40,
            0x00, 0x00, 0x80, 0x3F, 0x0C, 0x02, 0x01, 0x00, 0x00,
        ];

        for (label, raw, bits, schema, want) in [
            (
                "damage",
                &damage,
                177u32,
                &LIFE_CHANGE_DAMAGE_SCHEMA,
                [
                    "ChangedComponent",
                    "LifeResult",
                    "DeltaLife",
                    "bAliveAfterChange",
                ],
            ),
            (
                "heal",
                &heal,
                177,
                &LIFE_CHANGE_BY_SECTION_SCHEMA,
                [
                    "ChangedComponent",
                    "LifeResult",
                    "DeltaLife",
                    "bAliveAfterChange",
                ],
            ),
        ] {
            let mut stats = ArrayDecodeStats::default();
            let fields = decode_struct_array(raw, bits, Some(schema), &[], &mut stats);
            assert_eq!(stats.errors, 0, "{label} decoded with errors");
            assert_eq!(fields.len(), 4, "{label}: {fields:?}");
            for (field, name) in fields.iter().zip(want) {
                assert!(
                    field.path.ends_with(name),
                    "{label}: {} vs {name}",
                    field.path
                );
            }
        }

        // The section schema numbers from 1, so it must not be interchangeable.
        let mut stats = ArrayDecodeStats::default();
        let wrong = decode_struct_array(
            &damage,
            177,
            Some(&LIFE_CHANGE_SECTION_SCHEMA),
            &[],
            &mut stats,
        );
        assert!(
            wrong.iter().all(|f| !f.path.ends_with("LifeResult")),
            "the wrong schema must not happen to name the members: {wrong:?}"
        );
    }
}
