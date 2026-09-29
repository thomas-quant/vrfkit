//! Net GUID loading -- `InternalLoadObject` recursive reader.
//!
//! Unreal's wire form of an object reference: a GUID, then, when its export
//! flags carry a path, that path and possibly an outer GUID, recursively. The
//! paths go to the caller's [`GuidPathSink`], which owns the NetGuidCache.

use vrf_bitio::BitReader;

use crate::error::{NetError, Result};
use crate::types::{ExportFlags, MAX_NET_GUID_RECURSION, NetworkGuid};

/// Callback invoked when a net GUID's path is decoded from the stream.
pub trait GuidPathSink {
    /// A GUID path was read from the wire.
    fn register_path(&mut self, guid: u32, path: &str, outer_guid: NetworkGuid);

    /// The path this GUID is known by in the receiver's NetGuidCache, which
    /// decides the net-player-index byte. Answer from the cache, not from
    /// `register_path` calls: paths reach it by more routes. The default
    /// `None` never consumes that byte.
    fn path_for_guid(&self, _guid: u32) -> Option<&str> {
        None
    }
}

/// Read a net GUID reference (and any associated export data) from the stream.
///
/// ```text
/// Wire layout:
///   net_guid           : IntPacked (u32)
///   if guid == default || is_exporting:
///     export_flags     : u8
///   if HasPath in export_flags:
///     outer_guid       : InternalLoadObject (recursive)
///     path_name        : FString
///     if HasNetworkChecksum:
///       checksum       : u32
/// ```
///
/// `is_exporting` is true inside a package-map export bunch; in content-block
/// headers only the default GUID (1) carries inline path data.
pub fn internal_load_object(
    reader: &mut BitReader<'_>,
    is_exporting: bool,
    depth: u32,
    sink: &mut dyn GuidPathSink,
) -> Result<NetworkGuid> {
    if depth >= MAX_NET_GUID_RECURSION {
        return Err(NetError::GuidRecursionLimit { depth });
    }

    let guid = NetworkGuid(reader.read_int_packed()?);
    if !guid.is_valid() {
        return Ok(guid);
    }

    let flags = if guid.is_default() || is_exporting {
        ExportFlags(reader.read_u8()?)
    } else {
        ExportFlags::NONE
    };

    if !flags.contains(ExportFlags::HAS_PATH) {
        return Ok(guid);
    }

    let outer_guid = internal_load_object(reader, is_exporting, depth + 1, sink)?;

    // Cap the FString at 4096 bytes to reject a corrupt length early.
    let path = reader.read_fstring(4096)?;

    if flags.contains(ExportFlags::HAS_NETWORK_CHECKSUM) {
        let _checksum = reader.read_u32()?;
    }

    sink.register_path(guid.0, &path, outer_guid);
    Ok(guid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_bits::{BitWrite, pack};

    #[derive(Default)]
    struct VecSink(Vec<(u32, String, NetworkGuid)>);

    impl GuidPathSink for VecSink {
        fn register_path(&mut self, guid: u32, path: &str, outer: NetworkGuid) {
            self.0.push((guid, path.to_owned(), outer));
        }
    }

    /// Build a minimal InternalLoadObject payload for a non-exporting read.
    fn build_simple_guid(guid: u32) -> Vec<u8> {
        let mut bits: Vec<bool> = Vec::new();
        bits.int_packed(guid);
        pack(&bits)
    }

    /// Build an InternalLoadObject with path export.
    fn build_export_guid(guid: u32, path: &str, outer_guid: u32) -> Vec<u8> {
        let mut bits: Vec<bool> = Vec::new();
        bits.int_packed(guid);
        // export flags = HasPath (0x01)
        bits.u8(0x01);
        // outer guid (simple, no path)
        bits.int_packed(outer_guid);
        // FString: length (i32) + bytes + null
        let path_bytes = format!("{}\0", path);
        let len = path_bytes.len() as i32;
        for b in len.to_le_bytes() {
            bits.u8(b);
        }
        for b in path_bytes.bytes() {
            bits.u8(b);
        }
        pack(&bits)
    }

    #[test]
    fn zero_guid_is_invalid_and_consumed() {
        let data = build_simple_guid(0);
        let mut reader = BitReader::new(&data);
        let mut sink = VecSink::default();
        let guid = internal_load_object(&mut reader, false, 0, &mut sink).unwrap();
        assert!(!guid.is_valid());
        assert!(sink.0.is_empty());
    }

    #[test]
    fn simple_guid_no_path() {
        let data = build_simple_guid(42);
        let mut reader = BitReader::new(&data);
        let mut sink = VecSink::default();
        let guid = internal_load_object(&mut reader, false, 0, &mut sink).unwrap();
        assert_eq!(guid.0, 42);
        assert!(sink.0.is_empty()); // No path since not default and not exporting
    }

    #[test]
    fn exporting_guid_with_path() {
        let data = build_export_guid(18, "/Game/Test.Test_C", 0);
        let mut reader = BitReader::new(&data);
        let mut sink = VecSink::default();
        let guid = internal_load_object(&mut reader, true, 0, &mut sink).unwrap();
        assert_eq!(guid.0, 18);
        assert_eq!(sink.0.len(), 1);
        assert_eq!(sink.0[0].0, 18);
        assert_eq!(sink.0[0].1, "/Game/Test.Test_C");
        assert_eq!(sink.0[0].2, NetworkGuid(0));
    }
}
