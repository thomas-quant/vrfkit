//! The `movement` table: one row per movement sample.
//!
//! Movement samples are the densest data in a replay (~1.8 M rows per match).
//! Every row has the same fixed schema with no nulls, which makes this table a
//! textbook case for columnar compression: each f32 column compresses very well
//! under ZSTD because adjacent position samples differ by small deltas.

use std::sync::Arc;

use arrow_array::{ArrayRef, Float32Array, RecordBatch, UInt8Array, UInt32Array};
use arrow_schema::Schema;

use super::columns::batch;
use crate::error::ExportError;
use crate::record::MovementRecord;
use crate::schema::movement_schema_ref;
use crate::writer::{Table, TableWriter};

/// Default row group size for movement data.
///
/// Movement rows are smaller (11 x 4 bytes + 1 x 4 + 2 x 1 = 50 bytes per row
/// uncompressed), so we can afford a larger row group without excessive memory
/// use. 256 Ki rows approximately 13 MB uncompressed per row group -- a good
/// chunk size for ZSTD.
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
///     yaw: 45.0, pitch: -10.0,
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

    // No strings here, so every entry is measured (dictionary/plain bytes over
    // the 45-replay sample; see `Table::DICTIONARY_COLUMNS`). Listed, where the
    // dictionary measured smaller: move_type 0.76, movement_state 0.80, vel_y
    // 0.92, vel_x 0.93, vel_z 0.93, character_net_guid 0.95. Not listed,
    // smaller PLAIN on all 45 replays: packet_id 5.19, time_ms 5.12, timestamp
    // 3.81, pos_x 1.79, pos_y 1.79, yaw 1.44, pos_z 1.42, pitch 1.36. On the
    // reference replay pos_x holds 153-163 K distinct values in every full
    // 256 Ki-row group (~650 KB of dictionary, under the 1 MiB fallback
    // limit), so it was written as 18-bit indices that hide the
    // sample-to-sample locality ZSTD uses on the raw floats; vel_x has ~14 K.
    //
    // This list used to be empty with the comment "there is nothing to
    // dictionary", while parquet-rs's default dictionary-encoded all 14
    // columns. See `Table::DICTIONARY_COLUMNS`.
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
        // Order must match movement_schema() exactly -- RecordBatch::try_new
        // only checks types, so a swap between two same-typed columns (e.g.
        // movement_state and move_type) would pass and corrupt the export.
        batch(
            movement_schema_ref(),
            vec![
                Arc::new(UInt32Array::from_iter_values(
                    rows.iter().map(|r| r.time_ms),
                )) as ArrayRef,
                Arc::new(UInt32Array::from_iter_values(
                    rows.iter().map(|r| r.packet_id),
                )),
                Arc::new(UInt32Array::from_iter_values(
                    rows.iter().map(|r| r.character_net_guid),
                )),
                Arc::new(Float32Array::from_iter_values(rows.iter().map(|r| r.pos_x))),
                Arc::new(Float32Array::from_iter_values(rows.iter().map(|r| r.pos_y))),
                Arc::new(Float32Array::from_iter_values(rows.iter().map(|r| r.pos_z))),
                Arc::new(Float32Array::from_iter_values(rows.iter().map(|r| r.yaw))),
                Arc::new(Float32Array::from_iter_values(rows.iter().map(|r| r.pitch))),
                Arc::new(Float32Array::from_iter_values(rows.iter().map(|r| r.vel_x))),
                Arc::new(Float32Array::from_iter_values(rows.iter().map(|r| r.vel_y))),
                Arc::new(Float32Array::from_iter_values(rows.iter().map(|r| r.vel_z))),
                Arc::new(UInt32Array::from_iter_values(
                    rows.iter().map(|r| r.timestamp),
                )),
                Arc::new(UInt8Array::from_iter_values(
                    rows.iter().map(|r| r.movement_state),
                )),
                Arc::new(UInt8Array::from_iter_values(
                    rows.iter().map(|r| r.move_type),
                )),
            ],
        )
    }
}
