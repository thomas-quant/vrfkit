//! The `movement` table: one row per movement sample, the densest data in a
//! replay (~1.8 M rows a match).

use std::sync::Arc;

use arrow_array::RecordBatch;
use arrow_schema::Schema;

use super::batch;
use crate::error::ExportError;
use crate::record::MovementRecord;
use crate::schema::movement_schema_ref;
use crate::writer::{Table, TableWriter};

/// Rows per row group by default, 256 Ki: a row is 50 bytes uncompressed
/// (twelve 4-byte columns, two 1-byte), so ~13 MB a group, a good ZSTD chunk.
pub const DEFAULT_MOVEMENT_ROW_GROUP_SIZE: usize = 262_144;

/// Table marker for `movement`. See [`MovementWriter`].
pub struct MovementTable;

/// Streaming Parquet writer for movement records.
///
/// # Usage
///
/// ```no_run
/// # use vrf_export::{MovementWriter, MovementRecord, ExportError};
/// # fn example() -> Result<(), ExportError> {
/// let file = std::fs::File::create("movement.parquet")?;
/// let mut writer = MovementWriter::new(file)?;
/// writer.push(MovementRecord {
///     time_ms: 5000, packet_id: 100, character_net_guid: 42,
///     pos_x: 1000.0, pos_y: 2000.0, pos_z: 300.0,
///     yaw: 45.0, pitch: 350.0,
///     vel_x: 100.0, vel_y: 0.0, vel_z: 0.0,
///     timestamp: 31_337, movement_state: 2, move_type: 1,
/// })?;
/// writer.finish()?;
/// # Ok(())
/// # }
/// ```
pub type MovementWriter<W> = TableWriter<MovementTable, W>;

impl Table for MovementTable {
    type Row = MovementRecord;

    const DEFAULT_ROW_GROUP_SIZE: usize = DEFAULT_MOVEMENT_ROW_GROUP_SIZE;

    // No strings, so every entry is measured (dictionary/plain bytes over the
    // 45-replay sample of `Table::DICTIONARY_COLUMNS`). Listed: move_type 0.76,
    // movement_state 0.80, vel_y 0.92, vel_x 0.93, vel_z 0.93,
    // character_net_guid 0.95. Not listed, smaller PLAIN on all 45: packet_id
    // 5.19, time_ms 5.12, timestamp 3.81, pos_x 1.79, pos_y 1.79, yaw 1.44,
    // pos_z 1.42, pitch 1.36. On the reference replay pos_x holds 153-163 K
    // distinct values in every full 256 Ki-row group (~650 KB of dictionary,
    // under the 1 MiB fallback), so a dictionary wrote 18-bit indices that hide
    // the sample-to-sample locality ZSTD uses on raw floats; vel_x has ~14 K.
    const DICTIONARY_COLUMNS: &'static [&'static str] = &[
        "character_net_guid",
        "vel_x",
        "vel_y",
        "vel_z",
        "movement_state",
        "move_type",
    ];

    fn schema() -> Arc<Schema> {
        movement_schema_ref()
    }

    fn build_batch(rows: &[MovementRecord]) -> Result<RecordBatch, ExportError> {
        batch(Self::schema(), MovementRecord::columns(rows.iter()))
    }
}
