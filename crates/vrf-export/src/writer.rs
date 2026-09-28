//! The streaming writer every table shares: buffer rows, convert a batch to
//! Arrow when the buffer fills, finalise on `finish`. What differs per table
//! (schema, dictionary columns, rows to `RecordBatch`) is the [`Table`] trait.
//!
//! Two independent thresholds: [`MAX_BUFFERED_ROWS`] records are held before
//! conversion, which bounds memory; the row-group size is where `ArrowWriter`
//! cuts row groups, which shapes the file. Tables with large heap payloads add
//! a byte budget ([`Table::MAX_BUFFERED_BYTES`]), the only other thing that
//! closes a row group; filling a batch never does.

use std::io::Write;
use std::marker::PhantomData;
use std::sync::Arc;

use arrow_array::RecordBatch;
use arrow_schema::Schema;
use parquet::arrow::ArrowWriter;
use parquet::basic::Compression;
use parquet::file::properties::{EnabledStatistics, WriterProperties};
use parquet::schema::types::ColumnPath;

use crate::error::ExportError;

/// The mini-batch a Parquet column writer works in, after which it checks
/// whether a data page is full. Pinned, not left to the library default, since
/// [`MAX_BUFFERED_ROWS`] must stay a multiple of it for the bytes to hold.
pub const PARQUET_WRITE_BATCH_SIZE: usize = 1_024;

/// Rows held as records before being converted to an Arrow batch. **Not** the
/// row-group size: `ArrowWriter` cuts a row group at `max_row_group_row_count`
/// across batches, so 16 batches of 8,192 cut where one of 131,072 did.
///
/// It must be a multiple of [`PARQUET_WRITE_BATCH_SIZE`], which the assertion
/// below enforces: `write_batch` checks page limits after each mini-batch, and
/// a batch ending inside one adds a check point that can move a page boundary.
/// Measured on all 11 Parquet outputs of the reference replay:
///
/// | rows per batch | result |
/// |---|---|
/// | 131,072 (one per row group), 8,192, 3,072 (divides neither group size) | byte-identical |
/// | 3,000 | **bytes moved** in `fields`, `movement` and `checkpoint_fields` |
///
/// Against one batch per row group, five `export` runs of the reference replay
/// went from 172.0 to 105.9 MB peak working set and 1.456 to 1.281 s median
/// wall time: a whole row group is no longer held as records (~20 MB of
/// `FieldRecord`), doubled while its arrays are built. Much smaller batches pay
/// a fixed cost of building fourteen Arrow arrays and lose the dictionary
/// builders' capacity hints.
pub const MAX_BUFFERED_ROWS: usize = 8_192;

const _: () = assert!(
    MAX_BUFFERED_ROWS % PARQUET_WRITE_BATCH_SIZE == 0,
    "MAX_BUFFERED_ROWS must be a multiple of PARQUET_WRITE_BATCH_SIZE or the \
     Parquet output moves; see the table above this constant"
);

/// Everything the generic writer needs to know about one table, implemented
/// by a zero-sized marker per table (e.g. `FieldsTable`); the public writer
/// names are type aliases over [`TableWriter`].
pub trait Table {
    type Row;

    /// Rows per row group when the caller does not choose: peak memory
    /// against how large a column chunk ZSTD gets to work on.
    const DEFAULT_ROW_GROUP_SIZE: usize;

    /// The only columns written with a Parquet dictionary; every other column
    /// is PLAIN (then ZSTD, like everything). **Every string column is
    /// listed**, as the docs promise (the few that measured larger are named in
    /// docs/PERFORMANCE_NOTES.md); **any other column only where a dictionary
    /// measured smaller** than PLAIN over a 45-replay sample, each table's
    /// comment giving its dictionary/plain ratios.
    ///
    /// parquet-rs dictionary-encodes every non-boolean column by default, so
    /// [`TableWriter`] turns that off first; applied over the default, this
    /// list switched on only what was already on (method and totals:
    /// docs/PERFORMANCE_NOTES.md, "Dictionary encoding is chosen per column").
    /// parquet-rs also ignores a name matching no column and never gives a
    /// BOOLEAN column a dictionary; the roundtrip tests reject both and check
    /// every written file's dictionary pages against this list.
    const DICTIONARY_COLUMNS: &'static [&'static str];

    /// Optional retained-row byte budget; zero leaves row-count batching alone.
    ///
    /// Non-zero bounds [`Self::retained_bytes`] over every row not yet in a
    /// closed row group, buffered or already held encoded by the `ArrowWriter`.
    /// The row that would pass the budget starts a new group (a lone oversized
    /// row gets its own). Only the budget closes a group early: closing one per
    /// 8,192-row batch left `checkpoint_guid_entries.parquet` 928,714 bytes in
    /// ten groups against 396,821 in one (reference replay, `export
    /// --checkpoints`). The sum resets only when a group closes; reset per
    /// batch, an open group could hold 16 batches of just under the budget.
    ///
    /// Encoder state (dictionaries, page buffers) is not counted and is bounded
    /// by the row limit, as for every table. Against the per-batch close, on
    /// the largest corpus replay (13.04 `fce40cc5`, 113.6 MB), `export
    /// --checkpoints`, five alternating runs: peak commit 226.6 -> 240.5 MB
    /// median, peak working set 219.6 -> 230.9 MB; the main-only export, with
    /// no budgeted rows, measured the same (peak working set 187.3 vs 186.9 MB).
    const MAX_BUFFERED_BYTES: usize = 0;

    /// Bytes retained outside the row struct itself for budgeted tables.
    fn retained_bytes(_row: &Self::Row) -> usize {
        0
    }

    /// The Arrow schema. Must match [`Self::build_batch`]'s column order:
    /// `RecordBatch::try_new` checks only types, so two swapped same-typed
    /// columns would pass and silently corrupt the export.
    fn schema() -> Arc<Schema>;

    /// Rows to reserve up front. Small tables (actors, net_guids, events)
    /// never fill one batch, so they reserve less.
    fn initial_capacity(batch_rows: usize) -> usize {
        batch_rows
    }

    /// Convert a full buffer into one Arrow record batch.
    fn build_batch(rows: &[Self::Row]) -> Result<RecordBatch, ExportError>;
}

/// Streaming Parquet writer for one table.
///
/// # Usage
///
/// ```no_run
/// # use vrf_export::{FieldWriter, FieldRecord, ExportError};
/// # fn example() -> Result<(), ExportError> {
/// let file = std::fs::File::create("fields.parquet")?;
/// let mut writer = FieldWriter::new(file)?;
/// writer.push(FieldRecord {
///     time_ms: 1000, packet_id: 42, channel_index: 3,
///     actor_net_guid: 100, object_net_guid: None,
///     group_path: "PlayerState".into(),
///     handle: 7, field_name: Some("Health".into()),
///     compatible_checksum: Some(2_983_776_962),
///     bit_count: 32, raw_bits: Some(vec![0x64, 0, 0, 0].into()),
///     value_i64: Some(100), value_f64: None,
///     value_bool: None, value_str: None,
/// })?;
/// writer.finish()?;
/// # Ok(())
/// # }
/// ```
pub struct TableWriter<T: Table, W: Write + Send> {
    writer: ArrowWriter<W>,
    buffer: Vec<T::Row>,
    /// Rows per Arrow batch: [`MAX_BUFFERED_ROWS`], or the row-group size if
    /// smaller, so tiny row groups stay possible.
    batch_rows: usize,
    /// [`Table::retained_bytes`] over every row not yet in a closed row group,
    /// in `buffer` or in `writer`; zeroed only when that group closes. Used
    /// only with a byte budget ([`Table::MAX_BUFFERED_BYTES`]).
    pending_bytes: usize,
    _table: PhantomData<fn() -> T>,
}

impl<T: Table, W: Write + Send> TableWriter<T, W> {
    /// A writer with the table's defaults: ZSTD, page statistics, and a
    /// dictionary for exactly [`Table::DICTIONARY_COLUMNS`].
    pub fn new(sink: W) -> Result<Self, ExportError> {
        Self::with_row_group_size(sink, T::DEFAULT_ROW_GROUP_SIZE)
    }

    /// Create a writer with a custom row-group size. Smaller groups cost
    /// compression; memory changes only below [`MAX_BUFFERED_ROWS`], where one
    /// batch is one row group, so that constant's alignment rule does not
    /// apply: no batch ends inside a group.
    pub fn with_row_group_size(sink: W, row_group_size: usize) -> Result<Self, ExportError> {
        if row_group_size == 0 {
            return Err(ExportError::Usage(
                "row group size must be greater than zero".into(),
            ));
        }
        let schema = T::schema();
        let props = Self::writer_properties(row_group_size);
        let writer = ArrowWriter::try_new(sink, schema, Some(props))?;
        let batch_rows = row_group_size.min(MAX_BUFFERED_ROWS);
        Ok(Self {
            writer,
            buffer: Vec::with_capacity(T::initial_capacity(batch_rows)),
            batch_rows,
            pending_bytes: 0,
            _table: PhantomData,
        })
    }

    /// Push a single record, handing a full buffer over as a batch. The
    /// `ArrowWriter` closes the row group at its row limit; this closes one
    /// only for the byte budget ([`Table::MAX_BUFFERED_BYTES`]).
    pub fn push(&mut self, record: T::Row) -> Result<(), ExportError> {
        self.push_batch(std::iter::once(record))
    }

    /// Push every record in `records`, each exactly as [`Self::push`] would.
    pub fn push_batch(
        &mut self,
        records: impl IntoIterator<Item = T::Row>,
    ) -> Result<(), ExportError> {
        for record in records {
            self.flush_for_byte_budget(&record)?;
            self.pending_bytes = self
                .pending_bytes
                .saturating_add(T::retained_bytes(&record));
            self.buffer.push(record);
            self.flush_if_full_or_oversized()?;
        }
        Ok(())
    }

    /// Flush any remaining buffered rows and finalise the Parquet file. This
    /// **must** be called: a writer dropped without it leaves a truncated,
    /// unreadable file.
    pub fn finish(mut self) -> Result<(), ExportError> {
        if !self.buffer.is_empty() {
            self.flush_buffer()?;
        }
        self.writer.close()?;
        Ok(())
    }

    /// Number of rows currently buffered (not yet flushed).
    pub fn buffered_rows(&self) -> usize {
        self.buffer.len()
    }

    /// Close the open row group before `record` if adding it would take the
    /// pending bytes past the budget, so the row that tips it starts a new
    /// group. "Open" includes rows the `ArrowWriter` already holds: straight
    /// after a batch flush the buffer is empty but the row group is not.
    fn flush_for_byte_budget(&mut self, record: &T::Row) -> Result<(), ExportError> {
        let incoming = T::retained_bytes(record);
        if T::MAX_BUFFERED_BYTES != 0
            && (!self.buffer.is_empty() || self.writer.in_progress_rows() != 0)
            && self.pending_bytes.saturating_add(incoming) > T::MAX_BUFFERED_BYTES
        {
            self.close_row_group()?;
        }
        Ok(())
    }

    fn flush_if_full_or_oversized(&mut self) -> Result<(), ExportError> {
        if T::MAX_BUFFERED_BYTES != 0 && self.pending_bytes >= T::MAX_BUFFERED_BYTES {
            // The budget: the ArrowWriter must let go of these bytes too.
            self.close_row_group()
        } else if self.buffer.len() >= self.batch_rows {
            // The row count alone: the group stays open to its row limit.
            self.flush_buffer()
        } else {
            Ok(())
        }
    }

    /// Flush the buffer and close the open row group, releasing every byte
    /// `pending_bytes` counts.
    fn close_row_group(&mut self) -> Result<(), ExportError> {
        if !self.buffer.is_empty() {
            self.flush_buffer()?;
        }
        self.writer.flush()?;
        self.pending_bytes = 0;
        Ok(())
    }

    fn writer_properties(row_group_size: usize) -> WriterProperties {
        let mut builder = WriterProperties::builder()
            .set_max_row_group_row_count(Some(row_group_size))
            .set_compression(Compression::ZSTD(Default::default()))
            // Statistics on every column, for predicate pushdown.
            .set_statistics_enabled(EnabledStatistics::Page)
            // The library default, pinned: see PARQUET_WRITE_BATCH_SIZE.
            .set_write_batch_size(PARQUET_WRITE_BATCH_SIZE)
            // Off unless listed: parquet-rs defaults it on for every column
            // (`Table::DICTIONARY_COLUMNS`).
            .set_dictionary_enabled(false);
        for column in T::DICTIONARY_COLUMNS {
            builder = builder
                .set_column_dictionary_enabled(ColumnPath::new(vec![(*column).to_owned()]), true);
        }
        builder.build()
    }

    fn flush_buffer(&mut self) -> Result<(), ExportError> {
        let batch = T::build_batch(&self.buffer)?;
        // Cleared, not taken: one allocation for the whole run, and the rows
        // are gone before the encoder runs, so rows and arrays never coexist.
        self.buffer.clear();
        self.writer.write(&batch)?;
        // A group the ArrowWriter closed at its row limit is on the sink now.
        // The budgeted tables' 131,072 is 16 batches and a budget cut empties
        // both sides, so that limit falls between batches and this sees every
        // such close. A custom size that splits a batch keeps counting the
        // flushed head: an over-count, which can only close a later group early.
        if self.writer.in_progress_rows() == 0 {
            self.pending_bytes = 0;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow_array::BinaryArray;
    use arrow_schema::{DataType, Field};
    use parquet::file::reader::{FileReader, SerializedFileReader};

    /// The byte budget all four budgeted production tables declare.
    const BUDGET: usize = 8 * 1024 * 1024;

    /// A budgeted table whose retained bytes are its payload length, so a test
    /// can compute the row that crosses the budget. Budget and row-group size
    /// match partials and the three checkpoint declaration tables.
    struct Budgeted;

    impl Table for Budgeted {
        type Row = Vec<u8>;
        const DEFAULT_ROW_GROUP_SIZE: usize = 131_072;
        const DICTIONARY_COLUMNS: &'static [&'static str] = &[];
        const MAX_BUFFERED_BYTES: usize = BUDGET;

        fn retained_bytes(row: &Self::Row) -> usize {
            row.len()
        }

        fn schema() -> Arc<Schema> {
            Arc::new(Schema::new(vec![Field::new(
                "payload",
                DataType::Binary,
                false,
            )]))
        }

        fn build_batch(rows: &[Self::Row]) -> Result<RecordBatch, ExportError> {
            RecordBatch::try_new(
                Self::schema(),
                vec![Arc::new(BinaryArray::from_iter_values(
                    rows.iter().map(Vec::as_slice),
                ))],
            )
            .map_err(|e| ExportError::Parquet(e.into()))
        }
    }

    /// Every row group's row count in file order, after pushing `rows`: only
    /// the exact vector tells the candidate flush policies apart.
    fn row_group_rows(
        test: &str,
        row_group_size: usize,
        rows: impl IntoIterator<Item = Vec<u8>>,
    ) -> Vec<i64> {
        let path = std::env::temp_dir().join(format!(
            "vrfkit-writer-{test}-{}.parquet",
            std::process::id()
        ));
        let file = std::fs::File::create(&path).unwrap();
        let mut writer =
            TableWriter::<Budgeted, _>::with_row_group_size(file, row_group_size).unwrap();
        for row in rows {
            writer.push(row).unwrap();
        }
        writer.finish().unwrap();
        let counts = {
            let reader = SerializedFileReader::new(std::fs::File::open(&path).unwrap()).unwrap();
            reader
                .metadata()
                .row_groups()
                .iter()
                .map(|group| group.num_rows())
                .collect()
        };
        std::fs::remove_file(&path).unwrap();
        counts
    }

    #[test]
    fn row_count_flushes_leave_a_budgeted_row_group_open() {
        // ~0.5 MB against 8 MiB: nothing closes a group before the row limit.
        // A close per batch is what fragmented the budgeted tables.
        const ROWS: usize = 4 * MAX_BUFFERED_ROWS + 5;
        const _: () = assert!(ROWS * 16 < BUDGET / 10, "must stay far below the budget");
        assert_eq!(
            row_group_rows(
                "row-count",
                Budgeted::DEFAULT_ROW_GROUP_SIZE,
                (0..ROWS).map(|_| vec![7; 16]),
            ),
            vec![ROWS as i64]
        );
    }

    #[test]
    fn the_byte_budget_counts_rows_already_in_the_open_row_group() {
        // No single batch reaches the budget, but the open row group does: a
        // batch the ArrowWriter holds still counts until its group closes.
        const ROW: usize = 700;
        const _: () = assert!(
            MAX_BUFFERED_ROWS * ROW < BUDGET,
            "no single batch may do it"
        );
        // The first group holds the most rows whose total still fits.
        const FIRST: usize = BUDGET / ROW;
        const _: () = assert!(FIRST > MAX_BUFFERED_ROWS, "the cut must span a batch flush");
        const ROWS: usize = 20_000;
        assert_eq!(
            row_group_rows(
                "across-batches",
                Budgeted::DEFAULT_ROW_GROUP_SIZE,
                (0..ROWS).map(|_| vec![7; ROW]),
            ),
            vec![FIRST as i64, (ROWS - FIRST) as i64]
        );
    }

    #[test]
    fn a_row_group_closed_at_its_row_limit_resets_the_byte_budget() {
        // Two 16,384-row groups of ~4.9 MB each. Bytes of the first, closed at
        // its row limit, must stop counting, or they force a third group.
        const ROW: usize = 300;
        const GROUP: usize = 2 * MAX_BUFFERED_ROWS;
        const _: () = assert!(GROUP * ROW < BUDGET && 2 * GROUP * ROW > BUDGET);
        assert_eq!(
            row_group_rows("row-limit", GROUP, (0..2 * GROUP).map(|_| vec![7; ROW])),
            vec![GROUP as i64, GROUP as i64]
        );
    }

    #[test]
    fn a_row_that_would_overflow_the_open_row_group_starts_a_new_one() {
        // One full batch (8,192,000 bytes, under 8,388,608) leaves the buffer
        // empty, but the next row would take the open group past the budget,
        // so the group closes first. Consulting only the buffer gives [8193].
        const ROW: usize = 1_000;
        const BIG: usize = 200_000;
        const _: () =
            assert!(MAX_BUFFERED_ROWS * ROW < BUDGET && MAX_BUFFERED_ROWS * ROW + BIG > BUDGET);
        let rows = (0..MAX_BUFFERED_ROWS)
            .map(|_| vec![7; ROW])
            .chain(std::iter::once(vec![7; BIG]));
        assert_eq!(
            row_group_rows("empty-buffer", Budgeted::DEFAULT_ROW_GROUP_SIZE, rows),
            vec![MAX_BUFFERED_ROWS as i64, 1]
        );
    }
}
