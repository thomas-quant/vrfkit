//! Arrow schema definitions for the five export tables.
//!
//! Schemas are defined once here so that writer and reader agree; the module is
//! public so a consumer can validate a file it did not write against the same
//! definition. The field metadata (e.g. `PARQUET:field_id`) is intentionally
//! omitted -- Parquet assigns ordinal field IDs automatically, and manual IDs
//! would only matter if we needed Iceberg-style schema evolution, which we
//! don't.
//!
//! Every schema is compiled in whenever the `parquet` feature is on, including
//! for tables whose writer feature is off: a schema is a few `Field`s and no
//! code, and a caller reading `fields.parquet` should not have to enable the
//! writer to get its column types.

use arrow_schema::{DataType, Field, Schema};
use std::sync::Arc;

/// Declare the `Arc`-wrapping companion of each table schema.
///
/// `ArrowWriter` takes a `SchemaRef`, so every schema below needs an
/// `Arc::new` twin. Thirteen hand-written twins is thirteen chances to wrap
/// the wrong schema, and the type system cannot catch that one -- all of them
/// return `Arc<Schema>`, so a mismatched pair compiles and writes a file with
/// the wrong column types. Pairing them here makes that a one-line diff to
/// read rather than a body to compare.
///
/// The names are spelled out rather than derived, so grepping for
/// `fields_schema_ref` still lands on its definition.
macro_rules! schema_refs {
    ($($ref_fn:ident => $schema_fn:ident),+ $(,)?) => {
        $(
            pub fn $ref_fn() -> Arc<Schema> {
                Arc::new($schema_fn())
            }
        )+
    };
}

schema_refs! {
    checkpoint_fields_schema_ref => checkpoint_fields_schema,
    checkpoint_actors_schema_ref => checkpoint_actors_schema,
    checkpoint_net_guids_schema_ref => checkpoint_net_guids_schema,
    checkpoint_blocks_schema_ref => checkpoint_blocks_schema,
    checkpoint_guid_entries_schema_ref => checkpoint_guid_entries_schema,
    checkpoint_export_groups_schema_ref => checkpoint_export_groups_schema,
    checkpoint_export_fields_schema_ref => checkpoint_export_fields_schema,
    fields_schema_ref => fields_schema,
    movement_schema_ref => movement_schema,
    actors_schema_ref => actors_schema,
    net_guids_schema_ref => net_guids_schema,
    events_schema_ref => events_schema,
    partials_schema_ref => partials_schema,
}

fn checkpoint_schema(base: Schema) -> Schema {
    let mut fields = Vec::with_capacity(base.fields().len() + 2);
    fields.push(Field::new("checkpoint_index", DataType::UInt32, false));
    fields.push(Field::new("checkpoint_id", DataType::Utf8, false));
    fields.extend(base.fields().iter().map(|field| field.as_ref().clone()));
    Schema::new(fields)
}

pub fn checkpoint_fields_schema() -> Schema {
    checkpoint_schema(fields_schema())
}

pub fn checkpoint_actors_schema() -> Schema {
    checkpoint_schema(actors_schema())
}

pub fn checkpoint_net_guids_schema() -> Schema {
    checkpoint_schema(net_guids_schema())
}

pub fn checkpoint_blocks_schema() -> Schema {
    Schema::new(vec![
        Field::new("checkpoint_index", DataType::UInt32, false),
        Field::new("checkpoint_id", DataType::Utf8, false),
        Field::new("block_index", DataType::UInt32, false),
        Field::new("time_ms", DataType::UInt32, false),
        Field::new("packet_id", DataType::UInt32, false),
        Field::new("channel_index", DataType::UInt32, false),
        Field::new("actor_net_guid", DataType::UInt32, false),
        Field::new("object_net_guid", DataType::UInt32, true),
        Field::new("class_net_guid", DataType::UInt32, true),
        Field::new("outer_net_guid", DataType::UInt32, true),
        Field::new("has_rep_layout", DataType::Boolean, false),
        Field::new("is_actor", DataType::Boolean, false),
        Field::new("is_deleted", DataType::Boolean, false),
        Field::new("is_stably_named", DataType::Boolean, false),
        Field::new("delete_flags", DataType::UInt8, false),
        Field::new("resolved_group_path", DataType::Utf8, false),
        Field::new("group_resolution_source", DataType::Utf8, false),
        Field::new("group_declared", DataType::Boolean, false),
        Field::new("resolution_memo_hit", DataType::Boolean, false),
        Field::new("function_count", DataType::UInt32, false),
        Field::new("function_count_source", DataType::Utf8, false),
        Field::new("actor_archetype_path", DataType::Utf8, true),
        Field::new("actor_archetype_outer_path", DataType::Utf8, true),
        Field::new("actor_guid_path", DataType::Utf8, true),
        Field::new("class_guid_path", DataType::Utf8, true),
        Field::new("object_guid_path", DataType::Utf8, true),
        Field::new("object_outer_path", DataType::Utf8, true),
        Field::new("field_row_start", DataType::UInt64, false),
        Field::new("field_row_count", DataType::UInt32, false),
    ])
}

pub fn checkpoint_guid_entries_schema() -> Schema {
    Schema::new(vec![
        Field::new("checkpoint_index", DataType::UInt32, false),
        Field::new("checkpoint_id", DataType::Utf8, false),
        Field::new("ordinal", DataType::UInt32, false),
        Field::new("net_guid", DataType::UInt32, false),
        Field::new("outer_net_guid", DataType::UInt32, false),
        Field::new("path_is_string", DataType::Boolean, false),
        Field::new("literal_path", DataType::Utf8, true),
        Field::new("name_index", DataType::UInt32, true),
        Field::new("flags", DataType::UInt8, false),
    ])
}
pub fn checkpoint_export_groups_schema() -> Schema {
    Schema::new(vec![
        Field::new("checkpoint_index", DataType::UInt32, false),
        Field::new("checkpoint_id", DataType::Utf8, false),
        Field::new("ordinal", DataType::UInt32, false),
        Field::new("path_name_index", DataType::UInt32, false),
        Field::new("group_path", DataType::Utf8, false),
        Field::new("declared_slots", DataType::UInt32, false),
    ])
}
pub fn checkpoint_export_fields_schema() -> Schema {
    Schema::new(vec![
        Field::new("checkpoint_index", DataType::UInt32, false),
        Field::new("checkpoint_id", DataType::Utf8, false),
        Field::new("group_ordinal", DataType::UInt32, false),
        Field::new("path_name_index", DataType::UInt32, false),
        Field::new("slot", DataType::UInt32, false),
        Field::new("handle", DataType::UInt32, false),
        Field::new("compatible_checksum", DataType::UInt32, false),
        Field::new("rendered_name", DataType::Utf8, false),
        Field::new("exported_flag", DataType::UInt8, false),
        Field::new("fname_kind", DataType::UInt8, false),
        Field::new("fname_base", DataType::Utf8, true),
        Field::new("fname_index", DataType::UInt32, true),
        Field::new("fname_number", DataType::Int32, true),
    ])
}
/// Schema for the `fields` table (long format).
///
/// Most rows represent one decoded field. A whole ClassNetCache block whose
/// function table is unresolved is preserved as one explicitly marked row;
/// it is not split into fabricated fields. The schema itself stays unchanged.
///
/// Column ordering is deliberate: the "address" columns come first (time,
/// packet, channel, actor, group, handle, name) so that predicate pushdown on
/// actor or group benefits from row-group statistics without reading value
/// columns. The sparse value columns at the end compress to near-zero when
/// null.
pub fn fields_schema() -> Schema {
    Schema::new(vec![
        Field::new("time_ms", DataType::UInt32, false),
        Field::new("packet_id", DataType::UInt32, false),
        Field::new("channel_index", DataType::UInt32, false),
        Field::new("actor_net_guid", DataType::UInt32, false),
        // Nullable: only subobject blocks carry one, and null must stay
        // distinguishable from 0 (the engine's invalid-GUID sentinel).
        Field::new("object_net_guid", DataType::UInt32, true),
        // Dictionary<Int32, Utf8>: ~300 distinct group paths over 780k rows.
        Field::new(
            "group_path",
            DataType::Dictionary(Box::new(DataType::Int32), Box::new(DataType::Utf8)),
            false,
        ),
        Field::new("handle", DataType::UInt32, false),
        // Nullable because the field name may be unknown (unmapped export index).
        Field::new(
            "field_name",
            DataType::Dictionary(Box::new(DataType::Int32), Box::new(DataType::Utf8)),
            true,
        ),
        // The replay's own `compatible_checksum` for this handle. Nullable, and
        // the null is information: it means the replay declares no checksum
        // here (array leaves and struct blobs are addressed inside a payload,
        // not by a declared handle), not that the export failed to carry one.
        // See `FieldRecord::compatible_checksum`.
        Field::new("compatible_checksum", DataType::UInt32, true),
        Field::new("bit_count", DataType::UInt32, false),
        // Raw bit payload; nullable because zero-bit fields carry no data.
        Field::new("raw_bits", DataType::Binary, true),
        // Sparse typed-value overlay -- at most one of these is non-null per row.
        Field::new("value_i64", DataType::Int64, true),
        Field::new("value_f64", DataType::Float64, true),
        Field::new("value_bool", DataType::Boolean, true),
        // Dictionary<Int32, Utf8>: the decoded values repeat heavily (enum
        // strings, JSON blobs), so a dictionary shrinks the column even though
        // it is the highest-cardinality of the three string columns.
        Field::new(
            "value_str",
            DataType::Dictionary(Box::new(DataType::Int32), Box::new(DataType::Utf8)),
            true,
        ),
    ])
}

/// Schema for the `movement` table (fixed format: every row is identical
/// structure, no nulls).
///
/// The coordinate system matches Unreal Engine's left-handed Z-up convention.
/// Positions are in centimetres; yaw/pitch are in degrees, unsigned and
/// wrapped to `[0, 360)` -- decoded from a `u16` scaled by `360/65536`, never
/// negative. A downward pitch near straight-down does not read as a small
/// negative number here; it wraps to a value near 360.
/// Velocity is cm/s as reported by the replication channel.
///
/// The last three columns are appended rather than interleaved: existing
/// consumers address movement columns positionally, so inserting `timestamp`
/// next to `time_ms` (where it reads more naturally) would silently repoint
/// every downstream `column(3)`/`column(8)` at the wrong data.
///
/// `mode_flags` is deliberately absent. The decoder exposes it on
/// `MovementMove`, but it is assigned from the same local as `movement_state`
/// at the struct's only construction site, so no code path can make the two
/// differ -- exporting it would add a byte-identical column over ~1.8 M rows.
pub fn movement_schema() -> Schema {
    Schema::new(vec![
        Field::new("time_ms", DataType::UInt32, false),
        Field::new("packet_id", DataType::UInt32, false),
        Field::new("character_net_guid", DataType::UInt32, false),
        Field::new("pos_x", DataType::Float32, false),
        Field::new("pos_y", DataType::Float32, false),
        Field::new("pos_z", DataType::Float32, false),
        Field::new("yaw", DataType::Float32, false),
        Field::new("pitch", DataType::Float32, false),
        Field::new("vel_x", DataType::Float32, false),
        Field::new("vel_y", DataType::Float32, false),
        Field::new("vel_z", DataType::Float32, false),
        // Server-assigned tick from the move header. Distinct from `time_ms`,
        // which is the replay-relative packet time this crate stamps on.
        Field::new("timestamp", DataType::UInt32, false),
        // One wire byte, so UInt8 -- widening would cost 3 bytes per row
        // before compression for no added range. See docs/USAGE.md
        // "movement.parquet" for why it reads 0 on every corpus row and why
        // posture must come from `bCrouchHeld` instead.
        Field::new("movement_state", DataType::UInt8, false),
        // Effectively a bool, but kept as the decoder's u8 so the column
        // stays a faithful copy of the wire value. See docs/USAGE.md
        // "movement.parquet" for the variant-0/variant-1 meaning and the
        // corpus measurement backing it.
        Field::new("move_type", DataType::UInt8, false),
    ])
}

/// Schema for the `actors` table (one row per channel open or close).
///
/// This table makes actors visible even if they never replicate a single
/// field -- e.g. weapon/ability instances, DefuserItem, HeavyArmorItem.
/// Without it, only actors that produce at least one field row in
/// `fields.parquet` can be resolved downstream.
///
/// Spawn location and rotation are nullable because static actors and
/// channel-close rows do not carry spatial data.
pub fn actors_schema() -> Schema {
    Schema::new(vec![
        Field::new("time_ms", DataType::UInt32, false),
        Field::new("packet_id", DataType::UInt32, false),
        Field::new("channel_index", DataType::UInt32, false),
        Field::new("actor_net_guid", DataType::UInt32, false),
        // "open", "close", or "dormant". Plain Utf8 in Arrow; the Parquet
        // column is still dictionary-encoded, which measured smaller -- see
        // `ActorsTable::DICTIONARY_COLUMNS`.
        Field::new("event", DataType::Utf8, false),
        // Nullable: class path may be unresolvable for some actors.
        Field::new(
            "class_path",
            DataType::Dictionary(Box::new(DataType::Int32), Box::new(DataType::Utf8)),
            true,
        ),
        // Nullable: archetype path may be absent (static actors).
        Field::new(
            "archetype_path",
            DataType::Dictionary(Box::new(DataType::Int32), Box::new(DataType::Utf8)),
            true,
        ),
        // Spawn location (nullable -- only present for dynamic actor opens).
        Field::new("spawn_x", DataType::Float32, true),
        Field::new("spawn_y", DataType::Float32, true),
        Field::new("spawn_z", DataType::Float32, true),
        // Spawn rotation (nullable).
        Field::new("spawn_pitch", DataType::Float32, true),
        Field::new("spawn_yaw", DataType::Float32, true),
        Field::new("spawn_roll", DataType::Float32, true),
    ])
}

/// Schema for the `net_guids` table (one row per registered NetGUID).
///
/// This is the replay's own object registry: which GUID maps to which object
/// path, and which object contains it. `actors.parquet` only covers GUIDs that
/// opened a channel, which excludes subobjects -- a weapon's `FiringState`
/// appears in no other table. Without the outer chain there is no route from a
/// shot event to the equippable that fired it.
///
/// `outer_net_guid` is nullable rather than zero-filled: GUID 0 is the engine's
/// "invalid" sentinel, so collapsing "no outer declared" onto 0 would erase the
/// distinction between an unknown parent and an explicitly invalid one.
pub fn net_guids_schema() -> Schema {
    Schema::new(vec![
        Field::new("net_guid", DataType::UInt32, false),
        // Paths repeat heavily (175 GUIDs share "FiringState" in one match).
        Field::new(
            "path",
            DataType::Dictionary(Box::new(DataType::Int32), Box::new(DataType::Utf8)),
            false,
        ),
        Field::new("outer_net_guid", DataType::UInt32, true),
    ])
}

/// Schema for the `events` table (one row per Event chunk).
///
/// Event chunks are the server's own labelled timeline -- the ground truth the
/// rest of the pipeline only reconstructs indirectly from RPCs. The six header
/// fields are decoded. For groups whose word count has been established, the
/// inner payload's structural tag, FString and trailing f32 are also exposed;
/// group-dependent words retain their neutral `word0`/`word1` names unless
/// independent evidence establishes a meaning.
///
/// `raw_payload` remains the whole payload verbatim. Its word count is not
/// self-describing (see `vrf_container::EventChunk`), so a group whose arity is
/// unknown leaves all structural overlay columns null rather than guessing.
///
/// The six outer fields and `raw_payload` are non-nullable: an empty `metadata`
/// is an empty string on the wire, and a zero-length payload is an empty blob.
/// The structural overlay columns are nullable because an unknown or changed
/// group deliberately falls back to raw-only preservation.
pub fn events_schema() -> Schema {
    Schema::new(vec![
        Field::new("id", DataType::Utf8, false),
        // ~7 distinct groups over the whole file; dictionary is nearly free.
        Field::new(
            "group",
            DataType::Dictionary(Box::new(DataType::Int32), Box::new(DataType::Utf8)),
            false,
        ),
        Field::new("metadata", DataType::Utf8, false),
        Field::new("time1", DataType::UInt32, false),
        Field::new("time2", DataType::UInt32, false),
        // The declared SizeInBytes, kept as the wire's i32. Redundant with
        // `raw_payload`'s length by construction, but readable from row-group
        // statistics without touching the binary column.
        Field::new("payload_size", DataType::Int32, false),
        Field::new("raw_payload", DataType::Binary, false),
        // The first two payload words (after the u32 group tag), for groups
        // whose word count is structurally fixed. Nullable: most groups carry
        // zero or one. `raw_payload` still keeps every byte.
        Field::new("word0", DataType::UInt32, true),
        Field::new("word1", DataType::UInt32, true),
        Field::new("payload_tag", DataType::UInt32, true),
        Field::new("payload_name", DataType::Utf8, true),
        Field::new("payload_seconds", DataType::Float32, true),
    ])
}

pub fn partials_schema() -> Schema {
    Schema::new(vec![
        Field::new("source", DataType::Utf8, false),
        Field::new("checkpoint_id", DataType::Utf8, true),
        Field::new("payload_kind", DataType::Utf8, false),
        Field::new("reason", DataType::Utf8, false),
        Field::new("source_packet_id", DataType::Int32, false),
        Field::new("source_payload_bit_offset", DataType::Int64, false),
        Field::new("rejection_packet_id", DataType::Int32, true),
        Field::new("channel_index", DataType::UInt32, false),
        Field::new("channel_sequence", DataType::Int32, false),
        Field::new("open", DataType::Boolean, false),
        Field::new("close", DataType::Boolean, false),
        Field::new("dormant", DataType::Boolean, false),
        Field::new("replication_paused", DataType::Boolean, false),
        Field::new("reliable", DataType::Boolean, false),
        Field::new("partial", DataType::Boolean, false),
        Field::new("partial_initial", DataType::Boolean, false),
        Field::new("partial_final", DataType::Boolean, false),
        Field::new("has_package_map_exports", DataType::Boolean, false),
        Field::new("has_must_be_mapped_guids", DataType::Boolean, false),
        Field::new("close_reason", DataType::UInt8, false),
        Field::new("source_payload_bit_count", DataType::Int32, false),
        Field::new("bit_count", DataType::UInt64, false),
        Field::new("raw_bits", DataType::Binary, false),
    ])
}
