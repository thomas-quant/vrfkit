//! Export wiring: one effect blob in, one JSON array out.

use core::fmt::Write as _;

use super::elements::{
    decode_effect_floats_at, decode_effect_objects_at, decode_effect_vectors_at,
};
use super::framing::{new_blob_reader, scan_element_handles};
use super::{EffectArrayKind, EffectBlobError, EffectHandles, Result};

/// Decode one effect-array blob and render it as a JSON array.
///
/// Each element becomes `{"tag":<u32|null>,"value":<value|null>}`, the value a
/// number for [`EffectArrayKind::Float`] and [`EffectArrayKind::Object`] and
/// `{"x":..,"y":..,"z":..}` for [`EffectArrayKind::Vector`]. Unpopulated slots
/// keep their position, so the JSON index is the wire index. `raw` is the
/// parameter payload as `fields.parquet` stores it; `bit_count` is its declared
/// `payload_bits`, **not** `raw.len() * 8`: the padding is not data
/// (`docs/archive/PROJECT_STATUS.md` 12-D).
///
/// # Errors
/// [`EffectBlobError`] if the payload is not a well-formed array of this kind,
/// does not consume its window, or holds a float JSON cannot represent. The
/// caller keeps the raw bits and counts the failure.
pub fn decode_effect_blob_json(
    kind: EffectArrayKind,
    raw: &[u8],
    bit_count: u32,
) -> Result<String> {
    // Which handles does *this* function use? `None` means no element carries
    // a field, so any pair decodes the blob identically.
    let handles = scan_element_handles(raw, bit_count)?.unwrap_or(EffectHandles::from_base(0));
    let mut reader = new_blob_reader(raw, bit_count)?;

    // Not pre-reserved: sizing from the element count measured no faster end
    // to end and cost resident memory in the export's row buffer.
    let mut out = String::new();
    match kind {
        EffectArrayKind::Float => {
            let elements = decode_effect_floats_at(&mut reader, handles)?;
            let pairs = elements.iter().map(|e| (e.tag_index, e.value));
            push_elements(&mut out, pairs, |out, index, v| {
                if !v.is_finite() {
                    return Err(EffectBlobError::NonFiniteFloat { index });
                }
                push_json_f64(out, f64::from(v));
                Ok(())
            })?;
        }
        EffectArrayKind::Object => {
            let elements = decode_effect_objects_at(&mut reader, handles)?;
            let pairs = elements.iter().map(|e| (e.tag_index, e.value));
            push_elements(&mut out, pairs, |out, _, v| {
                let _ = write!(out, "{v}");
                Ok(())
            })?;
        }
        EffectArrayKind::Vector => {
            let elements = decode_effect_vectors_at(&mut reader, handles)?;
            let pairs = elements.iter().map(|e| (e.tag_index, e.value));
            push_elements(&mut out, pairs, |out, index, v| {
                if !(v.x.is_finite() && v.y.is_finite() && v.z.is_finite()) {
                    return Err(EffectBlobError::NonFiniteFloat { index });
                }
                out.push_str("{\"x\":");
                push_json_f64(out, v.x);
                out.push_str(",\"y\":");
                push_json_f64(out, v.y);
                out.push_str(",\"z\":");
                push_json_f64(out, v.z);
                out.push('}');
                Ok(())
            })?;
        }
    }

    // Checked after the decode, which stops at the terminator by design.
    // Every remaining bit counts, a sub-byte tail included: `bit_count` is the
    // declared payload, so four unread bits are as wrong as forty.
    let remaining = reader.bits_remaining();
    if remaining > 0 {
        return Err(EffectBlobError::ResidualBits { remaining });
    }

    Ok(out)
}

/// Write `elements` as comma-separated `{"tag":..,"value":..}` objects in a
/// JSON array; `push_value` renders a present value, an absent one is `null`.
/// One loop for the three element types, so their framing cannot drift.
fn push_elements<V>(
    out: &mut String,
    elements: impl Iterator<Item = (Option<u32>, Option<V>)>,
    mut push_value: impl FnMut(&mut String, usize, V) -> Result<()>,
) -> Result<()> {
    out.push('[');
    for (index, (tag_index, value)) in elements.enumerate() {
        if index > 0 {
            out.push(',');
        }
        push_tag(out, tag_index);
        match value {
            Some(v) => push_value(out, index, v)?,
            None => out.push_str("null"),
        }
        out.push('}');
    }
    out.push(']');
    Ok(())
}

/// Open one element object and write its `tag` member. A hand-written
/// `u32` writer (here and for the object value, ~128,000 tags a replay)
/// measured neutral in an interleaved A/B of the whole export -- the time is in
/// `push_json_f64` -- so `write!` stayed.
fn push_tag(out: &mut String, tag_index: Option<u32>) {
    match tag_index {
        Some(t) => {
            let _ = write!(out, "{{\"tag\":{t},\"value\":");
        }
        None => out.push_str("{\"tag\":null,\"value\":"),
    }
}

/// Append a JSON number for a finite `f64`: `Display` is the shortest
/// round-trip form, always valid JSON (`1.0` renders `1`). Deliberately
/// `write!`: the faster shortest-float printers write large magnitudes as
/// `1E20` where Rust writes `100000000000000000000`, and the export oracle pins
/// these bytes.
fn push_json_f64(out: &mut String, v: f64) {
    // Writing into a String is infallible.
    let _ = write!(out, "{v}");
}
