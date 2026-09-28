//! The `net_guids` table: one row per NetGUID the replay registered, with its
//! object path and containing object (why: `schema::net_guids_schema`).

use std::sync::Arc;

use arrow_array::RecordBatch;
use arrow_schema::Schema;

use super::columns::{batch, net_guid_columns};
use crate::error::ExportError;
use crate::record::NetGuidRecord;
use crate::schema::net_guids_schema_ref;
use crate::writer::{Table, TableWriter};

/// Rows per row group by default; the ~16 K rows of a match fit in one.
pub const DEFAULT_NET_GUID_ROW_GROUP_SIZE: usize = 131_072;

/// Table marker for `net_guids`. See [`NetGuidWriter`].
pub struct NetGuidsTable;

/// Streaming Parquet writer for NetGUID registrations.
pub type NetGuidWriter<W> = TableWriter<NetGuidsTable, W>;

impl Table for NetGuidsTable {
    type Row = NetGuidRecord;

    const DEFAULT_ROW_GROUP_SIZE: usize = DEFAULT_NET_GUID_ROW_GROUP_SIZE;

    // Paths repeat heavily: 175 GUIDs share "FiringState" in one match. Listed
    // as a string by rule; measured, it is close to even (dictionary/plain
    // 0.98 over the 45-replay sample, smaller on 30 of 45). Not listed,
    // smaller PLAIN on all 45: outer_net_guid 2.07, net_guid 1.74.
    const DICTIONARY_COLUMNS: &'static [&'static str] = &["path"];

    fn schema() -> Arc<Schema> {
        net_guids_schema_ref()
    }

    fn initial_capacity(_batch_rows: usize) -> usize {
        4096
    }

    fn build_batch(rows: &[NetGuidRecord]) -> Result<RecordBatch, ExportError> {
        batch(
            net_guids_schema_ref(),
            net_guid_columns(rows.iter(), rows.len()),
        )
    }
}
