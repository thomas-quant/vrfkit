//! Net GUID references (`InternalLoadObject`), read by vrf-schema's
//! [`load_object`](vrf_schema::load_object). The paths go to the caller's
//! [`GuidPathSink`], which owns the NetGuidCache.

use vrf_bitio::BitReader;
use vrf_schema::SchemaError;

use crate::error::{NetError, Result};
use crate::types::NetworkGuid;

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

/// Read a net GUID reference, path cap 4096 bytes, and register its paths.
/// `is_exporting` is true inside a package-map export bunch; in content-block
/// headers only the default GUID (1) carries inline path data.
pub fn internal_load_object(
    reader: &mut BitReader<'_>,
    is_exporting: bool,
    depth: u32,
    sink: &mut dyn GuidPathSink,
) -> Result<NetworkGuid> {
    let mut register = |guid, path: String, outer| sink.register_path(guid, &path, outer);
    vrf_schema::load_object(reader, is_exporting, 4096, depth, &mut register).map_err(|e| match e {
        SchemaError::Bitio(e) => NetError::Bit(e),
        SchemaError::RecursionLimitExceeded { limit } => NetError::GuidRecursionLimit {
            depth: limit.max(depth),
        },
        e => unreachable!("load_object fails only on a read or its depth limit, not {e}"),
    })
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

    /// Sixteen nested HasPath GUIDs reach the limit before a 17th is read.
    #[test]
    fn nesting_to_the_depth_limit_is_an_error() {
        let mut bits: Vec<bool> = Vec::new();
        for _ in 0..16 {
            bits.int_packed(2).u8(0x01);
        }
        let data = pack(&bits);
        let mut sink = VecSink::default();
        let result = internal_load_object(&mut BitReader::new(&data), true, 0, &mut sink);
        assert_eq!(result, Err(NetError::GuidRecursionLimit { depth: 16 }));
        assert!(sink.0.is_empty());
    }
}
