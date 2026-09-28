//! The four fixed sections that precede a DemoFrame's packet loop.
//!
//! Each is a distinct sub-grammar with its own source in the C# reference, and
//! three of the four exist only to be *skipped* correctly -- getting a byte
//! count wrong here desynchronises the whole frame rather than failing, so each
//! reader is kept separate and named after the reference type it mirrors.
//!
//! Measured frame/allocation counts: docs/PERFORMANCE_NOTES.md#measured-shape-on-real-replays. Level names are still read and validated rather than blind-skipped.

use vrf_bitio::BitReader;
use vrf_schema::NetGuidCache;

use crate::error::FrameError;

/// Maximum sane FString bytes for a level name. The module doc explains why
/// these are read and validated rather than skipped.
const MAX_FSTRING_BYTES: i64 = 1024 * 1024;

/// ExportData: read net field exports + export GUIDs into the cache.
///
/// Source: `ExportDataReader.Read()` -- calls `ReadNetFieldExports()` then
/// `ReadExportGuids()`. Both are byte-aligned (FBinaryArchive) reads.
///
/// This is the only section that mutates state: the schema a replay declares
/// arrives here, incrementally, frame by frame, and its group total reaches
/// the summary as `Export groups`.
pub(crate) fn read_export_data(
    reader: &mut BitReader<'_>,
    cache: &mut NetGuidCache,
) -> Result<(), FrameError> {
    // Bound on purpose (`#[must_use]` misses a bare `?`); these per-frame counts reach no counter.
    let _exports = vrf_schema::read_net_field_exports(reader, cache)?;
    let _guids = vrf_schema::read_export_guids(reader, cache)?;
    Ok(())
}

/// StreamingLevelFixes: skip level names (either compact or verbose form).
///
/// Source: `StreamingLevelFixesReader.cs`
pub(crate) fn read_streaming_level_fixes(
    reader: &mut BitReader<'_>,
    has_streaming_fixes: bool,
) -> Result<(), FrameError> {
    let num_levels = reader.read_int_packed()?;

    if has_streaming_fixes {
        // Compact form: just FString names + a u64 externalOffset.
        for _ in 0..num_levels {
            let _ = reader.read_fstring(MAX_FSTRING_BYTES)?;
        }
        let _ = reader.read_u64()?;
    } else {
        // Verbose form: packageName + packageNameToLoad + FTransform per entry.
        // The C# code calls `_archive.ReadFTransform()`, which in their
        // implementation reads rotation(4 x f32) + translation(3 x f32) +
        // scale(3 x f32) = 10 x f32 = 40 bytes.
        //
        // No corpus replay takes this branch: all 215 set HasStreamingFixes.
        for _ in 0..num_levels {
            let _ = reader.read_fstring(MAX_FSTRING_BYTES)?;
            let _ = reader.read_fstring(MAX_FSTRING_BYTES)?;
            reader.skip_bits(40 * 8)?;
        }
    }

    Ok(())
}

/// ExternalData: loop reading numBits + netGuid + skip, until numBits == 0.
///
/// Source: `PlaybackPacketReader.ReadExternalData()`
///
/// Returns `(blobs, bytes)` skipped. The reference discards them too, and the
/// length prefix keeps the frame aligned whatever they hold, so this count is
/// the only thing that moves if a build starts sending external data.
pub(crate) fn read_external_data(reader: &mut BitReader<'_>) -> Result<(u64, u64), FrameError> {
    let mut blobs = 0u64;
    let mut bytes = 0u64;
    loop {
        let num_bits = reader.read_int_packed()?;
        if num_bits == 0 {
            return Ok((blobs, bytes));
        }
        let _net_guid = reader.read_int_packed()?;
        let byte_count = u64::from(num_bits.div_ceil(8));
        reader.skip_bits(byte_count * 8)?;
        blobs += 1;
        bytes += byte_count;
    }
}

/// GameSpecificFrameData: optionally read a u64 skip-offset and skip that many bytes.
///
/// Source: `GameSpecificFrameDataReader.Read()`
///
/// See the "Flag semantics" table in lib.rs's module doc for which flag
/// enables this section and its measured absence in the corpus.
///
/// Returns the bytes skipped: 0 when the flag is off or the offset is 0.
pub(crate) fn read_game_specific_frame_data(
    reader: &mut BitReader<'_>,
    has_game_specific: bool,
) -> Result<u64, FrameError> {
    if !has_game_specific {
        return Ok(0);
    }
    let skip_offset = reader.read_u64()?;
    if skip_offset == 0 {
        return Ok(0);
    }
    // `skip_offset` is a raw u64 from the wire; `* 8` is plain wrapping
    // multiplication, so a large value silently wraps to a small skip and
    // desynchronises the frame. The flag is unset on every known replay, but a
    // malformed/large offset must fail loudly rather than wrap.
    let skip_bits = skip_offset.checked_mul(8).ok_or_else(|| {
        FrameError::Bit(format!(
            "game-specific skip offset overflows: {skip_offset}"
        ))
    })?;
    reader.skip_bits(skip_bits)?;
    Ok(skip_offset)
}

#[cfg(test)]
mod tests {
    use super::read_game_specific_frame_data;
    use vrf_bitio::BitReader;

    #[test]
    fn a_huge_game_specific_skip_offset_errors_instead_of_wrapping() {
        // u64 (1 << 61) + 1: `* 8` overflows u64 and wraps to 8. A trailing
        // byte leaves 8 bits after the u64 read, so the wrapped `skip_bits(8)`
        // would SUCCEED and the bug is a silent Ok. checked_mul must reject it.
        let bytes: [u8; 9] = [0x01, 0, 0, 0, 0, 0, 0, 0x20, 0xFF];
        let mut reader = BitReader::new(&bytes);
        let result = read_game_specific_frame_data(&mut reader, true);
        assert!(
            result.is_err(),
            "an overflowing skip offset must error, not wrap to 8 and succeed"
        );
    }
}
