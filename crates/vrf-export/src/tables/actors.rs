//! The `actors` table: one row per actor channel open, close or dormancy
//! (what it is for: `schema::actors_schema`).

use std::sync::Arc;

use arrow_array::RecordBatch;
use arrow_schema::Schema;

use super::batch;
use crate::error::ExportError;
use crate::record::ActorRecord;
use crate::schema::actors_schema_ref;
use crate::writer::{Table, TableWriter};

/// Rows per row group by default; the ~4 K rows of a match fit in one.
pub const DEFAULT_ACTOR_ROW_GROUP_SIZE: usize = 131_072;

/// Table marker for `actors`. See [`ActorWriter`].
pub struct ActorsTable;

/// Streaming Parquet writer for actor lifecycle records.
pub type ActorWriter<W> = TableWriter<ActorsTable, W>;

impl Table for ActorsTable {
    type Row = ActorRecord;

    const DEFAULT_ROW_GROUP_SIZE: usize = DEFAULT_ACTOR_ROW_GROUP_SIZE;

    // Dictionary/plain bytes over the 45-replay sample of
    // `Table::DICTIONARY_COLUMNS`. Strings, listed by rule: class_path 0.25,
    // archetype_path 0.28, event 0.29 (smaller on 44 of 45; its Arrow type
    // stays Utf8, which is the schema, not the page encoding). Number listed:
    // channel_index 0.84. Not listed, each smaller PLAIN on at least 44 of 45:
    // time_ms 1.77, packet_id 1.52, actor_net_guid 1.44, spawn_x 1.31, spawn_y
    // 1.30, spawn_yaw 1.26, spawn_pitch 1.15, spawn_z 1.11, spawn_roll 1.07.
    const DICTIONARY_COLUMNS: &'static [&'static str] =
        &["channel_index", "event", "class_path", "archetype_path"];

    fn schema() -> Arc<Schema> {
        actors_schema_ref()
    }

    fn initial_capacity(_batch_rows: usize) -> usize {
        4096
    }

    fn build_batch(rows: &[ActorRecord]) -> Result<RecordBatch, ExportError> {
        batch(Self::schema(), ActorRecord::columns(rows.iter()))
    }
}
