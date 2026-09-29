//! The four fixed sections that precede a DemoFrame's packet loop, one reader
//! per section. Three exist only to be skipped, and a miscounted one
//! desynchronises the whole frame instead of failing.
//!
//! Level names are read and validated, not blind-skipped: 29 names over the
//! reference replay's 226,190 frames are not worth a skip path
//! (docs/PERFORMANCE_NOTES.md#measured-shape-on-real-replays).

use vrf_bitio::BitReader;
use vrf_schema::NetGuidCache;

use crate::error::FrameError;

/// Cap on a level name's FString.
const MAX_FSTRING_BYTES: i64 = 1024 * 1024;

/// ExportData (`ExportDataReader.Read()`): net field exports, then export
/// GUIDs, byte-aligned, into `cache`. The only section that mutates state; the
/// accumulated schema reaches the summary as `Export groups`.
pub(crate) fn read_export_data(
    reader: &mut BitReader<'_>,
    cache: &mut NetGuidCache,
) -> Result<(), FrameError> {
    // Bound on purpose (`#[must_use]` misses a bare `?`); these per-frame counts reach no counter.
    let _exports = vrf_schema::read_net_field_exports(reader, cache)?;
    let _guids = vrf_schema::read_export_guids(reader, cache)?;
    Ok(())
}

/// StreamingLevelFixes: level names, compact or verbose.
pub(crate) fn read_streaming_level_fixes(
    reader: &mut BitReader<'_>,
    has_streaming_fixes: bool,
) -> Result<(), FrameError> {
    let num_levels = reader.read_int_packed()?;

    if has_streaming_fixes {
        // Compact form: FString names, then a u64 externalOffset.
        for _ in 0..num_levels {
            let _ = reader.read_fstring(MAX_FSTRING_BYTES)?;
        }
        let _ = reader.read_u64()?;
    } else {
        // Verbose form: packageName, packageNameToLoad and an FTransform
        // (rotation 4 + translation 3 + scale 3 f32 = 40 bytes) per entry. No
        // measured replay takes it; see the flag note in the crate doc.
        for _ in 0..num_levels {
            let _ = reader.read_fstring(MAX_FSTRING_BYTES)?;
            let _ = reader.read_fstring(MAX_FSTRING_BYTES)?;
            reader.skip_bits(40 * 8)?;
        }
    }

    Ok(())
}

/// ExternalData (`PlaybackPacketReader.ReadExternalData()`): numBits, netGuid
/// and payload until numBits is 0. Returns the `(blobs, bytes)` skipped, the
/// only thing that moves if a build starts sending external data.
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

/// GameSpecificFrameData (`GameSpecificFrameDataReader.Read()`): when the flag
/// is set, a u64 byte count and that many bytes. Returns the bytes skipped.
pub(crate) fn read_game_specific_frame_data(
    reader: &mut BitReader<'_>,
    has_game_specific: bool,
) -> Result<u64, FrameError> {
    if !has_game_specific {
        return Ok(0);
    }
    let skip_offset = reader.read_u64()?;
    // A raw wire u64: a wrapping `* 8` would silently skip the wrong amount.
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
        // (1 << 61) + 1 wraps to 8 bits under `* 8`, which the trailing byte
        // would satisfy: a silent Ok without `checked_mul`.
        let bytes: [u8; 9] = [0x01, 0, 0, 0, 0, 0, 0, 0x20, 0xFF];
        let mut reader = BitReader::new(&bytes);
        let result = read_game_specific_frame_data(&mut reader, true);
        assert!(
            result.is_err(),
            "an overflowing skip offset must error, not wrap to 8 and succeed"
        );
    }
}
