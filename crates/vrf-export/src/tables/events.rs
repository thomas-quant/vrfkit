//! The `events` table: one row per Event chunk.
//!
//! Event chunks are the server's own labelled game timeline. Round starts,
//! character deaths, spike plants and defuses arrive here already named, with a
//! millisecond timestamp, instead of having to be inferred from replicated
//! properties and RPCs.
//!
//! The bytes it wraps contain a group-dependent word list with no count on the
//! wire (see `vrf_container::EventChunk`). Groups with an established count
//! expose the structural tag, FString and trailing f32 alongside neutral word
//! columns; `raw_payload` keeps every byte either way.

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

/// Default row group size for the events table.
///
/// A full competitive match yields a couple of hundred rows, so this holds the
/// whole table in one row group while keeping the streaming shape the other
/// writers use.
pub const DEFAULT_EVENT_ROW_GROUP_SIZE: usize = 131_072;

/// Table marker for `events`. See [`EventWriter`].
pub struct EventsTable;

/// Streaming Parquet writer for Event chunks.
pub type EventWriter<W> = TableWriter<EventsTable, W>;

impl Table for EventsTable {
    type Row = EventRecord;

    const DEFAULT_ROW_GROUP_SIZE: usize = DEFAULT_EVENT_ROW_GROUP_SIZE;

    // Dictionary/plain bytes over the 45-replay sample (see
    // `Table::DICTIONARY_COLUMNS`). Strings, listed by rule: payload_name 0.51,
    // group 0.60. `id` 1.07 and `metadata` 1.49 are near-unique per row and
    // measured larger under a dictionary on all 45 replays; they stay listed
    // for the every-string-column rule (the table is ~200 rows per match).
    // Their Arrow type is plain Utf8 either way; that is the schema, not the
    // page encoding. Numbers listed: word1 0.74, payload_size 0.89, word0
    // 0.89, payload_tag 0.91. Not listed, smaller PLAIN on all 45: time1 1.32,
    // time2 1.32, payload_seconds 1.29, raw_payload 1.14.
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
