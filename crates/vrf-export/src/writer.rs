//! The streaming writer every table shares.
//!
//! All five tables have the same shape: buffer rows, convert a batch of them to
//! Arrow when the buffer fills, finalise on `finish`. Only three things differ
//! -- the Arrow schema, the columns worth dictionary-encoding, and how a slice
//! of rows becomes a `RecordBatch`. Those three are the [`Table`] trait;
//! everything else lives here once.
//!
//! Two thresholds, not one, and they are independent. [`MAX_BUFFERED_ROWS`] is
//! how many records are held before conversion -- what bounds this crate's
//! memory. The row-group size is what `ArrowWriter` cuts row groups at -- what
//! shapes the file. A multi-million-row export holds one batch, not one row
//! group and not the whole table.
//!
//! A table whose rows carry large heap payloads adds a third, a byte budget
//! ([`Table::MAX_BUFFERED_BYTES`]). It is the only thing besides the row-group
//! size that closes a row group; filling a batch never does.

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

/// The mini-batch a Parquet column writer works in, pinned rather than taken
/// from the library default.
///
/// It matters here because it is the granularity at which the column writer
/// asks "is this data page full yet". See [`MAX_BUFFERED_ROWS`]: leaving it
/// implicit would put a byte-level invariant of this crate's output in another
/// crate's default value.
pub const PARQUET_WRITE_BATCH_SIZE: usize = 1_024;

/// Rows held as records before being converted to an Arrow batch.
///
/// This is **not** the row-group size. `ArrowWriter` accumulates the batches it
/// is given and cuts a row group when `max_row_group_row_count` is reached, so
/// feeding it sixteen batches of 8,192 puts the row-group boundaries in exactly
/// the same places as one batch of 131,072 did.
///
/// # The invariant: a multiple of [`PARQUET_WRITE_BATCH_SIZE`]
///
/// Batch size is **not** free of the output bytes, and assuming it was is a
/// mistake this constant is here to prevent repeating. `write_batch` splits its
/// input into mini-batches of [`PARQUET_WRITE_BATCH_SIZE`] and evaluates the
/// data-page limits after each one, so the set of value offsets at which a page
/// may be cut is the set of multiples of that size -- *unless* a batch ends
/// part-way through one, which introduces an extra, differently-placed check
/// point and can move a page boundary.
///
/// Measured on the reference replay, all 11 Parquet outputs:
///
/// | rows per batch | result |
/// |---|---|
/// | 131,072 (the old behaviour, one batch per row group) | byte-identical |
/// | 8,192 = 8 x 1,024 | byte-identical |
/// | 3,072 = 3 x 1,024, and no divisor of either row group | byte-identical |
/// | 3,000 | **bytes moved**, in `fields`, `movement` and `checkpoint_fields` |
///
/// So the constraint is alignment to the mini-batch, not any relationship to
/// the row-group size. The assertion below enforces it at compile time.
///
/// # What it buys
///
/// Peak memory. Holding a whole row group as records cost ~20 MB of
/// `FieldRecord` plus their heap payloads, and `build_batch` doubles that for
/// the duration of the conversion because the rows and the arrays are both
/// live. This change on its own, five runs of `export` on the reference replay
/// with the previous binary as the only difference, took peak working set from
/// 172.0 MB to 105.9 MB and median wall time from 1.456 s to 1.281 s. It is the
/// single largest memory win in the rewrite.
///
/// Not smaller: below a few thousand rows the fixed cost of building fourteen
/// Arrow arrays starts to show up against the per-row work, and the dictionary
/// builders' capacity hints stop being useful.
pub const MAX_BUFFERED_ROWS: usize = 8_192;

const _: () = assert!(
    MAX_BUFFERED_ROWS % PARQUET_WRITE_BATCH_SIZE == 0,
    "MAX_BUFFERED_ROWS must be a multiple of PARQUET_WRITE_BATCH_SIZE or the \
     Parquet output moves; see the table above this constant"
);

/// Everything the generic writer needs to know about one table.
///
/// Implemented by a zero-sized marker type per table (e.g. `FieldsTable`); the
/// public writer names are type aliases over [`TableWriter`].
pub trait Table {
    /// The record type callers push.
    type Row;

    /// Rows per row group when the caller does not choose.
    ///
    /// Sized per table: it trades peak memory against how large a column chunk
    /// ZSTD gets to work on.
    const DEFAULT_ROW_GROUP_SIZE: usize;

    /// Columns to dictionary-encode. Dictionary is Parquet's default for Utf8,
    /// but the tables pin it explicitly so the intent is in the source.
    const DICTIONARY_COLUMNS: &'static [&'static str];

    /// Optional retained-row byte budget. Zero leaves row-count batching unchanged.
    ///
    /// Non-zero bounds [`Self::retained_bytes`] summed over every row not yet
    /// in a closed row group: the rows still buffered as records *and* the
    /// rows already handed to the `ArrowWriter`, which holds them encoded in
    /// memory until their row group closes. Before a row that would take that
    /// sum past the budget, the buffer is flushed and the row group closed; a
    /// single row larger than the budget is allowed and gets a row group of
    /// its own.
    ///
    /// Only the budget closes a row group early. A batch flush because the
    /// buffer reached [`MAX_BUFFERED_ROWS`] leaves the row group open, exactly
    /// as it does for an unbudgeted table. It used to close it as well, which
    /// cut every budgeted table into 8,192-row row groups and cost them
    /// compression -- on the reference replay's `export --checkpoints`,
    /// `checkpoint_guid_entries.parquet` went from 928,714 bytes in ten row
    /// groups to 396,821 in one once it stopped.
    ///
    /// The sum is kept across batch flushes, and reset only when a row group
    /// closes, because resetting it per batch would have made that fix a
    /// memory regression: the open row group could then hold 131,072 / 8,192
    /// = 16 batches of just under the budget each, sixteen times what the
    /// budget promises.
    ///
    /// The open row group still costs memory the budget does not count: its
    /// encoders' state (dictionaries, page buffers), bounded by the row limit
    /// as for every unbudgeted table. The per-batch close had been keeping
    /// that at 8,192 rows' worth. Measured on the largest corpus replay (13.04
    /// `fce40cc5`, 113.6 MB), `export --checkpoints`, five alternating runs
    /// against the previous binary: peak commit 226.6 -> 240.5 MB median, peak
    /// working set 219.6 -> 230.9 MB. The main-only export, which has no
    /// budgeted rows there, measured the same on both (peak working set 187.3
    /// vs 186.9 MB).
    const MAX_BUFFERED_BYTES: usize = 0;

    /// Bytes retained outside the row struct itself for budgeted tables.
    fn retained_bytes(_row: &Self::Row) -> usize {
        0
    }

    /// The Arrow schema. Must match [`Self::build_batch`]'s column order:
    /// `RecordBatch::try_new` only checks types, so swapping two same-typed
    /// columns would pass and silently corrupt the export.
    fn schema() -> Arc<Schema>;

    /// How many rows to reserve in the in-memory buffer up front.
    ///
    /// Small tables (actors, net_guids, events) never fill even one batch, so
    /// reserving a whole one would be dead memory.
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
    /// Rows per Arrow batch. See [`MAX_BUFFERED_ROWS`]; never larger than the
    /// row-group size, so a caller asking for tiny row groups still gets them.
    batch_rows: usize,
    /// Reachable only through `finish_ref`. `finish` takes `self` by value, so
    /// after it nothing holds the writer to push into; the guard exists for the
    /// by-reference variant and for a caller that keeps the writer alive.
    finished: bool,
    /// [`Table::retained_bytes`] summed over every row not yet in a closed row
    /// group -- in `buffer` or in `writer`'s open row group. Zeroed whenever
    /// that row group closes, never by a batch flush alone, and consulted only
    /// for a table with a byte budget: see [`Table::MAX_BUFFERED_BYTES`].
    pending_bytes: usize,
    _table: PhantomData<fn() -> T>,
}

impl<T: Table, W: Write + Send> TableWriter<T, W> {
    /// Create a writer with the table's default settings (ZSTD compression,
    /// dictionary encoding for the table's string columns, page statistics).
    pub fn new(sink: W) -> Result<Self, ExportError> {
        Self::with_row_group_size(sink, T::DEFAULT_ROW_GROUP_SIZE)
    }

    /// Create a writer with a custom row-group size.
    ///
    /// Smaller values put more, smaller row groups in the file, which costs
    /// compression ratio. It does not change how many rows are held in memory
    /// at once unless it is below [`MAX_BUFFERED_ROWS`].
    ///
    /// A `row_group_size` below that ceiling is *not* subject to the
    /// mini-batch-alignment rule documented on [`MAX_BUFFERED_ROWS`], despite
    /// what the assertion there might suggest. Below the ceiling the clamp is a
    /// no-op, so one batch is one row group exactly as it was before batching
    /// and row groups were separated -- there is no partial batch inside a row
    /// group for a page boundary to land differently in.
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
            finished: false,
            pending_bytes: 0,
            _table: PhantomData,
        })
    }

    /// Push a single record. Converts and hands off a batch when the buffer is
    /// full; the row group is closed by `ArrowWriter` at its row limit, not
    /// here -- unless the table's byte budget is crossed (see
    /// [`Table::MAX_BUFFERED_BYTES`]).
    pub fn push(&mut self, record: T::Row) -> Result<(), ExportError> {
        self.guard_open()?;
        self.flush_for_byte_budget(&record)?;
        self.pending_bytes = self
            .pending_bytes
            .saturating_add(T::retained_bytes(&record));
        self.buffer.push(record);
        self.flush_if_full_or_oversized()
    }

    /// Push a batch of records. Cheaper than repeated single pushes because the
    /// finished-writer check happens once for the whole batch.
    pub fn push_batch(
        &mut self,
        records: impl IntoIterator<Item = T::Row>,
    ) -> Result<(), ExportError> {
        self.guard_open()?;
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

    /// Flush any remaining buffered rows and finalise the Parquet file.
    ///
    /// This **must** be called to produce a valid file. Dropping the writer
    /// without calling `finish` will leave a truncated (unreadable) file.
    pub fn finish(mut self) -> Result<(), ExportError> {
        if !self.buffer.is_empty() {
            self.flush_buffer()?;
        }
        self.writer.close()?;
        self.finished = true;
        Ok(())
    }

    /// Number of rows currently buffered (not yet flushed).
    pub fn buffered_rows(&self) -> usize {
        self.buffer.len()
    }

    // -- internal ----------------------------------------------------------

    fn guard_open(&self) -> Result<(), ExportError> {
        if self.finished {
            return Err(ExportError::Usage(
                "cannot push to a finished writer".into(),
            ));
        }
        Ok(())
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
            // The budget, not the row count: the ArrowWriter must let go of
            // these bytes too, so the row group closes with the buffer.
            self.close_row_group()
        } else if self.buffer.len() >= self.batch_rows {
            // The row count alone: hand the batch over and leave the row group
            // open for the ArrowWriter to close at its row limit.
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
            // Statistics for all columns so that readers can skip row groups
            // via predicate pushdown (e.g. "actor_net_guid = X").
            .set_statistics_enabled(EnabledStatistics::Page)
            // Same as the library default, set explicitly because
            // MAX_BUFFERED_ROWS has to stay a multiple of it. A parquet release
            // that changed the default would otherwise move this crate's output
            // bytes with nothing in this repository having changed.
            .set_write_batch_size(PARQUET_WRITE_BATCH_SIZE);
        for column in T::DICTIONARY_COLUMNS {
            builder = builder
                .set_column_dictionary_enabled(ColumnPath::new(vec![(*column).to_owned()]), true);
        }
        builder.build()
    }

    fn flush_buffer(&mut self) -> Result<(), ExportError> {
        let batch = T::build_batch(&self.buffer)?;
        // Cleared, not taken: the capacity is bounded by `batch_rows` now, so
        // keeping it across flushes costs one allocation for the whole run.
        // Dropping the rows before handing the batch to the encoder also keeps
        // the records and the arrays from being live at the same time.
        self.buffer.clear();
        self.writer.write(&batch)?;
        // The ArrowWriter closes a row group on its own at the row limit, and
        // what it held is then on the sink, not in memory. The budgeted
        // tables' 131,072 is 16 x MAX_BUFFERED_ROWS and a budget cut empties
        // both sides, so that limit falls exactly between two batches and
        // this sees every such close. A custom size that splits a batch
        // leaves the tail open here and keeps counting the flushed head: an
        // over-count, which can only close a later row group early.
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

    /// A budgeted table whose retained bytes are exactly its payload length,
    /// so every test can compute the row on which the budget is crossed
    /// instead of guessing it. Same budget and default row-group size as the
    /// production budgeted tables (partials and the three checkpoint
    /// declaration tables).
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

    /// Push `rows` through a `Budgeted` writer and return every row group's
    /// row count, in file order -- the exact vector, because a bare "one row
    /// group" or ">= 3" cannot tell the candidate flush policies apart.
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
        // Four full batches and a remainder, 16 bytes each: ~0.5 MB against an
        // 8 MiB budget. Nothing here is a reason to close a row group before
        // the 131,072-row limit, so the whole table is one row group. Closing
        // one on every 8,192-row batch is what fragmented the four budgeted
        // tables; `Table::MAX_BUFFERED_BYTES` records what it cost.
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
        // No single batch reaches the budget, but the open row group does. A
        // batch handed to the ArrowWriter is still held in memory, encoded,
        // until its row group closes -- so its bytes must still count. Reset
        // the count on every batch instead and the open row group can hold up
        // to 131,072 / 8,192 = 16 batches of just under 8 MiB each.
        const ROW: usize = 700;
        const _: () = assert!(
            MAX_BUFFERED_ROWS * ROW < BUDGET,
            "no single batch may do it"
        );
        // `flush_for_byte_budget` closes the group before the row whose bytes
        // would take the total strictly past the budget, so the first group
        // holds the largest row count whose total still fits.
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
        // Two full 16,384-row groups of 300-byte rows: each holds ~4.9 MB, the
        // pair ~9.8 MB. The ArrowWriter closes the first at its row limit and
        // those bytes leave memory with it, so they must stop counting.
        // Carried over, they would force a spurious third group.
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
        // One full batch (8,192,000 bytes, under the 8,388,608 budget) goes to
        // the ArrowWriter by row count and leaves the application buffer
        // empty. The next row would take the open row group past the budget,
        // so the group closes first -- even though the application buffer has
        // nothing in it. Only the buffer consulted, that row would join the
        // group and close it one row over budget: [8193].
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
