//! The `events` table: one row per Event chunk, the server's own labelled
//! timeline (round starts, deaths, plants, defuses; see `schema::events_schema`).

use std::sync::Arc;

use arrow_array::{
    ArrayRef, BinaryArray, Float32Array, Int32Array, RecordBatch, StringArray, UInt32Array,
};
use arrow_schema::Schema;

use super::columns::{batch, dict};
use crate::error::ExportError;
use crate::record::EventRecord;
use crate::schema::events_schema_ref;
use crate::writer::{Table, TableWriter};

/// Rows per row group by default; a match's couple of hundred rows fit in one.
pub const DEFAULT_EVENT_ROW_GROUP_SIZE: usize = 131_072;

/// Table marker for `events`. See [`EventWriter`].
pub struct EventsTable;

/// Streaming Parquet writer for Event chunks.
pub type EventWriter<W> = TableWriter<EventsTable, W>;

impl Table for EventsTable {
    type Row = EventRecord;

    const DEFAULT_ROW_GROUP_SIZE: usize = DEFAULT_EVENT_ROW_GROUP_SIZE;

    // Dictionary/plain bytes over the 45-replay sample of
    // `Table::DICTIONARY_COLUMNS`. Strings, listed by rule: payload_name 0.51,
    // group 0.60, and id 1.07 and metadata 1.49, near-unique per row and larger
    // as a dictionary on all 45 (a table of ~200 rows a match). Numbers listed:
    // word1 0.74, payload_size 0.89, word0 0.89, payload_tag 0.91. Not listed,
    // smaller PLAIN on all 45: time1 1.32, time2 1.32, payload_seconds 1.29,
    // raw_payload 1.14.
    const DICTIONARY_COLUMNS: &'static [&'static str] = &[
        "id",
        "group",
        "metadata",
        "payload_size",
        "word0",
        "word1",
        "payload_tag",
        "payload_name",
    ];

    fn schema() -> Arc<Schema> {
        events_schema_ref()
    }

    fn initial_capacity(_batch_rows: usize) -> usize {
        256
    }

    fn build_batch(rows: &[EventRecord]) -> Result<RecordBatch, ExportError> {
        let len = rows.len();
        batch(
            events_schema_ref(),
            vec![
                Arc::new(StringArray::from_iter_values(
                    rows.iter().map(|r| r.id.as_str()),
                )) as ArrayRef,
                dict(
                    len,
                    16,
                    len * 24,
                    rows.iter().map(|r| Some(r.group.as_str())),
                ),
                Arc::new(StringArray::from_iter_values(
                    rows.iter().map(|r| r.metadata.as_str()),
                )),
                Arc::new(UInt32Array::from_iter_values(rows.iter().map(|r| r.time1))),
                Arc::new(UInt32Array::from_iter_values(rows.iter().map(|r| r.time2))),
                Arc::new(Int32Array::from_iter_values(
                    rows.iter().map(|r| r.payload_size),
                )),
                Arc::new(BinaryArray::from_iter_values(
                    rows.iter().map(|r| r.raw_payload.as_slice()),
                )),
                Arc::new(UInt32Array::from_iter(rows.iter().map(|r| r.word0))),
                Arc::new(UInt32Array::from_iter(rows.iter().map(|r| r.word1))),
                Arc::new(UInt32Array::from_iter(rows.iter().map(|r| r.payload_tag))),
                Arc::new(StringArray::from_iter(
                    rows.iter().map(|r| r.payload_name.as_deref()),
                )),
                Arc::new(Float32Array::from_iter(
                    rows.iter().map(|r| r.payload_seconds),
                )),
            ],
        )
    }
}
