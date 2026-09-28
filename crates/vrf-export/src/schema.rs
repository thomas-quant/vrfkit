//! Arrow schema definitions for the 13 export tables, defined once so writer
//! and reader agree, and public so a consumer can check a file it did not
//! write. Every schema compiles whenever `parquet` is on, whatever writer
//! features are: a schema is a few `Field`s, and reading `fields.parquet`
//! should not need its writer.

use arrow_schema::{DataType, Field, Schema};
use std::sync::Arc;

/// Declare each schema's `Arc` twin, which `ArrowWriter` takes. Hand-written,
/// a mismatched pair would compile (all return `Arc<Schema>`) and write the
/// wrong column types; here a pairing is one line. Names stay greppable.
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

/// `Dictionary<Int32, Utf8>`, the Arrow type of every dictionary string column.
fn dict_utf8() -> DataType {
    DataType::Dictionary(Box::new(DataType::Int32), Box::new(DataType::Utf8))
}

/// `base` with the two checkpoint identity columns in front of its own.
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
    checkpoint_schema(Schema::new(vec![
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
    ]))
}

pub fn checkpoint_guid_entries_schema() -> Schema {
    checkpoint_schema(Schema::new(vec![
        Field::new("ordinal", DataType::UInt32, false),
        Field::new("net_guid", DataType::UInt32, false),
        Field::new("outer_net_guid", DataType::UInt32, false),
        Field::new("path_is_string", DataType::Boolean, false),
        Field::new("literal_path", DataType::Utf8, true),
        Field::new("name_index", DataType::UInt32, true),
        Field::new("flags", DataType::UInt8, false),
    ]))
}

pub fn checkpoint_export_groups_schema() -> Schema {
    checkpoint_schema(Schema::new(vec![
        Field::new("ordinal", DataType::UInt32, false),
        Field::new("path_name_index", DataType::UInt32, false),
        Field::new("group_path", DataType::Utf8, false),
        Field::new("declared_slots", DataType::UInt32, false),
    ]))
}

pub fn checkpoint_export_fields_schema() -> Schema {
    checkpoint_schema(Schema::new(vec![
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
    ]))
}

/// Schema for the `fields` table (long format). An unresolved ClassNetCache
/// block is one explicitly marked row, not split into fabricated fields.
///
/// The address columns (time, packet, channel, actor, group, handle, name)
/// come first, so predicate pushdown on actor or group uses row-group
/// statistics without reading values; the sparse value columns end the row.
/// What each null means: the [`crate::record::FieldRecord`] field docs.
pub fn fields_schema() -> Schema {
    Schema::new(vec![
        Field::new("time_ms", DataType::UInt32, false),
        Field::new("packet_id", DataType::UInt32, false),
        Field::new("channel_index", DataType::UInt32, false),
        Field::new("actor_net_guid", DataType::UInt32, false),
        Field::new("object_net_guid", DataType::UInt32, true),
        Field::new("group_path", dict_utf8(), false),
        Field::new("handle", DataType::UInt32, false),
        Field::new("field_name", dict_utf8(), true),
        Field::new("compatible_checksum", DataType::UInt32, true),
        Field::new("bit_count", DataType::UInt32, false),
        Field::new("raw_bits", DataType::Binary, true),
        // Sparse typed-value overlay -- at most one of these is non-null per row.
        Field::new("value_i64", DataType::Int64, true),
        Field::new("value_f64", DataType::Float64, true),
        Field::new("value_bool", DataType::Boolean, true),
        // Dictionary-typed although the most varied string column: its values
        // (enum strings, JSON blobs) repeat heavily.
        Field::new("value_str", dict_utf8(), true),
    ])
}

/// Schema for the `movement` table: every row dense, no nulls.
///
/// Unreal's left-handed Z-up coordinates: positions in cm, velocity in cm/s,
/// yaw and pitch in degrees `[0, 360)` -- a `u16` scaled by `360/65536`, never
/// negative, so a downward pitch reads near 360, not as a small negative.
///
/// The last three columns are appended rather than interleaved: consumers
/// address movement columns by position, and a `timestamp` beside `time_ms`
/// would silently repoint every downstream `column(3)`/`column(8)`.
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
        // The move header's server tick, unlike `time_ms`, the replay-relative
        // packet time this crate stamps on.
        Field::new("timestamp", DataType::UInt32, false),
        // One wire byte each, kept as u8 (`move_type` is effectively a bool).
        Field::new("movement_state", DataType::UInt8, false),
        Field::new("move_type", DataType::UInt8, false),
    ])
}

/// Schema for the `actors` table (one row per channel open, close or
/// dormancy). It shows actors that never replicate a field -- weapon and
/// ability instances, DefuserItem, HeavyArmorItem -- which `fields.parquet`
/// alone cannot resolve. Static actors and close rows have no spawn data.
pub fn actors_schema() -> Schema {
    Schema::new(vec![
        Field::new("time_ms", DataType::UInt32, false),
        Field::new("packet_id", DataType::UInt32, false),
        Field::new("channel_index", DataType::UInt32, false),
        Field::new("actor_net_guid", DataType::UInt32, false),
        // Plain Utf8 in Arrow; the Parquet column still gets a dictionary,
        // which measured smaller (`ActorsTable::DICTIONARY_COLUMNS`).
        Field::new("event", DataType::Utf8, false),
        Field::new("class_path", dict_utf8(), true),
        Field::new("archetype_path", dict_utf8(), true),
        Field::new("spawn_x", DataType::Float32, true),
        Field::new("spawn_y", DataType::Float32, true),
        Field::new("spawn_z", DataType::Float32, true),
        Field::new("spawn_pitch", DataType::Float32, true),
        Field::new("spawn_yaw", DataType::Float32, true),
        Field::new("spawn_roll", DataType::Float32, true),
    ])
}

/// Schema for the `net_guids` table (one row per registered NetGUID): the
/// replay's own object registry, GUID to object path and containing object.
/// `actors.parquet` covers only GUIDs that opened a channel, not subobjects: a
/// weapon's `FiringState` appears in no other table, and without the outer
/// chain no route leads from a shot event to the equippable that fired it.
/// `outer_net_guid` is null, not 0, when no outer is declared: 0 is the
/// engine's "invalid" sentinel, a different statement.
pub fn net_guids_schema() -> Schema {
    Schema::new(vec![
        Field::new("net_guid", DataType::UInt32, false),
        Field::new("path", dict_utf8(), false),
        Field::new("outer_net_guid", DataType::UInt32, true),
    ])
}

/// Schema for the `events` table (one row per Event chunk): the server's own
/// labelled timeline, which the rest of the pipeline only reconstructs from
/// RPCs. The six header fields and `raw_payload`, every byte verbatim, are
/// never null (an empty `metadata` or payload is empty, not missing).
///
/// A payload's word count is not self-describing (`vrf_container::EventChunk`),
/// so the structural overlay (tag, FString, trailing f32 and the words, which
/// keep neutral names until independent evidence gives them a meaning) is
/// filled only for groups of established arity, and stays null rather than
/// guessing for an unknown or changed one.
pub fn events_schema() -> Schema {
    Schema::new(vec![
        Field::new("id", DataType::Utf8, false),
        // ~7 distinct groups in a file.
        Field::new("group", dict_utf8(), false),
        Field::new("metadata", DataType::Utf8, false),
        Field::new("time1", DataType::UInt32, false),
        Field::new("time2", DataType::UInt32, false),
        // The wire's i32 SizeInBytes: redundant with `raw_payload`'s length,
        // but readable from row-group statistics without the binary column.
        Field::new("payload_size", DataType::Int32, false),
        Field::new("raw_payload", DataType::Binary, false),
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
