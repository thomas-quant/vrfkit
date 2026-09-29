//! Wire-format readers for net field exports and export GUIDs.
//!
//! These functions consume bytes from a [`vrf_bitio::BitReader`] using the exact
//! layout that Unreal Engine's `ExportDataReader` writes:
//!
//! ## `ReadNetFieldExports` byte layout (byte-aligned, not bit-aligned)
//!
//! ```text
//! numLayoutCmdExports: IntPacked
//! for each export:
//!   pathNameIndex:  IntPacked
//!   isExported:    IntPacked (1 = new group, 0 = reference existing, else an error)
//!   if isExported:
//!     pathName:    FString (i32 length + UTF-8/UTF-16 bytes)
//!     numExports:  IntPacked (declared field-slot count)
//!   isFieldExported: u8 boolean (1 byte, not 1 bit -- this is FBinaryArchive)
//!   if isFieldExported:
//!     handle:             IntPacked
//!     compatibleChecksum: u32 (little-endian, 4 bytes)
//!     name:               FName (isHardcoded: u8 bool, then FString + i32 number)
//! ```
//!
//! ## `ReadExportGuids` byte layout
//!
//! ```text
//! numGuids: IntPacked
//! for each guid:
//!   payloadSize: i32 (little-endian)
//!   payload[payloadSize]:
//!     netGuid:     IntPacked
//!     exportFlags: u8 (if guid is default or isExportingNetGuidBunch)
//!     if HasPath:
//!       outerGuid:        (recursive InternalLoadObject)
//!       pathName:         FString
//!       if HasNetworkChecksum: u32
//! ```

use vrf_bitio::BitReader;

use crate::cache::NetGuidCache;
use crate::error::{Result, SchemaError};
use crate::export::{NetFieldExport, NetFieldExportGroup, render_fname};
use crate::guid::{ExportFlags, NetworkGuid};

/// Cap on a path-name FString, so a corrupt prefix cannot size an allocation.
pub(crate) const MAX_FSTRING_BYTES: i64 = 1024 * 1024; // 1 MiB

/// Maximum recursion depth for nested NetGUID objects.
const MAX_NET_GUID_RECURSION: u32 = 16;

/// Slot cap for an export group, shared with the checkpoint form: no corpus
/// group reaches 65,536, and it stops a five-byte IntPacked count from sizing
/// an allocation.
pub(crate) const MAX_FIELDS_PER_GROUP: u32 = 65_536;

/// A byte-aligned FName as sent, with the name the schema stores.
#[cfg_attr(not(feature = "checkpoint"), allow(dead_code))]
pub(crate) struct FName {
    /// The index in decimal, or [`render_fname`] of `base` and `number`.
    pub(crate) rendered: String,
    pub(crate) kind: u8,
    pub(crate) base: Option<String>,
    pub(crate) index: Option<u32>,
    pub(crate) number: Option<i32>,
}

/// A u8 kind, then an IntPacked index (nonzero kind) or an FString and i32
/// number (kind 0).
pub(crate) fn read_fname(reader: &mut BitReader<'_>) -> Result<FName> {
    let kind = reader.read_u8()?;
    if kind != 0 {
        let index = reader.read_int_packed()?;
        return Ok(FName {
            rendered: index.to_string(),
            kind,
            base: None,
            index: Some(index),
            number: None,
        });
    }
    let base = reader.read_fstring(MAX_FSTRING_BYTES)?;
    let number = reader.read_i32()?;
    Ok(FName {
        rendered: render_fname(base.clone(), number),
        kind,
        base: Some(base),
        index: None,
        number: Some(number),
    })
}

/// Read one frame's net-field export commands into `cache`, which accumulates
/// them across frames: new groups, and fields added to existing ones. Returns
/// the number of commands read.
#[must_use = "the export count is a tally; bind it or discard it explicitly"]
pub fn read_net_field_exports(reader: &mut BitReader<'_>, cache: &mut NetGuidCache) -> Result<u32> {
    let num_exports = reader.read_int_packed()?;

    for _ in 0..num_exports {
        let path_name_index = reader.read_int_packed()?;
        let is_exported = match reader.read_int_packed()? {
            0 => false,
            1 => true,
            value => {
                return Err(SchemaError::BadExportedFlag {
                    path_name_index,
                    value,
                });
            }
        };

        if is_exported {
            let path_name = reader.read_fstring(MAX_FSTRING_BYTES)?;
            let num_fields = reader.read_int_packed()?;

            if num_fields > MAX_FIELDS_PER_GROUP {
                return Err(SchemaError::FieldCountOverflow {
                    count: num_fields,
                    max: MAX_FIELDS_PER_GROUP,
                });
            }

            let group = NetFieldExportGroup::try_new(path_name, path_name_index, num_fields)?;
            cache.add_export_group(group)?;
        } else {
            // A reference to an existing group, which must already be known.
            if cache.get_group_by_index(path_name_index).is_none() {
                return Err(SchemaError::UnknownPathIndex {
                    index: path_name_index,
                });
            }
        }

        let is_field_exported = reader.read_u8()? != 0;
        if !is_field_exported {
            continue;
        }

        let handle = reader.read_int_packed()?;
        let compatible_checksum = reader.read_u32()?;
        let name = read_fname(reader)?.rendered;

        let field = NetFieldExport {
            handle,
            compatible_checksum,
            name,
        };

        // Out-of-range handles are dropped and counted (manifest
        // `dropped_field_exports`).
        cache.set_field_on_group(path_name_index, field);
    }

    Ok(num_exports)
}

/// Read one frame's exported NetGUID payloads into `cache`: each maps a GUID to
/// a path and optionally an outer GUID, and is length-prefixed so it can be
/// checked for complete consumption. Returns the number of payloads read.
#[must_use = "the GUID payload count is a tally; bind it or discard it explicitly"]
pub fn read_export_guids(reader: &mut BitReader<'_>, cache: &mut NetGuidCache) -> Result<u32> {
    let num_guids = reader.read_int_packed()?;

    for _ in 0..num_guids {
        let size = reader.read_i32()?;
        if size < 0 {
            return Err(SchemaError::NegativePayloadSize { size });
        }
        let byte_count = size as u64;

        // Exactly `size` bytes, so consumption can be verified.
        let mut payload = reader.sub_reader(byte_count * 8)?;

        internal_load_object(&mut payload, cache, true, 0)?;

        if payload.bits_remaining() >= 8 {
            return Err(SchemaError::TrailingPayloadData {
                remaining: (payload.bits_remaining() / 8) as usize,
            });
        }
    }

    Ok(num_guids)
}

/// Read a NetGUID object reference, recursing into its outer, and register it
/// (`NetGuidObjectReader.InternalLoadObject`).
fn internal_load_object(
    reader: &mut BitReader<'_>,
    cache: &mut NetGuidCache,
    is_exporting: bool,
    depth: u32,
) -> Result<NetworkGuid> {
    if depth >= MAX_NET_GUID_RECURSION {
        return Err(SchemaError::RecursionLimitExceeded {
            limit: MAX_NET_GUID_RECURSION,
        });
    }

    let net_guid = NetworkGuid(reader.read_int_packed()?);
    if !net_guid.is_valid() {
        return Ok(net_guid);
    }

    let flags = if net_guid.is_default() || is_exporting {
        ExportFlags(reader.read_u8()?)
    } else {
        ExportFlags::NONE
    };

    if !flags.contains(ExportFlags::HAS_PATH) {
        return Ok(net_guid);
    }

    let outer_guid = internal_load_object(reader, cache, is_exporting, depth + 1)?;

    let path_name = reader.read_fstring(MAX_FSTRING_BYTES)?;

    if flags.contains(ExportFlags::HAS_NETWORK_CHECKSUM) {
        let _checksum = reader.read_u32()?;
    }

    cache.set_net_guid_path(
        net_guid.0,
        path_name,
        outer_guid.is_valid().then_some(outer_guid),
    );

    Ok(net_guid)
}

/// Byte encoders for the tests of both archive readers.
#[cfg(test)]
pub(crate) mod wire {
    /// IntPacked: seven payload bits per byte, low bit set when more follow.
    pub(crate) fn int_packed(mut value: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        loop {
            let mut next_byte = ((value & 0x7F) << 1) as u8;
            value >>= 7;
            if value != 0 {
                next_byte |= 1; // continuation flag
            }
            bytes.push(next_byte);
            if value == 0 {
                return bytes;
            }
        }
    }

    /// FString in UTF-8: i32 length including the null, the bytes, the null.
    pub(crate) fn fstring(s: &str) -> Vec<u8> {
        let mut bytes = ((s.len() + 1) as i32).to_le_bytes().to_vec();
        bytes.extend_from_slice(s.as_bytes());
        bytes.push(0);
        bytes
    }

    /// A string FName: kind 0 (not hardcoded), the FString, the number.
    pub(crate) fn fname(s: &str, number: i32) -> Vec<u8> {
        let mut bytes = vec![0];
        bytes.extend(fstring(s));
        bytes.extend(number.to_le_bytes());
        bytes
    }
}

#[cfg(test)]
mod tests {
    use super::wire::{fname, fstring, int_packed};
    use super::*;

    /// A net-field export command declaring a group, with an optional field.
    fn build_new_group(
        path_name_index: u32,
        path: &str,
        num_fields: u32,
        field: Option<(u32, &str)>,
    ) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend(int_packed(path_name_index));
        bytes.extend(int_packed(1)); // isExported = true
        bytes.extend(fstring(path));
        bytes.extend(int_packed(num_fields));
        if let Some((handle, name)) = field {
            bytes.push(1); // isFieldExported = true
            bytes.extend(int_packed(handle));
            bytes.extend(0xAABBCCDDu32.to_le_bytes());
            bytes.extend(fname(name, 0));
        } else {
            bytes.push(0); // isFieldExported = false
        }
        bytes
    }

    /// A reference to an existing group that adds one field.
    fn build_existing_group_field(
        path_name_index: u32,
        handle: u32,
        name: &str,
        number: i32,
    ) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend(int_packed(path_name_index));
        bytes.extend(int_packed(0)); // isExported = false (reference)
        bytes.push(1); // isFieldExported = true
        bytes.extend(int_packed(handle));
        bytes.extend(0xAABBCCDDu32.to_le_bytes());
        bytes.extend(fname(name, number));
        bytes
    }

    /// An export-GUID payload: one object with HasPath and no outer.
    fn build_guid_payload(net_guid: u32, path: &str) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend(int_packed(net_guid));
        bytes.push(ExportFlags::HAS_PATH.0); // flags
        bytes.extend(int_packed(0)); // outer guid 0: invalid, ends the recursion
        bytes.extend(fstring(path));
        bytes
    }

    #[test]
    fn registers_exported_group() {
        let mut data = int_packed(1); // 1 export
        data.extend(build_new_group(11, "/Game/Test.Test_C", 3, None));

        let mut reader = BitReader::new(&data);
        let mut cache = NetGuidCache::new();
        read_net_field_exports(&mut reader, &mut cache).unwrap();

        let group = cache.get_group_by_index(11).unwrap();
        assert_eq!(group.path, "/Game/Test.Test_C");
        assert_eq!(group.path_name_index, 11);
        assert_eq!(group.len(), 3);
        assert!(cache.get_group_by_path("/Game/Test.Test_C").is_some());
    }

    #[test]
    fn live_group_rejects_field_count_above_checkpoint_protocol_limit() {
        let mut data = int_packed(1);
        data.extend(build_new_group(11, "/Game/Test.Test_C", 65_537, None));

        let mut reader = BitReader::new(&data);
        let mut cache = NetGuidCache::new();
        assert!(matches!(
            read_net_field_exports(&mut reader, &mut cache).unwrap_err(),
            SchemaError::FieldCountOverflow {
                count: 65_537,
                max: 65_536
            }
        ));
        assert_eq!(cache.group_count(), 0);
    }

    #[test]
    fn stores_export_by_handle() {
        let mut data = int_packed(1);
        data.extend(build_new_group(
            11,
            "/Game/Test.Test_C",
            3,
            Some((2, "FieldName")),
        ));

        let mut reader = BitReader::new(&data);
        let mut cache = NetGuidCache::new();
        read_net_field_exports(&mut reader, &mut cache).unwrap();

        let group = cache.get_group_by_index(11).unwrap();
        let field = group.get_field(2).unwrap();
        assert_eq!(field.handle, 2);
        assert_eq!(field.compatible_checksum, 0xAABBCCDD);
        assert_eq!(field.name, "FieldName");
    }

    #[test]
    fn existing_path_index_updates_group() {
        let mut data = int_packed(2); // 2 exports
        data.extend(build_new_group(11, "/Game/Test.Test_C", 3, None));
        data.extend(build_existing_group_field(11, 1, "LaterField", 0));

        let mut reader = BitReader::new(&data);
        let mut cache = NetGuidCache::new();
        read_net_field_exports(&mut reader, &mut cache).unwrap();

        let group = cache.get_group_by_index(11).unwrap();
        assert_eq!(group.get_field(1).unwrap().name, "LaterField");
    }

    #[test]
    fn re_exported_group_expands_without_losing_existing_fields() {
        let mut data = int_packed(2); // 2 exports
        data.extend(build_new_group(
            11,
            "/Game/Test.Test_C",
            2,
            Some((1, "ExistingField")),
        ));
        data.extend(build_new_group(
            11,
            "/Game/Test.Test_C",
            4,
            Some((3, "ExpandedField")),
        ));

        let mut reader = BitReader::new(&data);
        let mut cache = NetGuidCache::new();
        read_net_field_exports(&mut reader, &mut cache).unwrap();

        let group = cache.get_group_by_index(11).unwrap();
        assert_eq!(group.len(), 4);
        assert_eq!(group.get_field(1).unwrap().name, "ExistingField");
        assert_eq!(group.get_field(3).unwrap().name, "ExpandedField");
    }

    /// Read as a reference, isExported 2 would overwrite a real field with
    /// whatever bytes follow, and the read would succeed.
    #[test]
    fn an_is_exported_value_other_than_0_or_1_is_an_error() {
        let mut data = int_packed(2);
        data.extend(build_new_group(11, "/Game/T.T_C", 1, Some((0, "Real"))));
        let mut reference = build_existing_group_field(11, 0, "Overwritten", 0);
        reference[1] = 2 << 1; // isExported: IntPacked 2 in place of 0
        data.extend(reference);

        let mut cache = NetGuidCache::new();
        let err = read_net_field_exports(&mut BitReader::new(&data), &mut cache).unwrap_err();
        assert_eq!(
            err,
            SchemaError::BadExportedFlag {
                path_name_index: 11,
                value: 2
            }
        );
        let group = cache.get_group_by_index(11).unwrap();
        assert_eq!(group.get_field(0).unwrap().name, "Real");
    }

    #[test]
    fn unknown_path_index_returns_error() {
        let mut data = int_packed(1); // 1 export
        data.extend(int_packed(42)); // pathNameIndex
        data.extend(int_packed(0)); // isExported = false (reference)
        // The error fires before isFieldExported is read.

        let mut reader = BitReader::new(&data);
        let mut cache = NetGuidCache::new();
        let err = read_net_field_exports(&mut reader, &mut cache).unwrap_err();
        assert!(matches!(err, SchemaError::UnknownPathIndex { index: 42 }));
    }

    /// A handle beyond the group's declared length is dropped and counted. The
    /// counter (manifest `dropped_field_exports`) read 0 in all 1,018 manifests
    /// of the 2026-09-28 full-corpus audit, so only this test shows it moves.
    #[test]
    fn out_of_range_handle_is_dropped_and_counted() {
        // Group has capacity 1, field handle is 2 (out of range).
        let mut data = int_packed(1);
        data.extend(build_new_group(
            11,
            "/Game/Test.Test_C",
            1,
            Some((2, "OutOfRange")),
        ));

        let mut reader = BitReader::new(&data);
        let mut cache = NetGuidCache::new();
        // A drop is counted, not an error.
        read_net_field_exports(&mut reader, &mut cache).unwrap();

        assert_eq!(cache.dropped_field_exports(), 1);
        let group = cache.get_group_by_index(11).unwrap();
        assert_eq!(group.len(), 1, "a dropped field must not grow the group");
        assert_eq!(group.populated_fields().count(), 0);
    }

    /// Positive control for the test above: without it, a counter that moved
    /// on every field would pass the drop test.
    #[test]
    fn in_range_handle_is_placed_and_not_counted() {
        let mut data = int_packed(1);
        data.extend(build_new_group(
            11,
            "/Game/Test.Test_C",
            1,
            Some((0, "InRange")),
        ));

        let mut reader = BitReader::new(&data);
        let mut cache = NetGuidCache::new();
        read_net_field_exports(&mut reader, &mut cache).unwrap();

        assert_eq!(cache.dropped_field_exports(), 0);
        let group = cache.get_group_by_index(11).unwrap();
        assert_eq!(group.len(), 1);
        assert_eq!(group.get_field(0).unwrap().name, "InRange");
        assert_eq!(group.populated_fields().count(), 1);
    }

    /// The number is part of the name: 0 renders bare, 1 renders `_0`.
    #[test]
    fn fname_numbers_disambiguate_two_fields_with_one_base_name() {
        let mut data = int_packed(3); // 3 exports
        data.extend(build_new_group(
            11,
            "/Game/Test.Test_C",
            3,
            Some((0, "Value")),
        ));
        data.extend(build_existing_group_field(11, 1, "Value", 1));
        data.extend(build_existing_group_field(11, 2, "Value", 2));

        let mut reader = BitReader::new(&data);
        let mut cache = NetGuidCache::new();
        read_net_field_exports(&mut reader, &mut cache).unwrap();

        let group = cache.get_group_by_index(11).unwrap();
        assert_eq!(group.get_field(0).unwrap().name, "Value");
        assert_eq!(group.get_field(1).unwrap().name, "Value_0");
        assert_eq!(group.get_field(2).unwrap().name, "Value_1");
    }

    #[test]
    fn export_guid_registers_path() {
        let payload = build_guid_payload(17, "/Game/Test.Test_C");
        let mut data = int_packed(1); // 1 guid
        data.extend((payload.len() as i32).to_le_bytes());
        data.extend(&payload);

        let mut reader = BitReader::new(&data);
        let mut cache = NetGuidCache::new();
        read_export_guids(&mut reader, &mut cache).unwrap();

        assert_eq!(cache.get_path_by_guid(17).unwrap(), "/Game/Test.Test_C");
    }

    #[test]
    fn export_guid_negative_size_returns_error() {
        let mut data = int_packed(1);
        data.extend((-1i32).to_le_bytes());

        let mut reader = BitReader::new(&data);
        let mut cache = NetGuidCache::new();
        let err = read_export_guids(&mut reader, &mut cache).unwrap_err();
        assert!(matches!(err, SchemaError::NegativePayloadSize { size: -1 }));
    }

    #[test]
    fn export_guid_trailing_data_returns_error() {
        let mut payload = build_guid_payload(17, "/Game/Test.Test_C");
        payload.push(0xFF); // trailing byte
        let mut data = int_packed(1);
        data.extend((payload.len() as i32).to_le_bytes());
        data.extend(&payload);

        let mut reader = BitReader::new(&data);
        let mut cache = NetGuidCache::new();
        let err = read_export_guids(&mut reader, &mut cache).unwrap_err();
        assert!(matches!(err, SchemaError::TrailingPayloadData { .. }));
    }

    #[test]
    fn truncated_net_field_exports_returns_bitio_error() {
        let data = int_packed(5); // says 5 exports, but no data follows.
        let mut reader = BitReader::new(&data);
        let mut cache = NetGuidCache::new();
        let err = read_net_field_exports(&mut reader, &mut cache).unwrap_err();
        assert!(matches!(err, SchemaError::Bitio(_)));
    }

    #[test]
    fn truncated_export_guids_returns_bitio_error() {
        // Count says 1 but no payload size follows.
        let data = int_packed(1);
        let mut reader = BitReader::new(&data);
        let mut cache = NetGuidCache::new();
        let err = read_export_guids(&mut reader, &mut cache).unwrap_err();
        assert!(matches!(err, SchemaError::Bitio(_)));
    }

    #[test]
    fn zero_exports_is_valid() {
        let data = int_packed(0);
        let mut reader = BitReader::new(&data);
        let mut cache = NetGuidCache::new();
        let n = read_net_field_exports(&mut reader, &mut cache).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn zero_guids_is_valid() {
        let data = int_packed(0);
        let mut reader = BitReader::new(&data);
        let mut cache = NetGuidCache::new();
        let n = read_export_guids(&mut reader, &mut cache).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn roundtrip_multiple_groups_multiple_fields() {
        // Register 2 groups each with 2 fields, then verify all survive.
        let mut data = int_packed(4); // 4 export commands total

        data.extend(build_new_group(1, "/Game/A.A_C", 3, Some((0, "Alpha"))));
        data.extend(build_existing_group_field(1, 2, "Gamma", 0));
        data.extend(build_new_group(2, "/Game/B.B_C", 2, Some((1, "Beta"))));
        data.extend(build_existing_group_field(2, 0, "Delta", 0));

        let mut reader = BitReader::new(&data);
        let mut cache = NetGuidCache::new();
        let n = read_net_field_exports(&mut reader, &mut cache).unwrap();
        assert_eq!(n, 4);

        let a = cache.get_group_by_index(1).unwrap();
        assert_eq!(a.get_field(0).unwrap().name, "Alpha");
        assert_eq!(a.get_field(2).unwrap().name, "Gamma");
        assert!(a.get_field(1).is_none());

        let b = cache.get_group_by_index(2).unwrap();
        assert_eq!(b.get_field(1).unwrap().name, "Beta");
        assert_eq!(b.get_field(0).unwrap().name, "Delta");
    }
}
