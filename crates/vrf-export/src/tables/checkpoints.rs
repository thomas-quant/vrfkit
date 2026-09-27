//! Checkpoint-scoped field, actor and NetGUID tables.

use std::sync::Arc;

use arrow_array::builder::StringDictionaryBuilder;
use arrow_array::types::Int32Type;
use arrow_array::{
    ArrayRef, BinaryArray, BooleanArray, Float32Array, Float64Array, Int32Array, Int64Array,
    RecordBatch, StringArray, UInt8Array, UInt32Array, UInt64Array,
};
use arrow_schema::Schema;

use crate::ExportError;
use crate::record::{
    CheckpointActorRecord, CheckpointBlockRecord, CheckpointExportFieldRecord,
    CheckpointExportGroupRecord, CheckpointFieldRecord, CheckpointGuidEntryRecord,
    CheckpointNetGuidRecord,
};
use crate::schema::{
    checkpoint_actors_schema_ref, checkpoint_blocks_schema_ref,
    checkpoint_export_fields_schema_ref, checkpoint_export_groups_schema_ref,
    checkpoint_fields_schema_ref, checkpoint_guid_entries_schema_ref,
    checkpoint_net_guids_schema_ref,
};
use crate::writer::{Table, TableWriter};

pub struct CheckpointFieldsTable;
pub struct CheckpointActorsTable;
pub struct CheckpointNetGuidsTable;
pub struct CheckpointBlocksTable;
pub struct CheckpointGuidEntriesTable;
pub struct CheckpointExportGroupsTable;
pub struct CheckpointExportFieldsTable;
pub type CheckpointFieldWriter<W> = TableWriter<CheckpointFieldsTable, W>;
pub type CheckpointActorWriter<W> = TableWriter<CheckpointActorsTable, W>;
pub type CheckpointNetGuidWriter<W> = TableWriter<CheckpointNetGuidsTable, W>;
pub type CheckpointBlockWriter<W> = TableWriter<CheckpointBlocksTable, W>;
pub type CheckpointGuidEntryWriter<W> = TableWriter<CheckpointGuidEntriesTable, W>;
pub type CheckpointExportGroupWriter<W> = TableWriter<CheckpointExportGroupsTable, W>;
pub type CheckpointExportFieldWriter<W> = TableWriter<CheckpointExportFieldsTable, W>;

fn identity_arrays<'a>(
    identities: impl Iterator<Item = &'a crate::record::CheckpointIdentity> + Clone,
) -> [ArrayRef; 2] {
    [
        Arc::new(UInt32Array::from_iter_values(
            identities.clone().map(|i| i.checkpoint_index),
        )),
        Arc::new(StringArray::from_iter_values(
            identities.map(|i| i.checkpoint_id.as_ref()),
        )),
    ]
}

impl Table for CheckpointGuidEntriesTable {
    type Row = CheckpointGuidEntryRecord;
    const DEFAULT_ROW_GROUP_SIZE: usize = 131_072;
    const MAX_BUFFERED_BYTES: usize = 8 * 1024 * 1024;
    // Dictionary/plain bytes over the 45-replay sample (see
    // `Table::DICTIONARY_COLUMNS`). Strings, listed by rule although both
    // measured larger under a dictionary on all 45 replays: checkpoint_id
    // 1.30, literal_path 1.19. Number listed: flags 0.44. Not listed, smaller
    // PLAIN on all 45: outer_net_guid 2.28, ordinal 2.14, net_guid 2.07,
    // checkpoint_index 1.42, name_index 1.16.
    //
    // Measured under the byte-budget flush, which cuts the reference replay's
    // 74,270 rows into 10 row groups. A dictionary is per row group, so what
    // it saves here depends on those cuts: re-measure this table, and the two
    // export-declaration tables below, if they change. docs/PERFORMANCE_NOTES.md
    // records a first check with one row group per table.
    const DICTIONARY_COLUMNS: &'static [&'static str] = &["checkpoint_id", "literal_path", "flags"];
    fn schema() -> Arc<Schema> {
        checkpoint_guid_entries_schema_ref()
    }
    fn retained_bytes(row: &Self::Row) -> usize {
        row.checkpoint.checkpoint_id.len() + row.literal_path.as_ref().map_or(0, String::len)
    }
    fn build_batch(rows: &[Self::Row]) -> Result<RecordBatch, ExportError> {
        let mut c = identity_arrays(rows.iter().map(|r| &r.checkpoint)).to_vec();
        c.extend([
            Arc::new(UInt32Array::from_iter_values(
                rows.iter().map(|r| r.ordinal),
            )) as ArrayRef,
            Arc::new(UInt32Array::from_iter_values(
                rows.iter().map(|r| r.net_guid),
            )),
            Arc::new(UInt32Array::from_iter_values(
                rows.iter().map(|r| r.outer_net_guid),
            )),
            Arc::new(BooleanArray::from_iter(
                rows.iter().map(|r| Some(r.path_is_string)),
            )),
            Arc::new(StringArray::from_iter(
                rows.iter().map(|r| r.literal_path.as_deref()),
            )),
            Arc::new(UInt32Array::from_iter(rows.iter().map(|r| r.name_index))),
            Arc::new(UInt8Array::from_iter_values(rows.iter().map(|r| r.flags))),
        ]);
        RecordBatch::try_new(Self::schema(), c).map_err(|e| ExportError::Parquet(e.into()))
    }
}

impl Table for CheckpointExportGroupsTable {
    type Row = CheckpointExportGroupRecord;
    const DEFAULT_ROW_GROUP_SIZE: usize = 131_072;
    const MAX_BUFFERED_BYTES: usize = 8 * 1024 * 1024;
    // Strings, listed by rule although both measured larger under a
    // dictionary on all 45 replays: checkpoint_id 1.34, group_path 1.25. No
    // number measured smaller as a dictionary; all smaller PLAIN on all 45:
    // ordinal 3.43, path_name_index 3.43, declared_slots 1.72,
    // checkpoint_index 1.33. Same row-group caveat as
    // `CheckpointGuidEntriesTable`.
    const DICTIONARY_COLUMNS: &'static [&'static str] = &["checkpoint_id", "group_path"];
    fn schema() -> Arc<Schema> {
        checkpoint_export_groups_schema_ref()
    }
    fn retained_bytes(row: &Self::Row) -> usize {
        row.checkpoint.checkpoint_id.len() + row.group_path.len()
    }
    fn build_batch(rows: &[Self::Row]) -> Result<RecordBatch, ExportError> {
        let mut c = identity_arrays(rows.iter().map(|r| &r.checkpoint)).to_vec();
        c.extend([
            Arc::new(UInt32Array::from_iter_values(
                rows.iter().map(|r| r.ordinal),
            )) as ArrayRef,
            Arc::new(UInt32Array::from_iter_values(
                rows.iter().map(|r| r.path_name_index),
            )),
            Arc::new(StringArray::from_iter_values(
                rows.iter().map(|r| r.group_path.as_str()),
            )),
            Arc::new(UInt32Array::from_iter_values(
                rows.iter().map(|r| r.declared_slots),
            )),
        ]);
        RecordBatch::try_new(Self::schema(), c).map_err(|e| ExportError::Parquet(e.into()))
    }
}

impl Table for CheckpointExportFieldsTable {
    type Row = CheckpointExportFieldRecord;
    const DEFAULT_ROW_GROUP_SIZE: usize = 131_072;
    const MAX_BUFFERED_BYTES: usize = 8 * 1024 * 1024;
    // Strings, listed by rule although all three measured larger under a
    // dictionary on all 45 replays: checkpoint_id 1.30, rendered_name 1.17,
    // fname_base 1.15. Number listed: fname_kind 0.34. Not listed:
    // group_ordinal 2.45, path_name_index 2.45, compatible_checksum 1.74,
    // checkpoint_index 1.44, exported_flag 1.37, slot 1.10, handle 1.10,
    // fname_index 1.08, fname_number 1.05. `slot` and `handle` split by build:
    // a dictionary is 0.89 on the 37 replays from 11.06-12.09, 12.10, 12.11,
    // 13.00 and 13.01, but 2.01 on the 8 from 13.02-13.06, which decides the
    // total. Same row-group caveat as `CheckpointGuidEntriesTable`.
    const DICTIONARY_COLUMNS: &'static [&'static str] =
        &["checkpoint_id", "rendered_name", "fname_kind", "fname_base"];
    fn schema() -> Arc<Schema> {
        checkpoint_export_fields_schema_ref()
    }
    fn retained_bytes(row: &Self::Row) -> usize {
        row.checkpoint.checkpoint_id.len()
            + row.rendered_name.len()
            + row.fname_base.as_ref().map_or(0, String::len)
    }
    fn build_batch(rows: &[Self::Row]) -> Result<RecordBatch, ExportError> {
        let mut c = identity_arrays(rows.iter().map(|r| &r.checkpoint)).to_vec();
        c.extend([
            Arc::new(UInt32Array::from_iter_values(
                rows.iter().map(|r| r.group_ordinal),
            )) as ArrayRef,
            Arc::new(UInt32Array::from_iter_values(
                rows.iter().map(|r| r.path_name_index),
            )),
            Arc::new(UInt32Array::from_iter_values(rows.iter().map(|r| r.slot))),
            Arc::new(UInt32Array::from_iter_values(rows.iter().map(|r| r.handle))),
            Arc::new(UInt32Array::from_iter_values(
                rows.iter().map(|r| r.compatible_checksum),
            )),
            Arc::new(StringArray::from_iter_values(
                rows.iter().map(|r| r.rendered_name.as_str()),
            )),
            Arc::new(UInt8Array::from_iter_values(
                rows.iter().map(|r| r.exported_flag),
            )),
            Arc::new(UInt8Array::from_iter_values(
                rows.iter().map(|r| r.fname_kind),
            )),
            Arc::new(StringArray::from_iter(
                rows.iter().map(|r| r.fname_base.as_deref()),
            )),
            Arc::new(UInt32Array::from_iter(rows.iter().map(|r| r.fname_index))),
            Arc::new(Int32Array::from_iter(rows.iter().map(|r| r.fname_number))),
        ]);
        RecordBatch::try_new(Self::schema(), c).map_err(|e| ExportError::Parquet(e.into()))
    }
}

impl Table for CheckpointBlocksTable {
    type Row = CheckpointBlockRecord;
    const DEFAULT_ROW_GROUP_SIZE: usize = 131_072;
    // Dictionary/plain bytes over the 45-replay sample (see
    // `Table::DICTIONARY_COLUMNS`). Strings, listed by rule:
    // group_resolution_source 0.17, resolved_group_path 0.34,
    // actor_archetype_outer_path 0.38, function_count_source 0.40,
    // actor_archetype_path 0.44, class_guid_path 0.60, actor_guid_path 0.63,
    // object_outer_path 0.68, object_guid_path 0.70, checkpoint_id 1.02.
    // Numbers listed: field_row_count 0.76, class_net_guid 0.91. Not listed:
    // block_index 4.18, field_row_start 2.83, object_net_guid 1.71,
    // outer_net_guid 1.52, actor_net_guid 1.50, packet_id 1.21, delete_flags
    // 1.12, function_count 1.12, checkpoint_index 1.10, time_ms 1.09,
    // channel_index 1.04.
    const DICTIONARY_COLUMNS: &'static [&'static str] = &[
        "checkpoint_id",
        "class_net_guid",
        "resolved_group_path",
        "group_resolution_source",
        "function_count_source",
        "actor_archetype_path",
        "actor_archetype_outer_path",
        "actor_guid_path",
        "class_guid_path",
        "object_guid_path",
        "object_outer_path",
        "field_row_count",
    ];
    fn schema() -> Arc<Schema> {
        checkpoint_blocks_schema_ref()
    }
    fn build_batch(rows: &[Self::Row]) -> Result<RecordBatch, ExportError> {
        let mut c = identity_arrays(rows.iter().map(|r| &r.checkpoint)).to_vec();
        macro_rules! values {
            ($ty:ty, $field:ident) => {
                Arc::new(<$ty>::from_iter_values(rows.iter().map(|r| r.$field))) as ArrayRef
            };
        }
        macro_rules! optional {
            ($ty:ty, $field:ident) => {
                Arc::new(<$ty>::from_iter(rows.iter().map(|r| r.$field))) as ArrayRef
            };
        }
        macro_rules! booleans {
            ($field:ident) => {
                Arc::new(BooleanArray::from_iter(rows.iter().map(|r| Some(r.$field)))) as ArrayRef
            };
        }
        c.extend([
            values!(UInt32Array, block_index),
            values!(UInt32Array, time_ms),
            values!(UInt32Array, packet_id),
            values!(UInt32Array, channel_index),
            values!(UInt32Array, actor_net_guid),
            optional!(UInt32Array, object_net_guid),
            optional!(UInt32Array, class_net_guid),
            optional!(UInt32Array, outer_net_guid),
            booleans!(has_rep_layout),
            booleans!(is_actor),
            booleans!(is_deleted),
            booleans!(is_stably_named),
            values!(UInt8Array, delete_flags),
        ]);
        c.push(Arc::new(StringArray::from_iter_values(
            rows.iter().map(|r| r.resolved_group_path.as_ref()),
        )));
        c.push(Arc::new(StringArray::from_iter_values(
            rows.iter().map(|r| r.group_resolution_source),
        )));
        c.push(booleans!(group_declared));
        c.push(booleans!(resolution_memo_hit));
        c.push(values!(UInt32Array, function_count));
        c.push(Arc::new(StringArray::from_iter_values(
            rows.iter().map(|r| r.function_count_source),
        )));
        for select in 0..6 {
            c.push(Arc::new(StringArray::from_iter(rows.iter().map(
                |r| match select {
                    0 => r.actor_archetype_path.as_deref(),
                    1 => r.actor_archetype_outer_path.as_deref(),
                    2 => r.actor_guid_path.as_deref(),
                    3 => r.class_guid_path.as_deref(),
                    4 => r.object_guid_path.as_deref(),
                    _ => r.object_outer_path.as_deref(),
                },
            ))));
        }
        c.push(values!(UInt64Array, field_row_start));
        c.push(values!(UInt32Array, field_row_count));
        RecordBatch::try_new(Self::schema(), c).map_err(|e| ExportError::Parquet(e.into()))
    }
}

impl Table for CheckpointFieldsTable {
    type Row = CheckpointFieldRecord;
    const DEFAULT_ROW_GROUP_SIZE: usize = 131_072;
    // Dictionary/plain bytes over the 45-replay sample (see
    // `Table::DICTIONARY_COLUMNS`). Strings, listed by rule: group_path 0.23,
    // field_name 0.48, value_str 0.52, checkpoint_id 0.56. Others listed:
    // raw_bits 0.51, handle 0.73, value_i64 0.73, bit_count 0.75, time_ms
    // 0.87, value_f64 0.87, checkpoint_index 0.91. Not listed: object_net_guid
    // 1.37, packet_id 1.34, compatible_checksum 1.33, channel_index 1.28,
    // actor_net_guid 1.08.
    //
    // Not the same list as `fields`: raw_bits and value_f64 go the other way
    // there. Checkpoints are snapshots that restate the same values -- on the
    // reference replay 343,683 raw_bits values hold 7,084 distinct payloads
    // here, against 321,735 distinct in 1,065,872 in `fields`. The old
    // strings-only list, had it been honoured, sums to 1.40x the size this
    // table had under the parquet-rs everything-dictionary default.
    const DICTIONARY_COLUMNS: &'static [&'static str] = &[
        "checkpoint_index",
        "checkpoint_id",
        "time_ms",
        "group_path",
        "handle",
        "field_name",
        "bit_count",
        "raw_bits",
        "value_i64",
        "value_f64",
        "value_str",
    ];
    fn schema() -> Arc<Schema> {
        checkpoint_fields_schema_ref()
    }
    fn build_batch(rows: &[Self::Row]) -> Result<RecordBatch, ExportError> {
        let len = rows.len();
        let mut columns = identity_arrays(rows.iter().map(|r| &r.checkpoint)).to_vec();
        let fields = rows.iter().map(|r| &r.field);
        columns.extend([
            Arc::new(UInt32Array::from_iter_values(
                fields.clone().map(|r| r.time_ms),
            )) as ArrayRef,
            Arc::new(UInt32Array::from_iter_values(
                fields.clone().map(|r| r.packet_id),
            )),
            Arc::new(UInt32Array::from_iter_values(
                fields.clone().map(|r| r.channel_index),
            )),
            Arc::new(UInt32Array::from_iter_values(
                fields.clone().map(|r| r.actor_net_guid),
            )),
            Arc::new(UInt32Array::from_iter(
                fields.clone().map(|r| r.object_net_guid),
            )),
        ]);
        let mut group = StringDictionaryBuilder::<Int32Type>::with_capacity(len, 256, len * 20);
        for r in fields.clone() {
            group.append_value(&r.group_path);
        }
        columns.push(Arc::new(group.finish()));
        columns.push(Arc::new(UInt32Array::from_iter_values(
            fields.clone().map(|r| r.handle),
        )));
        let mut name = StringDictionaryBuilder::<Int32Type>::with_capacity(len, 256, len * 16);
        for r in fields.clone() {
            match &r.field_name {
                Some(v) => name.append_value(v),
                None => name.append_null(),
            }
        }
        columns.push(Arc::new(name.finish()));
        columns.extend([
            Arc::new(UInt32Array::from_iter(
                fields.clone().map(|r| r.compatible_checksum),
            )) as ArrayRef,
            Arc::new(UInt32Array::from_iter_values(
                fields.clone().map(|r| r.bit_count),
            )),
            Arc::new(BinaryArray::from_iter(
                fields.clone().map(|r| r.raw_bits.as_deref()),
            )),
            Arc::new(Int64Array::from_iter(fields.clone().map(|r| r.value_i64))),
            Arc::new(Float64Array::from_iter(fields.clone().map(|r| r.value_f64))),
            Arc::new(BooleanArray::from_iter(
                fields.clone().map(|r| r.value_bool),
            )),
        ]);
        let mut value = StringDictionaryBuilder::<Int32Type>::with_capacity(len, 2048, len * 32);
        for r in fields {
            match &r.value_str {
                Some(v) => value.append_value(v),
                None => value.append_null(),
            }
        }
        columns.push(Arc::new(value.finish()));
        RecordBatch::try_new(Self::schema(), columns).map_err(|e| ExportError::Parquet(e.into()))
    }
}

impl Table for CheckpointActorsTable {
    type Row = CheckpointActorRecord;
    const DEFAULT_ROW_GROUP_SIZE: usize = 131_072;
    // Dictionary/plain bytes over the 45-replay sample (see
    // `Table::DICTIONARY_COLUMNS`). Strings, listed by rule: class_path 0.32,
    // archetype_path 0.39, and two that measured larger under a dictionary,
    // event 1.33 and checkpoint_id 1.37 (each under 200 bytes per file).
    // Numbers listed: spawn_y 0.77, spawn_x 0.78, spawn_yaw 0.80, spawn_z
    // 0.83, channel_index 0.98. Not listed: packet_id 1.93, actor_net_guid
    // 1.46, checkpoint_index 1.28, time_ms 1.22, spawn_pitch 1.07, spawn_roll
    // 1.07. The spawn columns go the other way from `actors` because each
    // checkpoint restates the same live actors: 2,549 spawn_x values hold 163
    // distinct ones on the reference replay.
    const DICTIONARY_COLUMNS: &'static [&'static str] = &[
        "checkpoint_id",
        "channel_index",
        "event",
        "class_path",
        "archetype_path",
        "spawn_x",
        "spawn_y",
        "spawn_z",
        "spawn_yaw",
    ];
    fn schema() -> Arc<Schema> {
        checkpoint_actors_schema_ref()
    }
    fn initial_capacity(_: usize) -> usize {
        4096
    }
    fn build_batch(rows: &[Self::Row]) -> Result<RecordBatch, ExportError> {
        let len = rows.len();
        let mut c = identity_arrays(rows.iter().map(|r| &r.checkpoint)).to_vec();
        let actors = rows.iter().map(|r| &r.actor);
        c.extend([
            Arc::new(UInt32Array::from_iter_values(
                actors.clone().map(|r| r.time_ms),
            )) as ArrayRef,
            Arc::new(UInt32Array::from_iter_values(
                actors.clone().map(|r| r.packet_id),
            )),
            Arc::new(UInt32Array::from_iter_values(
                actors.clone().map(|r| r.channel_index),
            )),
            Arc::new(UInt32Array::from_iter_values(
                actors.clone().map(|r| r.actor_net_guid),
            )),
            Arc::new(StringArray::from_iter_values(
                actors.clone().map(|r| r.event),
            )),
        ]);
        for select in [0, 1] {
            let mut b = StringDictionaryBuilder::<Int32Type>::with_capacity(len, 128, len * 30);
            for r in actors.clone() {
                let v = if select == 0 {
                    &r.class_path
                } else {
                    &r.archetype_path
                };
                match v {
                    Some(v) => b.append_value(v),
                    None => b.append_null(),
                }
            }
            c.push(Arc::new(b.finish()));
        }
        c.extend([
            Arc::new(Float32Array::from_iter(actors.clone().map(|r| r.spawn_x))) as ArrayRef,
            Arc::new(Float32Array::from_iter(actors.clone().map(|r| r.spawn_y))),
            Arc::new(Float32Array::from_iter(actors.clone().map(|r| r.spawn_z))),
            Arc::new(Float32Array::from_iter(
                actors.clone().map(|r| r.spawn_pitch),
            )),
            Arc::new(Float32Array::from_iter(actors.clone().map(|r| r.spawn_yaw))),
            Arc::new(Float32Array::from_iter(actors.map(|r| r.spawn_roll))),
        ]);
        RecordBatch::try_new(Self::schema(), c).map_err(|e| ExportError::Parquet(e.into()))
    }
}

impl Table for CheckpointNetGuidsTable {
    type Row = CheckpointNetGuidRecord;
    const DEFAULT_ROW_GROUP_SIZE: usize = 131_072;
    // Dictionary/plain bytes over the 45-replay sample (see
    // `Table::DICTIONARY_COLUMNS`). Strings, listed by rule: path 0.48,
    // checkpoint_id 0.69. Number listed: checkpoint_index 0.92. Not listed,
    // smaller PLAIN on all 45: net_guid 2.30, outer_net_guid 1.78.
    const DICTIONARY_COLUMNS: &'static [&'static str] =
        &["checkpoint_index", "checkpoint_id", "path"];
    fn schema() -> Arc<Schema> {
        checkpoint_net_guids_schema_ref()
    }
    fn initial_capacity(_: usize) -> usize {
        4096
    }
    fn build_batch(rows: &[Self::Row]) -> Result<RecordBatch, ExportError> {
        let mut c = identity_arrays(rows.iter().map(|r| &r.checkpoint)).to_vec();
        let records = rows.iter().map(|r| &r.net_guid);
        c.push(Arc::new(UInt32Array::from_iter_values(
            records.clone().map(|r| r.net_guid),
        )));
        let mut path =
            StringDictionaryBuilder::<Int32Type>::with_capacity(rows.len(), 1024, rows.len() * 40);
        for r in records.clone() {
            path.append_value(&r.path);
        }
        c.push(Arc::new(path.finish()));
        c.push(Arc::new(UInt32Array::from_iter(
            records.map(|r| r.outer_net_guid),
        )));
        RecordBatch::try_new(Self::schema(), c).map_err(|e| ExportError::Parquet(e.into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{ActorRecord, CheckpointIdentity, FieldRecord, NetGuidRecord};
    use crate::schema::fields_schema;
    use arrow_array::Array;
    use smallvec::smallvec;

    fn identities() -> [CheckpointIdentity; 2] {
        [
            CheckpointIdentity {
                checkpoint_index: 3,
                checkpoint_id: Arc::from("same"),
            },
            CheckpointIdentity {
                checkpoint_index: 4,
                checkpoint_id: Arc::from("same"),
            },
        ]
    }

    #[test]
    fn duplicate_wire_ids_remain_distinct_and_payload_values_are_unchanged() {
        let [a, b] = identities();
        let field = |checkpoint| CheckpointFieldRecord {
            checkpoint,
            field: FieldRecord {
                time_ms: 7,
                packet_id: 0,
                channel_index: 2,
                actor_net_guid: 9,
                object_net_guid: Some(10),
                group_path: Arc::from("group"),
                handle: 11,
                field_name: Some(Arc::from("field")),
                compatible_checksum: Some(12),
                bit_count: 5,
                raw_bits: Some(smallvec![0x15]),
                value_i64: Some(42),
                value_f64: None,
                value_bool: None,
                value_str: None,
            },
        };
        let batch =
            CheckpointFieldsTable::build_batch(&[field(a.clone()), field(b.clone())]).unwrap();
        assert_eq!(
            batch
                .column(0)
                .as_any()
                .downcast_ref::<UInt32Array>()
                .unwrap()
                .values(),
            &[3, 4]
        );
        assert_eq!(
            batch
                .column(1)
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap()
                .value(0),
            "same"
        );
        assert_eq!(
            batch
                .column(12)
                .as_any()
                .downcast_ref::<BinaryArray>()
                .unwrap()
                .value(1),
            &[0x15]
        );
        assert_eq!(
            batch
                .column(13)
                .as_any()
                .downcast_ref::<Int64Array>()
                .unwrap()
                .value(0),
            42
        );
        assert_eq!(
            fields_schema().fields()[0].name(),
            "time_ms",
            "main schema must not acquire checkpoint identity"
        );

        let actor = |checkpoint| CheckpointActorRecord {
            checkpoint,
            actor: ActorRecord {
                time_ms: 7,
                packet_id: 0,
                channel_index: 2,
                actor_net_guid: 9,
                event: "open",
                class_path: Some("class".into()),
                archetype_path: None,
                spawn_x: Some(1.0),
                spawn_y: None,
                spawn_z: None,
                spawn_pitch: None,
                spawn_yaw: None,
                spawn_roll: None,
            },
        };
        let actors =
            CheckpointActorsTable::build_batch(&[actor(a.clone()), actor(b.clone())]).unwrap();
        assert_eq!(
            actors
                .column(0)
                .as_any()
                .downcast_ref::<UInt32Array>()
                .unwrap()
                .values(),
            &[3, 4]
        );
        assert_eq!(
            actors
                .column(3)
                .as_any()
                .downcast_ref::<UInt32Array>()
                .unwrap()
                .values(),
            &[0, 0]
        );

        let guid = |checkpoint| CheckpointNetGuidRecord {
            checkpoint,
            net_guid: NetGuidRecord {
                net_guid: 9,
                path: "path".into(),
                outer_net_guid: Some(1),
            },
        };
        let guids = CheckpointNetGuidsTable::build_batch(&[guid(a), guid(b)]).unwrap();
        assert_eq!(
            guids
                .column(0)
                .as_any()
                .downcast_ref::<UInt32Array>()
                .unwrap()
                .values(),
            &[3, 4]
        );
        assert_eq!(
            guids
                .column(2)
                .as_any()
                .downcast_ref::<UInt32Array>()
                .unwrap()
                .values(),
            &[9, 9]
        );

        let [a, b] = identities();
        let block = |checkpoint| CheckpointBlockRecord {
            checkpoint,
            block_index: 0,
            time_ms: 7,
            packet_id: 0,
            channel_index: 2,
            actor_net_guid: 9,
            object_net_guid: Some(0),
            class_net_guid: Some(0),
            outer_net_guid: Some(9),
            has_rep_layout: true,
            is_actor: false,
            is_deleted: false,
            is_stably_named: false,
            delete_flags: 0,
            resolved_group_path: Arc::from("group"),
            group_resolution_source: "replay_declared_group",
            group_declared: true,
            resolution_memo_hit: false,
            function_count: 0,
            function_count_source: "rep_layout_not_applicable",
            actor_archetype_path: None,
            actor_archetype_outer_path: None,
            actor_guid_path: None,
            class_guid_path: None,
            object_guid_path: Some("object".into()),
            object_outer_path: None,
            field_row_start: 12,
            field_row_count: 2,
        };
        let blocks = CheckpointBlocksTable::build_batch(&[block(a), block(b)]).unwrap();
        assert_eq!(
            blocks
                .column(0)
                .as_any()
                .downcast_ref::<UInt32Array>()
                .unwrap()
                .values(),
            &[3, 4]
        );
        assert_eq!(
            blocks
                .column(8)
                .as_any()
                .downcast_ref::<UInt32Array>()
                .unwrap()
                .values(),
            &[0, 0]
        );
        assert_eq!(
            blocks
                .column(27)
                .as_any()
                .downcast_ref::<UInt64Array>()
                .unwrap()
                .values(),
            &[12, 12]
        );
    }

    #[test]
    fn declaration_tables_preserve_raw_forms_sparse_slots_and_empty_groups() {
        let checkpoint = identities()[0].clone();
        let guid_rows = [
            CheckpointGuidEntryRecord {
                checkpoint: checkpoint.clone(),
                ordinal: 0,
                net_guid: 7,
                outer_net_guid: 0,
                path_is_string: true,
                literal_path: Some("18".into()),
                name_index: None,
                flags: 0,
            },
            CheckpointGuidEntryRecord {
                checkpoint: checkpoint.clone(),
                ordinal: 1,
                net_guid: 8,
                outer_net_guid: 7,
                path_is_string: false,
                literal_path: None,
                name_index: Some(18),
                flags: 3,
            },
        ];
        let guids = CheckpointGuidEntriesTable::build_batch(&guid_rows).unwrap();
        let path_is_string = guids
            .column_by_name("path_is_string")
            .unwrap()
            .as_any()
            .downcast_ref::<BooleanArray>()
            .unwrap();
        let name_index = guids
            .column_by_name("name_index")
            .unwrap()
            .as_any()
            .downcast_ref::<UInt32Array>()
            .unwrap();
        let flags = guids
            .column_by_name("flags")
            .unwrap()
            .as_any()
            .downcast_ref::<UInt8Array>()
            .unwrap();
        assert_eq!(
            [path_is_string.value(0), path_is_string.value(1)],
            [true, false]
        );
        assert!(name_index.is_null(0));
        assert_eq!(name_index.value(1), 18);
        assert_eq!(flags.values(), &[0, 3]);

        let groups = CheckpointExportGroupsTable::build_batch(&[CheckpointExportGroupRecord {
            checkpoint: checkpoint.clone(),
            ordinal: 4,
            path_name_index: 18,
            group_path: "18".into(),
            declared_slots: 0,
        }])
        .unwrap();
        assert_eq!(
            groups
                .column_by_name("declared_slots")
                .unwrap()
                .as_any()
                .downcast_ref::<UInt32Array>()
                .unwrap()
                .value(0),
            0
        );

        let fields = CheckpointExportFieldsTable::build_batch(&[CheckpointExportFieldRecord {
            checkpoint,
            group_ordinal: 9,
            path_name_index: 18,
            slot: 6,
            handle: 31,
            compatible_checksum: 0x1234,
            rendered_name: "44".into(),
            exported_flag: 2,
            fname_kind: 1,
            fname_base: None,
            fname_index: Some(44),
            fname_number: None,
        }])
        .unwrap();
        assert_eq!(
            fields
                .column_by_name("slot")
                .unwrap()
                .as_any()
                .downcast_ref::<UInt32Array>()
                .unwrap()
                .value(0),
            6
        );
        assert_eq!(
            fields
                .column_by_name("exported_flag")
                .unwrap()
                .as_any()
                .downcast_ref::<UInt8Array>()
                .unwrap()
                .value(0),
            2
        );
        assert_eq!(
            fields
                .column_by_name("fname_index")
                .unwrap()
                .as_any()
                .downcast_ref::<UInt32Array>()
                .unwrap()
                .value(0),
            44
        );
    }
}
