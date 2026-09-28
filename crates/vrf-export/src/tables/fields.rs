//! The `fields` table: one row per decoded field, RPC parameter, or preserved
//! whole-block payload.

use std::sync::Arc;

use arrow_array::RecordBatch;
use arrow_schema::Schema;

use super::columns::{batch, field_columns};
use crate::error::ExportError;
use crate::record::FieldRecord;
use crate::schema::fields_schema_ref;
use crate::writer::{Table, TableWriter};

/// Rows per row group by default, 128 Ki: column chunks large enough for ZSTD
/// to reach steady state.
pub const DEFAULT_ROW_GROUP_SIZE: usize = 131_072;

/// Table marker for `fields`. See [`FieldWriter`].
pub struct FieldsTable;

/// Streaming Parquet writer for field records.
pub type FieldWriter<W> = TableWriter<FieldsTable, W>;

impl Table for FieldsTable {
    type Row = FieldRecord;

    const DEFAULT_ROW_GROUP_SIZE: usize = DEFAULT_ROW_GROUP_SIZE;

    // Dictionary/plain bytes over the 45-replay sample of
    // `Table::DICTIONARY_COLUMNS`; below 1.00 the dictionary is smaller.
    // Strings, listed by rule: group_path 0.27 and field_name 0.44 (~475 group
    // paths and a few thousand names over 1.2 M rows), value_str 0.92
    // (repeating enum strings and JSON). Numbers listed: handle 0.66,
    // channel_index 0.76, actor_net_guid 0.87, value_i64 0.94, bit_count 0.95.
    // Not listed, smaller PLAIN on all 45: time_ms 3.16, packet_id 2.37,
    // value_f64 1.32, object_net_guid 1.16, raw_bits 1.16, compatible_checksum
    // 1.07.
    const DICTIONARY_COLUMNS: &'static [&'static str] = &[
        "channel_index",
        "actor_net_guid",
        "group_path",
        "handle",
        "field_name",
        "bit_count",
        "value_i64",
        "value_str",
    ];

    fn schema() -> Arc<Schema> {
        fields_schema_ref()
    }

    fn build_batch(rows: &[FieldRecord]) -> Result<RecordBatch, ExportError> {
        batch(fields_schema_ref(), field_columns(rows.iter(), rows.len()))
    }
}
