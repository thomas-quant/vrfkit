//! The two schema tables a Checkpoint chunk carries ahead of its DemoFrame.
//!
//! A checkpoint archive is self-contained: it restates the server's NetGUID
//! cache and its whole net-field export map, then a single DemoFrame. Nothing
//! in it references the ReplayData stream, so it is read into its own
//! [`NetGuidCache`] rather than merged into the live one -- the frame that
//! follows re-opens every actor alive at that instant, and replaying those
//! channel opens through the live reader would corrupt the running channel
//! table.
//!
//! # Archive layout
//!
//! ```text
//! +0   u32  frame offset      -- the DemoFrame begins at this + 8
//! +4   u32  0                 -- reserved, zero in every corpus checkpoint
//! +8   u32  0
//! +12  u32  0
//! +16  u32  guid entry count
//! +20  GuidCacheEntry x count
//!      u32  export group count
//!      NetFieldExportGroup x count
//!      <- the DemoFrame starts here, and this offset must equal (+0) + 8
//! ```
//!
//! All reads are byte-aligned `FBinaryArchive` semantics, as in the DemoFrame
//! grammar itself.
//!
//! # GuidCacheEntry
//!
//! ```text
//! NetGUID      : IntPacked
//! OuterGUID    : IntPacked
//! PathIsString : u8            -- 0 or 1 only
//!   if 1: PathName  : FString  -- no trailing i32, unlike an FName
//!   if 0: NameIndex : IntPacked
//! Flags        : u8
//! ```
//!
//! **The polarity is the opposite of an FName's.** In [`read_fname`] a leading
//! `1` means "hardcoded index"; here a leading `1` means "a string follows".
//! They are different fields and the FName reader must not be pointed at this
//! one.
//!
//! # NetFieldExportGroup
//!
//! ```text
//! PathName           : FString
//! PathNameIndex      : IntPacked
//! NumNetFieldExports : IntPacked        <-- IntPacked, and the count at the
//!                                           head of the section is a u32
//! repeat, slot index i = 0..N:
//!     bExported : u8
//!     if bExported:
//!         Handle             : IntPacked   -- always == i
//!         CompatibleChecksum : u32
//!         ExportName         : FName
//! ```
//!
//! The two counts in this section use different encodings, and that is the
//! detail that defeats a first implementation: reading `NumNetFieldExports` as
//! a `u32` yields exactly twice the true value for small counts, because
//! `IntPacked` shifts left by one. The cursor then overruns into the next
//! record and produces plausible garbage rather than an error, which is why
//! [`read_checkpoint_tables`] ends by asserting the prologue's frame offset.

use vrf_bitio::{BitError, BitReader};

use crate::cache::NetGuidCache;
use crate::error::{Result, SchemaError};
use crate::export::{NetFieldExport, NetFieldExportGroup, render_fname};
use crate::guid::NetworkGuid;

/// Streaming observer for checkpoint schema records. Borrowed strings are valid
/// only for the callback; the cache receives its own owned copy afterwards.
pub trait CheckpointTableSink {
    type Error;

    #[allow(clippy::too_many_arguments)]
    fn on_guid_entry(
        &mut self,
        ordinal: u32,
        guid: u32,
        outer: u32,
        path_is_string: bool,
        literal_path: Option<&str>,
        name_index: Option<u32>,
        flags: u8,
    ) -> core::result::Result<(), Self::Error>;

    fn on_export_group(
        &mut self,
        ordinal: u32,
        path_name_index: u32,
        group_path: &str,
        declared_slots: u32,
    ) -> core::result::Result<(), Self::Error>;

    #[allow(clippy::too_many_arguments)]
    fn on_export_field(
        &mut self,
        group_ordinal: u32,
        path_name_index: u32,
        slot: u32,
        handle: u32,
        checksum: u32,
        rendered_name: &str,
        exported_flag: u8,
        fname_kind: u8,
        base: Option<&str>,
        index: Option<u32>,
        number: Option<i32>,
    ) -> core::result::Result<(), Self::Error>;
}

#[derive(Debug, thiserror::Error)]
pub enum CheckpointReadError<E> {
    #[error(transparent)]
    Schema(#[from] SchemaError),
    #[error(transparent)]
    Bit(#[from] BitError),
    #[error("checkpoint table observer failed")]
    Sink(E),
}

/// Maximum string size for a checkpoint path or name.
const MAX_FSTRING_BYTES: i64 = 1024 * 1024;

/// Sanity bound on the guid-cache entry count. The largest corpus checkpoint
/// carries roughly 12,000; a million would mean a mis-read length.
const MAX_GUID_ENTRIES: u32 = 1_000_000;

/// Sanity bound on the export-group count. The largest corpus checkpoint
/// declares 543.
const MAX_GROUPS: u32 = 100_000;

/// Sanity bound on a single group's declared field-slot count.
const MAX_FIELDS_PER_GROUP: u32 = 65_536;

/// What [`read_checkpoint_tables`] consumed, for the caller to report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckpointTables {
    /// GUID cache entries read.
    pub guid_count: u32,
    /// Export groups read.
    pub group_count: u32,
    /// Field slots that were actually exported (the rest are holes).
    pub exported_fields: u32,
    /// Byte offset where the DemoFrame begins.
    pub frame_offset: usize,
    /// Entries whose path arrived as a GUID-path name-table index rather than
    /// a string. The historical field name is retained for API compatibility;
    /// these indices are not hardcoded Unreal EName values.
    pub hardcoded_paths: u32,
    /// Literal GUID-path entries read, in either mode.
    pub literal_paths: u32,
    /// Wire indices resolved through preceding literal paths.
    pub resolved_path_indices: u32,
    /// Export-group collisions. Always zero on success: a collision makes the
    /// checkpoint cache untrusted and returns
    /// [`SchemaError::CheckpointGroupCollision`] before frame decode.
    pub group_collisions: u32,
}

/// Interpretation of checkpoint GUID path indices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckpointPathMode {
    /// Retain the former decimal rendering for callers comparing legacy output.
    LegacyDecimal,
    /// Resolve zero-based indices into preceding literal GUID paths in this
    /// checkpoint. This is the default. It was first measured on builds 13.01
    /// through 13.05; the main-stream cross-check described at
    /// [`read_checkpoint_tables`] covers all 24 supported builds.
    LiteralPathTable,
}

/// Read a checkpoint archive's guid cache and export-group map into `cache`,
/// and report where its DemoFrame begins.
///
/// `data` is the decompressed archive from
/// `vrf_container::decompress_checkpoint`. `cache` should be **fresh**: a
/// checkpoint restates the whole schema, and merging it into the live
/// ReplayData cache would combine independent schema snapshots.
///
/// # GUID path indices
///
/// An indexed path selects a preceding literal path in this checkpoint, using
/// zero-based order. References do not append to the table, and the table resets
/// for every call. This rule resolved all 14,403,610 indexed entries in 714
/// measured replays from builds 13.01, 13.02, 13.04 and 13.05 (2026-09-08).
///
/// The main replay stream is an independent check on it: it declares the same
/// server GUIDs through [`crate::read_export_guids`] into a separate cache, so
/// its paths never pass through this rule. On 2026-09-28, in 1,018 replays
/// covering all 24 supported builds, the path this rule selects equalled the
/// main stream's path for all 19,993,994 indexed entries whose GUID the main
/// stream also declared, while a one-based index, references appended to the
/// table, and one table shared across checkpoints each disagreed on most of
/// them. `tools/check_export_baseline.py --checkpoints` and
/// `tools/verify_build_corpus.py` repeat the comparison on every export they
/// check; docs/CHECKPOINT_PATH_RESOLUTION.md has the method. That is
/// agreement between two readers of the same files: the exact current engine
/// serializer is still not available as an independent specification. Raw
/// indices remain available to [`CheckpointTableSink`].
///
/// An index outside the preceding literals is rejected before decoding the
/// frame. Call [`read_checkpoint_tables_with_sink_mode`] with
/// [`CheckpointPathMode::LegacyDecimal`] only when reproducing legacy paths.
///
/// # Errors
///
/// Beyond truncation: a path discriminator outside `{0, 1}`, a slot whose
/// declared handle is not its own index, a non-zero reserved prologue word,
/// and a table parse that does not finish exactly where the prologue says the
/// frame begins. Each is a check that the cursor is still aligned; without
/// them a mis-read count yields well-formed nonsense.
/// `PathIsString` accepts only `0` and `1`; the separate FName kind and
/// exported-slot flag retain and accept any nonzero byte, matching the legacy
/// reader's permissive wire behavior.
pub fn read_checkpoint_tables(data: &[u8], cache: &mut NetGuidCache) -> Result<CheckpointTables> {
    let mut sink = NoopCheckpointTableSink;
    match read_checkpoint_tables_with_sink(data, cache, &mut sink) {
        Ok(tables) => Ok(tables),
        Err(CheckpointReadError::Schema(error)) => Err(error),
        Err(CheckpointReadError::Bit(error)) => Err(SchemaError::Bitio(error)),
        Err(CheckpointReadError::Sink(never)) => match never {},
    }
}

struct NoopCheckpointTableSink;

impl CheckpointTableSink for NoopCheckpointTableSink {
    type Error = core::convert::Infallible;

    fn on_guid_entry(
        &mut self,
        _: u32,
        _: u32,
        _: u32,
        _: bool,
        _: Option<&str>,
        _: Option<u32>,
        _: u8,
    ) -> core::result::Result<(), Self::Error> {
        Ok(())
    }

    fn on_export_group(
        &mut self,
        _: u32,
        _: u32,
        _: &str,
        _: u32,
    ) -> core::result::Result<(), Self::Error> {
        Ok(())
    }

    fn on_export_field(
        &mut self,
        _: u32,
        _: u32,
        _: u32,
        _: u32,
        _: u32,
        _: &str,
        _: u8,
        _: u8,
        _: Option<&str>,
        _: Option<u32>,
        _: Option<i32>,
    ) -> core::result::Result<(), Self::Error> {
        Ok(())
    }
}

/// Read checkpoint tables while delivering each decoded record to `sink`.
///
/// The sink is invoked after a record has passed its wire checks but before it
/// is stored in `cache`. A sink failure stops immediately and does not consume
/// any later record or DemoFrame bytes.
pub fn read_checkpoint_tables_with_sink<S: CheckpointTableSink>(
    data: &[u8],
    cache: &mut NetGuidCache,
    sink: &mut S,
) -> core::result::Result<CheckpointTables, CheckpointReadError<S::Error>> {
    read_checkpoint_tables_with_sink_mode(data, cache, sink, CheckpointPathMode::LiteralPathTable)
}

/// Read checkpoint tables with an explicit path interpretation.
pub fn read_checkpoint_tables_with_sink_mode<S: CheckpointTableSink>(
    data: &[u8],
    cache: &mut NetGuidCache,
    sink: &mut S,
    mode: CheckpointPathMode,
) -> core::result::Result<CheckpointTables, CheckpointReadError<S::Error>> {
    let mut reader = BitReader::new(data);

    // -- Prologue ----------------------------------------------------------
    let frame_offset_word = reader.read_u32()?;
    for offset in [4usize, 8, 12] {
        let value = reader.read_u32()?;
        if value != 0 {
            return Err(SchemaError::CheckpointReservedWordSet { offset, value }.into());
        }
    }
    let guid_count = reader.read_u32()?;
    if guid_count > MAX_GUID_ENTRIES {
        return Err(SchemaError::CheckpointCountOverflow {
            field: "guid entries",
            count: guid_count,
            max: MAX_GUID_ENTRIES,
        }
        .into());
    }

    // -- GUID cache --------------------------------------------------------
    let mut hardcoded_paths = 0u32;
    let mut literal_paths = Vec::new();
    let mut literal_count = 0u32;
    let mut resolved_path_indices = 0u32;
    for entry in 0..guid_count {
        let net_guid = reader.read_int_packed()?;
        let outer_guid = reader.read_int_packed()?;
        let path_kind = reader.read_u8()?;
        let (path_is_string, path, name_index) = match path_kind {
            1 => {
                let path = reader.read_fstring(MAX_FSTRING_BYTES)?;
                literal_count += 1;
                if mode == CheckpointPathMode::LiteralPathTable {
                    literal_paths.push(path.clone());
                }
                (true, path, None)
            }
            0 => {
                hardcoded_paths += 1;
                let index = reader.read_int_packed()?;
                let path = match mode {
                    CheckpointPathMode::LegacyDecimal => index.to_string(),
                    CheckpointPathMode::LiteralPathTable => {
                        let Some(path) = literal_paths.get(index as usize) else {
                            return Err(SchemaError::CheckpointPathIndexOutOfBounds {
                                entry,
                                index,
                                literals: literal_paths.len() as u32,
                            }
                            .into());
                        };
                        resolved_path_indices += 1;
                        path.clone()
                    }
                };
                (false, path, Some(index))
            }
            byte => return Err(SchemaError::CheckpointBadPathKind { entry, byte }.into()),
        };
        // Preserve the flags byte without assigning unverified bit meanings.
        let flags = reader.read_u8()?;

        sink.on_guid_entry(
            entry,
            net_guid,
            outer_guid,
            path_is_string,
            path_is_string.then_some(path.as_str()),
            name_index,
            flags,
        )
        .map_err(CheckpointReadError::Sink)?;

        cache.set_net_guid_path(net_guid, path, Some(NetworkGuid(outer_guid)));
    }

    // -- Export group map --------------------------------------------------
    let group_count = reader.read_u32()?;
    if group_count > MAX_GROUPS {
        return Err(SchemaError::CheckpointCountOverflow {
            field: "export groups",
            count: group_count,
            max: MAX_GROUPS,
        }
        .into());
    }

    let mut exported_fields = 0u32;
    for group_ordinal in 0..group_count {
        let path = reader.read_fstring(MAX_FSTRING_BYTES)?;
        let path_name_index = reader.read_int_packed()?;
        // IntPacked, not u32. See the module docs.
        let declared = reader.read_int_packed()?;
        if declared > MAX_FIELDS_PER_GROUP {
            return Err(SchemaError::CheckpointCountOverflow {
                field: "fields in a group",
                count: declared,
                max: MAX_FIELDS_PER_GROUP,
            }
            .into());
        }

        sink.on_export_group(group_ordinal, path_name_index, &path, declared)
            .map_err(CheckpointReadError::Sink)?;

        // Tested before the add, and with exactly the two lookups
        // `add_export_group` merges on: `by_path` (which includes the path
        // aliases it registers) and `by_index`. The cache is fresh per
        // checkpoint, so a hit here means this checkpoint declared the slot
        // twice, not that a previous frame did. Returning here prevents an
        // ambiguous cache from reaching frame decode.
        if cache.get_group_by_index(path_name_index).is_some()
            || cache.get_group_by_path(&path).is_some()
        {
            return Err(SchemaError::CheckpointGroupCollision {
                path,
                path_name_index,
            }
            .into());
        }

        cache.add_export_group(NetFieldExportGroup::new(
            path.clone(),
            path_name_index,
            declared,
        ))?;

        for slot in 0..declared {
            let exported_flag = reader.read_u8()?;
            if exported_flag == 0 {
                continue;
            }
            let handle = reader.read_int_packed()?;
            if handle != slot {
                return Err(SchemaError::CheckpointHandleNotSlot {
                    group: path,
                    slot,
                    handle,
                }
                .into());
            }
            let compatible_checksum = reader.read_u32()?;
            let observed_name = read_observed_fname(&mut reader)?;
            sink.on_export_field(
                group_ordinal,
                path_name_index,
                slot,
                handle,
                compatible_checksum,
                &observed_name.rendered,
                exported_flag,
                observed_name.kind,
                observed_name.base.as_deref(),
                observed_name.index,
                observed_name.number,
            )
            .map_err(CheckpointReadError::Sink)?;
            cache.set_field_on_group(
                path_name_index,
                NetFieldExport {
                    handle,
                    compatible_checksum,
                    name: observed_name.rendered,
                },
            );
            exported_fields += 1;
        }
    }

    // -- The one end-to-end check -----------------------------------------
    let map_end = (reader.position() / 8) as usize;
    let expected = frame_offset_word as usize + 8;
    if map_end != expected {
        return Err(SchemaError::CheckpointFrameOffsetMismatch { map_end, expected }.into());
    }

    Ok(CheckpointTables {
        guid_count,
        group_count,
        exported_fields,
        frame_offset: map_end,
        hardcoded_paths,
        literal_paths: literal_count,
        resolved_path_indices,
        group_collisions: 0,
    })
}

/// Read an FName from a byte-aligned archive.
///
/// Duplicated from `reader.rs` rather than shared because the two callers read
/// different fields that merely look alike: this one's leading byte is
/// `bHardcoded`, while a guid entry's is `PathIsString` with the opposite
/// meaning. Keeping them apart is what stops the wrong one being reused.
///
/// The *rendering* is shared, though -- see [`render_fname`]. The two readers
/// may not be merged, but a name must mean the same thing whichever one
/// produced it, and this one used to drop the instance number just as the other
/// did.
struct ObservedFName {
    rendered: String,
    kind: u8,
    base: Option<String>,
    index: Option<u32>,
    number: Option<i32>,
}

fn read_observed_fname(reader: &mut BitReader<'_>) -> Result<ObservedFName> {
    let kind = reader.read_u8()?;
    if kind != 0 {
        let index = reader.read_int_packed()?;
        Ok(ObservedFName {
            rendered: index.to_string(),
            kind,
            base: None,
            index: Some(index),
            number: None,
        })
    } else {
        let base = reader.read_fstring(MAX_FSTRING_BYTES)?;
        let number = reader.read_i32()?;
        Ok(ObservedFName {
            rendered: render_fname(base.clone(), number),
            kind,
            base: Some(base),
            index: None,
            number: Some(number),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `(net_guid, outer_guid, path or None for a hardcoded name, name index)`.
    type GuidSpec<'a> = (u32, u32, Option<&'a str>, u32);
    /// `(group path, declared slot count, exported (handle, name) pairs)`.
    type GroupSpec<'a> = (&'a str, u32, &'a [(u32, &'a str)]);
    type GuidEvent = (u32, bool, Option<String>, Option<u32>, u8);
    type FieldEvent = (
        u32,
        u32,
        u32,
        u32,
        String,
        u8,
        u8,
        Option<String>,
        Option<u32>,
        Option<i32>,
    );

    /// Build an archive: prologue, guid entries, group map, then `frame`.
    fn build(guids: &[GuidSpec<'_>], groups: &[GroupSpec<'_>], frame: &[u8]) -> Vec<u8> {
        let mut body = Vec::new();
        for (guid, outer, path, name_index) in guids {
            push_packed(&mut body, *guid);
            push_packed(&mut body, *outer);
            match path {
                Some(p) => {
                    body.push(1);
                    push_fstring(&mut body, p);
                }
                None => {
                    body.push(0);
                    push_packed(&mut body, *name_index);
                }
            }
            body.push(0x03);
        }
        body.extend_from_slice(&(groups.len() as u32).to_le_bytes());
        for (path, declared, fields) in groups {
            push_fstring(&mut body, path);
            push_packed(&mut body, 7);
            push_packed(&mut body, *declared);
            for slot in 0..*declared {
                match fields.iter().find(|(h, _)| *h == slot) {
                    Some((h, name)) => {
                        body.push(1);
                        push_packed(&mut body, *h);
                        body.extend_from_slice(&0xdead_beefu32.to_le_bytes());
                        body.push(0); // FName: not hardcoded
                        push_fstring(&mut body, name);
                        body.extend_from_slice(&0i32.to_le_bytes());
                    }
                    None => body.push(0),
                }
            }
        }
        let mut out = Vec::new();
        // The frame offset is measured from byte 8, so it is (20 + body) - 8.
        out.extend_from_slice(&((20 + body.len() - 8) as u32).to_le_bytes());
        out.extend_from_slice(&[0u8; 12]);
        out.extend_from_slice(&(guids.len() as u32).to_le_bytes());
        out.extend_from_slice(&body);
        out.extend_from_slice(frame);
        out
    }

    fn push_packed(out: &mut Vec<u8>, mut value: u32) {
        loop {
            let mut b = ((value & 0x7F) << 1) as u8;
            value >>= 7;
            if value != 0 {
                b |= 1;
            }
            out.push(b);
            if value == 0 {
                return;
            }
        }
    }

    /// UTF-16LE with a negative length, which is how every corpus string
    /// arrives.
    fn push_fstring(out: &mut Vec<u8>, s: &str) {
        let units: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
        out.extend_from_slice(&(-(units.len() as i32)).to_le_bytes());
        for u in units {
            out.extend_from_slice(&u.to_le_bytes());
        }
    }

    /// An archive with no GUID entries and one slotless group per
    /// `(path, path_name_index)`; `build` writes index 7 for every group.
    fn groups_at(groups: &[(&str, u32)]) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&(groups.len() as u32).to_le_bytes());
        for (path, index) in groups {
            push_fstring(&mut body, path);
            push_packed(&mut body, *index);
            push_packed(&mut body, 0); // no field slots
        }
        let mut archive = Vec::new();
        archive.extend_from_slice(&((20 + body.len() - 8) as u32).to_le_bytes());
        archive.extend_from_slice(&[0u8; 12]);
        archive.extend_from_slice(&0u32.to_le_bytes()); // no guid entries
        archive.extend_from_slice(&body);
        archive
    }

    #[derive(Default)]
    struct RecordingSink {
        guids: Vec<GuidEvent>,
        groups: Vec<(u32, u32, String, u32)>,
        fields: Vec<FieldEvent>,
    }

    impl CheckpointTableSink for RecordingSink {
        type Error = ();

        fn on_guid_entry(
            &mut self,
            ordinal: u32,
            _: u32,
            _: u32,
            path_is_string: bool,
            literal_path: Option<&str>,
            name_index: Option<u32>,
            flags: u8,
        ) -> core::result::Result<(), Self::Error> {
            self.guids.push((
                ordinal,
                path_is_string,
                literal_path.map(str::to_owned),
                name_index,
                flags,
            ));
            Ok(())
        }

        fn on_export_group(
            &mut self,
            ordinal: u32,
            path_name_index: u32,
            group_path: &str,
            declared_slots: u32,
        ) -> core::result::Result<(), Self::Error> {
            self.groups.push((
                ordinal,
                path_name_index,
                group_path.to_owned(),
                declared_slots,
            ));
            Ok(())
        }

        fn on_export_field(
            &mut self,
            group_ordinal: u32,
            path_name_index: u32,
            slot: u32,
            handle: u32,
            checksum: u32,
            rendered_name: &str,
            exported_flag: u8,
            fname_kind: u8,
            base: Option<&str>,
            index: Option<u32>,
            number: Option<i32>,
        ) -> core::result::Result<(), Self::Error> {
            self.fields.push((
                group_ordinal,
                path_name_index,
                slot,
                handle,
                rendered_name.to_owned(),
                exported_flag,
                fname_kind,
                base.map(str::to_owned),
                index,
                number,
            ));
            assert_eq!(checksum, 0xdead_beef);
            Ok(())
        }
    }

    #[test]
    fn observer_reports_raw_variants_before_cache_storage() {
        let archive = build(
            &[(7, 0, Some("/Game/X"), 0), (8, 7, None, 0)],
            &[("/Script/G.Thing", 2, &[(1, "Value")])],
            &[],
        );
        let mut cache = NetGuidCache::new();
        let mut sink = RecordingSink::default();
        let tables = read_checkpoint_tables_with_sink(&archive, &mut cache, &mut sink).unwrap();

        assert_eq!(tables.exported_fields, 1);
        assert_eq!(sink.guids[0], (0, true, Some("/Game/X".into()), None, 3));
        assert_eq!(sink.guids[1], (1, false, None, Some(0), 3));
        assert_eq!(sink.groups, vec![(0, 7, "/Script/G.Thing".into(), 2)]);
        assert_eq!(
            sink.fields,
            vec![(
                0,
                7,
                1,
                1,
                "Value".into(),
                1,
                0,
                Some("Value".into()),
                None,
                Some(0),
            )]
        );
        assert_eq!(cache.get_path_by_guid(7), Some("/Game/X"));
    }

    struct StopAfterFirstGuid(u32);

    impl CheckpointTableSink for StopAfterFirstGuid {
        type Error = &'static str;

        fn on_guid_entry(
            &mut self,
            _: u32,
            _: u32,
            _: u32,
            _: bool,
            _: Option<&str>,
            _: Option<u32>,
            _: u8,
        ) -> core::result::Result<(), Self::Error> {
            self.0 += 1;
            Err("stop")
        }

        fn on_export_group(
            &mut self,
            _: u32,
            _: u32,
            _: &str,
            _: u32,
        ) -> core::result::Result<(), Self::Error> {
            panic!("reader consumed a later record after sink failure")
        }

        fn on_export_field(
            &mut self,
            _: u32,
            _: u32,
            _: u32,
            _: u32,
            _: u32,
            _: &str,
            _: u8,
            _: u8,
            _: Option<&str>,
            _: Option<u32>,
            _: Option<i32>,
        ) -> core::result::Result<(), Self::Error> {
            panic!("reader consumed a later record after sink failure")
        }
    }

    #[test]
    fn observer_failure_stops_before_cache_or_later_records() {
        let archive = build(
            &[
                (7, 0, Some("/Game/First"), 0),
                (8, 0, Some("/Game/Second"), 0),
            ],
            &[("/Script/G.Thing", 0, &[])],
            &[0xA5],
        );
        let mut cache = NetGuidCache::new();
        let mut sink = StopAfterFirstGuid(0);
        let error = read_checkpoint_tables_with_sink(&archive, &mut cache, &mut sink).unwrap_err();

        assert!(matches!(error, CheckpointReadError::Sink("stop")));
        assert_eq!(sink.0, 1);
        assert!(cache.get_path_by_guid(7).is_none());
        assert!(cache.get_path_by_guid(8).is_none());
        assert_eq!(cache.group_count(), 0);
    }

    #[test]
    fn observer_sees_zero_slot_group() {
        let archive = build(&[], &[("/Script/G.Empty", 0, &[])], &[0xA5]);
        let mut cache = NetGuidCache::new();
        let mut sink = RecordingSink::default();
        read_checkpoint_tables_with_sink(&archive, &mut cache, &mut sink).unwrap();

        assert_eq!(sink.groups, vec![(0, 7, "/Script/G.Empty".into(), 0)]);
        assert!(sink.fields.is_empty());
    }

    #[test]
    fn observer_preserves_nonzero_wire_flags_and_hardcoded_fname_index() {
        let mut body = Vec::new();
        body.extend_from_slice(&1u32.to_le_bytes()); // one group
        push_fstring(&mut body, "/Script/G.Raw");
        push_packed(&mut body, 42);
        push_packed(&mut body, 1);
        body.push(9); // nonzero bExported is accepted verbatim
        push_packed(&mut body, 0);
        body.extend_from_slice(&0xdead_beefu32.to_le_bytes());
        body.push(2); // nonzero FName kind is a hardcoded index, verbatim
        push_packed(&mut body, 216);

        let mut archive = Vec::new();
        archive.extend_from_slice(&((20 + body.len() - 8) as u32).to_le_bytes());
        archive.extend_from_slice(&[0u8; 12]);
        archive.extend_from_slice(&0u32.to_le_bytes());
        archive.extend_from_slice(&body);

        let mut cache = NetGuidCache::new();
        let mut sink = RecordingSink::default();
        read_checkpoint_tables_with_sink(&archive, &mut cache, &mut sink).unwrap();

        assert_eq!(
            sink.fields,
            vec![(0, 42, 0, 0, "216".into(), 9, 2, None, Some(216), None)]
        );
        assert_eq!(
            cache
                .get_group_by_index(42)
                .and_then(|group| group.get_field(0))
                .map(|field| field.name.as_str()),
            Some("216")
        );
    }

    const RESOLVED: CheckpointPathMode = CheckpointPathMode::LiteralPathTable;

    #[test]
    fn resolved_default_preserves_raw_observers_and_explicit_legacy_mode() {
        let archive = build(
            &[
                (7, 0, Some("/First"), 0),
                (8, 7, Some("123"), 0),
                (9, 7, None, 0),
                (10, 8, None, 1),
            ],
            &[("/Script/G.Thing", 2, &[(1, "Value")])],
            &[0xa5],
        );
        let mut legacy_cache = NetGuidCache::new();
        let mut legacy_sink = RecordingSink::default();
        let legacy = read_checkpoint_tables_with_sink_mode(
            &archive,
            &mut legacy_cache,
            &mut legacy_sink,
            CheckpointPathMode::LegacyDecimal,
        )
        .unwrap();
        let mut cache = NetGuidCache::new();
        let mut sink = RecordingSink::default();
        let measured = read_checkpoint_tables_with_sink(&archive, &mut cache, &mut sink).unwrap();
        assert_eq!(sink.guids, legacy_sink.guids);
        assert_eq!(sink.groups, legacy_sink.groups);
        assert_eq!(sink.fields, legacy_sink.fields);
        assert_eq!(measured.frame_offset, archive.len() - 1);
        assert_eq!(measured.frame_offset, legacy.frame_offset);
        assert_eq!(measured.guid_count, legacy.guid_count);
        assert_eq!(measured.exported_fields, legacy.exported_fields);
        assert_eq!(measured.hardcoded_paths, 2);
        assert_eq!(measured.literal_paths, 2);
        assert_eq!(legacy.literal_paths, 2);
        assert_eq!(measured.resolved_path_indices, 2);
        assert_eq!(legacy.resolved_path_indices, 0);
        assert_eq!(legacy_cache.get_path_by_guid(9), Some("0"));
        assert_eq!(legacy_cache.get_path_by_guid(10), Some("1"));
        assert_eq!(cache.get_path_by_guid(9), Some("/First"));
        assert_eq!(cache.get_path_by_guid(10), Some("123"));
        assert_eq!(cache.get_outer_guid(10), Some(NetworkGuid(8)));
    }

    #[test]
    fn resolved_references_do_not_append_to_the_literal_table() {
        let archive = build(
            &[
                (7, 0, Some("A"), 0),
                (8, 0, None, 0),
                (9, 0, Some("B"), 0),
                (10, 0, None, 1),
            ],
            &[],
            &[],
        );
        let mut cache = NetGuidCache::new();
        let measured = read_checkpoint_tables_with_sink_mode(
            &archive,
            &mut cache,
            &mut NoopCheckpointTableSink,
            RESOLVED,
        )
        .unwrap();
        assert_eq!(cache.get_path_by_guid(10), Some("B"));
        assert_eq!(
            (measured.literal_paths, measured.resolved_path_indices),
            (2, 2)
        );
    }

    #[test]
    fn resolved_duplicate_literal_occurrences_each_append() {
        // No corpus duplicate was observed; this defines only the hypothesis.
        let archive = build(
            &[
                (7, 0, Some("A"), 0),
                (8, 0, Some("A"), 0),
                (9, 0, Some("B"), 0),
                (10, 0, None, 2),
            ],
            &[],
            &[],
        );
        let mut cache = NetGuidCache::new();
        let measured = read_checkpoint_tables_with_sink_mode(
            &archive,
            &mut cache,
            &mut NoopCheckpointTableSink,
            RESOLVED,
        )
        .unwrap();
        assert_eq!(cache.get_path_by_guid(10), Some("B"));
        assert_eq!(measured.literal_paths, 3);
    }

    #[test]
    fn resolved_table_resets_between_checkpoint_reads() {
        let first = build(&[(7, 0, Some("A"), 0)], &[], &[]);
        let second = build(&[(8, 0, None, 0)], &[], &[]);
        let mut cache = NetGuidCache::new();
        read_checkpoint_tables_with_sink_mode(
            &first,
            &mut cache,
            &mut NoopCheckpointTableSink,
            RESOLVED,
        )
        .unwrap();
        let error = read_checkpoint_tables_with_sink_mode(
            &second,
            &mut cache,
            &mut NoopCheckpointTableSink,
            RESOLVED,
        )
        .unwrap_err();
        assert!(matches!(
            error,
            CheckpointReadError::Schema(SchemaError::CheckpointPathIndexOutOfBounds {
                entry: 0,
                index: 0,
                literals: 0
            })
        ));
        assert!(cache.get_path_by_guid(8).is_none());
    }

    #[test]
    fn resolved_forward_and_upper_bound_indices_are_rejected() {
        for (guids, entry, index, literals) in [
            (vec![(7, 0, None, 0), (8, 0, Some("Later"), 0)], 0, 0, 0),
            (vec![(7, 0, Some("A"), 0), (8, 0, None, 1)], 1, 1, 1),
            (
                vec![(7, 0, Some("A"), 0), (8, 0, None, u32::MAX)],
                1,
                u32::MAX,
                1,
            ),
        ] {
            let archive = build(&guids, &[], &[]);
            let error = read_checkpoint_tables_with_sink_mode(
                &archive,
                &mut NetGuidCache::new(),
                &mut NoopCheckpointTableSink,
                RESOLVED,
            )
            .unwrap_err();
            assert!(matches!(error, CheckpointReadError::Schema(
                SchemaError::CheckpointPathIndexOutOfBounds {
                    entry: e, index: i, literals: l
                }) if (e, i, l) == (entry, index, literals)));
        }
    }

    #[test]
    fn reads_both_tables_and_lands_on_the_frame() {
        let archive = build(
            &[(7, 0, Some("/Game/Maps/Ascent/Ascent"), 0), (5, 7, None, 0)],
            &[(
                "/Script/ShooterGame.Thing",
                4,
                &[(1, "Health"), (3, "Armor")],
            )],
            &[0xAB; 32],
        );
        let mut cache = NetGuidCache::new();
        let t = read_checkpoint_tables(&archive, &mut cache).unwrap();

        assert_eq!(t.guid_count, 2);
        assert_eq!(t.group_count, 1);
        assert_eq!(t.exported_fields, 2);
        assert_eq!(t.hardcoded_paths, 1);
        assert_eq!(t.frame_offset, archive.len() - 32);
        assert_eq!(cache.get_path_by_guid(7), Some("/Game/Maps/Ascent/Ascent"));
        // The non-observer wrapper also uses the checkpoint-local path table.
        assert_eq!(cache.get_path_by_guid(5), Some("/Game/Maps/Ascent/Ascent"));
        assert_eq!(cache.get_outer_guid(5), Some(NetworkGuid(7)));
        let g = cache
            .get_group_by_path("/Script/ShooterGame.Thing")
            .unwrap();
        assert_eq!(g.get_field(1).map(|f| f.name.as_str()), Some("Health"));
        assert_eq!(g.get_field(3).map(|f| f.name.as_str()), Some("Armor"));
        assert!(g.get_field(0).is_none(), "unexported slot must stay empty");
    }

    /// The trap that defeats a first implementation: reading the per-group
    /// count as a u32 doubles it and overruns. The frame-offset check is the
    /// only thing standing between that and well-formed nonsense, so it has to
    /// be seen failing.
    #[test]
    fn a_desynced_table_is_rejected_not_silently_accepted() {
        let mut archive = build(
            &[(7, 0, Some("/Game/X"), 0)],
            &[("/Script/G.Thing", 2, &[(0, "A")])],
            &[0u8; 16],
        );
        // Move the declared frame offset one byte on: the tables still parse,
        // and only the end-to-end check can tell.
        let w0 = u32::from_le_bytes(archive[0..4].try_into().unwrap());
        archive[0..4].copy_from_slice(&(w0 + 1).to_le_bytes());
        let mut cache = NetGuidCache::new();
        let err = read_checkpoint_tables(&archive, &mut cache).unwrap_err();
        assert!(
            matches!(err, SchemaError::CheckpointFrameOffsetMismatch { .. }),
            "expected a frame-offset mismatch, got {err}"
        );
    }

    /// The checkpoint reader discarded the FName number exactly as the
    /// ReplayData reader did, so two slots whose base string matches came out
    /// with one name. Both sites now render the same way: number 0 is the bare
    /// name, and any other number is the base plus `_{number - 1}`, which is
    /// how Unreal displays it.
    #[test]
    fn fname_numbers_survive_into_the_checkpoint_field_names() {
        // Hand-built: the shared `build` helper always writes number 0.
        let mut body = Vec::new();
        body.extend_from_slice(&1u32.to_le_bytes()); // one group
        push_fstring(&mut body, "/Script/G.Thing");
        push_packed(&mut body, 7);
        push_packed(&mut body, 2); // two slots
        for (slot, number) in [(0u32, 0i32), (1, 1)] {
            body.push(1); // exported
            push_packed(&mut body, slot);
            body.extend_from_slice(&0u32.to_le_bytes()); // checksum
            body.push(0); // FName: not hardcoded
            push_fstring(&mut body, "Value");
            body.extend_from_slice(&number.to_le_bytes());
        }

        let mut archive = Vec::new();
        archive.extend_from_slice(&((20 + body.len() - 8) as u32).to_le_bytes());
        archive.extend_from_slice(&[0u8; 12]);
        archive.extend_from_slice(&0u32.to_le_bytes()); // no guid entries
        archive.extend_from_slice(&body);

        let mut cache = NetGuidCache::new();
        read_checkpoint_tables(&archive, &mut cache).unwrap();

        let g = cache.get_group_by_path("/Script/G.Thing").unwrap();
        assert_eq!(g.get_field(0).map(|f| f.name.as_str()), Some("Value"));
        assert_eq!(g.get_field(1).map(|f| f.name.as_str()), Some("Value_0"));
    }

    /// A checkpoint restates the whole export map into a fresh cache, so two
    /// groups sharing a `path_name_index` inside ONE checkpoint is not the
    /// incremental re-export that [`NetGuidCache::add_export_group`] merges --
    /// it is two different paths claiming one slot.
    ///
    /// The probe's path half is the same case from the other side: one path,
    /// or a spelling of it the cache registers as an alias, declared again at
    /// another index. `add_export_group` would merge that without an error.
    #[test]
    fn two_groups_at_one_index_fail_before_returning_an_untrusted_cache() {
        // `build` writes path_name_index 7 for every group, so two groups is
        // exactly the collision.
        let archive = build(
            &[],
            &[("/Script/G.A", 0, &[]), ("/Script/G.B", 0, &[])],
            &[0u8; 8],
        );
        let mut cache = NetGuidCache::new();
        let err = read_checkpoint_tables(&archive, &mut cache).unwrap_err();

        assert!(matches!(
            err,
            SchemaError::CheckpointGroupCollision {
                path_name_index: 7,
                ..
            }
        ));

        for (first, second) in [
            ("/Script/G.A", "/Script/G.A"),
            (
                "/Game/Characters/Jett/Jett_C",
                "/Game/Characters/_Core/Jett/Jett_C",
            ),
        ] {
            let archive = groups_at(&[(first, 7), (second, 8)]);
            match read_checkpoint_tables(&archive, &mut NetGuidCache::new()) {
                Err(SchemaError::CheckpointGroupCollision {
                    path,
                    path_name_index,
                }) => assert_eq!(
                    (path.as_str(), path_name_index),
                    (second, 8),
                    "{first} then {second}"
                ),
                other => panic!("{first} then {second}: expected a collision, got {other:?}"),
            }
        }
    }

    /// Two groups at different indices are the ordinary case and must not be
    /// counted.
    #[test]
    fn two_groups_at_different_indices_are_not_a_collision() {
        let mut body = Vec::new();
        body.extend_from_slice(&2u32.to_le_bytes()); // two groups
        for (path, index) in [("/Script/G.A", 7u32), ("/Script/G.B", 8)] {
            push_fstring(&mut body, path);
            push_packed(&mut body, index);
            push_packed(&mut body, 0); // no field slots
        }

        let mut archive = Vec::new();
        archive.extend_from_slice(&((20 + body.len() - 8) as u32).to_le_bytes());
        archive.extend_from_slice(&[0u8; 12]);
        archive.extend_from_slice(&0u32.to_le_bytes()); // no guid entries
        archive.extend_from_slice(&body);

        let mut cache = NetGuidCache::new();
        let t = read_checkpoint_tables(&archive, &mut cache).unwrap();
        assert_eq!(t.group_count, 2);
        assert_eq!(t.group_collisions, 0);
        assert_eq!(cache.group_count(), 2);
    }

    #[test]
    fn a_third_path_discriminator_is_an_error() {
        let mut archive = build(&[(7, 0, Some("/Game/X"), 0)], &[], &[]);
        archive[22] = 2; // the PathIsString byte of entry 0
        let mut cache = NetGuidCache::new();
        let err = read_checkpoint_tables(&archive, &mut cache).unwrap_err();
        assert!(
            matches!(
                err,
                SchemaError::CheckpointBadPathKind { entry: 0, byte: 2 }
            ),
            "got {err}"
        );
    }

    #[test]
    fn a_nonzero_reserved_word_is_an_error() {
        let mut archive = build(&[], &[], &[]);
        archive[8..12].copy_from_slice(&7u32.to_le_bytes());
        let mut cache = NetGuidCache::new();
        let err = read_checkpoint_tables(&archive, &mut cache).unwrap_err();
        assert!(
            matches!(
                err,
                SchemaError::CheckpointReservedWordSet {
                    offset: 8,
                    value: 7
                }
            ),
            "got {err}"
        );
    }

    #[test]
    fn a_handle_that_is_not_its_slot_is_an_error() {
        // Hand-build one group whose single exported slot lies about its
        // handle. Attaching a real name to the wrong handle is the failure
        // mode this check exists for, and it reads as valid data.
        let mut body = Vec::new();
        body.extend_from_slice(&1u32.to_le_bytes()); // one group
        push_fstring(&mut body, "/Script/G.Thing");
        push_packed(&mut body, 7);
        push_packed(&mut body, 2); // two slots
        body.push(0); // slot 0 not exported
        body.push(1); // slot 1 exported
        push_packed(&mut body, 9); // ... but claims handle 9
        body.extend_from_slice(&0u32.to_le_bytes());
        body.push(1);
        push_packed(&mut body, 216);

        let mut archive = Vec::new();
        archive.extend_from_slice(&((20 + body.len() - 8) as u32).to_le_bytes());
        archive.extend_from_slice(&[0u8; 12]);
        archive.extend_from_slice(&0u32.to_le_bytes()); // no guid entries
        archive.extend_from_slice(&body);

        let mut cache = NetGuidCache::new();
        let err = read_checkpoint_tables(&archive, &mut cache).unwrap_err();
        assert!(
            matches!(
                err,
                SchemaError::CheckpointHandleNotSlot {
                    slot: 1,
                    handle: 9,
                    ..
                }
            ),
            "got {err}"
        );
    }
}
