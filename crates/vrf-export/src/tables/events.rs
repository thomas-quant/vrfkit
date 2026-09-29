//! The `events` table: one row per Event chunk, the server's own labelled
//! timeline (round starts, deaths, plants, defuses; see `schema::events_schema`).

use std::sync::Arc;

use arrow_array::RecordBatch;
use arrow_schema::Schema;

use super::batch;
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
        batch(Self::schema(), EventRecord::columns(rows.iter()))
    }
}
