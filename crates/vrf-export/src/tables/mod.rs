//! One module per export table.
//!
//! Each module holds that table's row-group size, its dictionary columns, and
//! the row-slice-to-`RecordBatch` conversion -- the three things
//! [`crate::writer::Table`] abstracts over. The buffer-and-flush machinery
//! itself lives once in [`crate::writer`].
//!
//! Every table is behind its own feature so a consumer can take, say, `fields`
//! without linking the movement writer. The record structs are not gated: they
//! carry no Arrow types and callers producing records for a table they do not
//! write (the validation oracle does exactly that) must still be able to name
//! them.

#[cfg(feature = "actors")]
pub mod actors;
#[cfg(feature = "checkpoint-context")]
pub mod checkpoints;
#[cfg(feature = "events")]
pub mod events;
#[cfg(feature = "fields")]
pub mod fields;
#[cfg(feature = "movement")]
pub mod movement;
#[cfg(feature = "net-guids")]
pub mod net_guids;
#[cfg(feature = "partials")]
pub mod partials;

/// Column builders the table modules share: the `RecordBatch` every
/// `build_batch` returns, the dictionary string column, and the column lists
/// of `fields`, `actors` and `net_guids`, which their checkpoint tables write
/// again after two identity columns. They live here, not in those table
/// modules, because `checkpoint-context` builds without the three features.
/// Each item is compiled only with a feature that uses it.
#[cfg(any(
    feature = "actors",
    feature = "checkpoint-context",
    feature = "events",
    feature = "fields",
    feature = "movement",
    feature = "net-guids",
    feature = "partials"
))]
mod columns {
    use std::sync::Arc;

    use arrow_array::{ArrayRef, RecordBatch};
    use arrow_schema::Schema;

    use crate::error::ExportError;

    /// `RecordBatch::try_new`, with its error as this crate's.
    pub(super) fn batch(
        schema: Arc<Schema>,
        columns: Vec<ArrayRef>,
    ) -> Result<RecordBatch, ExportError> {
        RecordBatch::try_new(schema, columns).map_err(|e| ExportError::Parquet(e.into()))
    }

    /// A `Dictionary<Int32, Utf8>` column of `len` rows. `distinct` and
    /// `bytes` only size the builder.
    #[cfg(any(
        feature = "actors",
        feature = "checkpoint-context",
        feature = "events",
        feature = "fields",
        feature = "net-guids"
    ))]
    pub(super) fn dict<'a>(
        len: usize,
        distinct: usize,
        bytes: usize,
        values: impl Iterator<Item = Option<&'a str>>,
    ) -> ArrayRef {
        use arrow_array::builder::StringDictionaryBuilder;
        use arrow_array::types::Int32Type;

        let mut builder = StringDictionaryBuilder::<Int32Type>::with_capacity(len, distinct, bytes);
        // `append_option`, one value at a time: `append_null` for `None`,
        // `append_value` for `Some`.
        builder.extend(values);
        Arc::new(builder.finish())
    }

    /// The `fields` columns in `fields_schema` order.
    #[cfg(any(feature = "fields", feature = "checkpoint-context"))]
    pub(super) fn field_columns<'a>(
        rows: impl Iterator<Item = &'a crate::record::FieldRecord> + Clone,
        len: usize,
    ) -> Vec<ArrayRef> {
        use arrow_array::{BinaryArray, BooleanArray, Float64Array, Int64Array, UInt32Array};

        vec![
            Arc::new(UInt32Array::from_iter_values(
                rows.clone().map(|r| r.time_ms),
            )) as ArrayRef,
            Arc::new(UInt32Array::from_iter_values(
                rows.clone().map(|r| r.packet_id),
            )),
            Arc::new(UInt32Array::from_iter_values(
                rows.clone().map(|r| r.channel_index),
            )),
            Arc::new(UInt32Array::from_iter_values(
                rows.clone().map(|r| r.actor_net_guid),
            )),
            Arc::new(UInt32Array::from_iter(
                rows.clone().map(|r| r.object_net_guid),
            )),
            // `&*r.group_path` derefs the interned `Arc<str>` to the same
            // `&str` a `String` would have produced, so the builder sees an
            // identical value sequence and the encoded dictionary is
            // unchanged. Interning must not be exploited here -- e.g. by
            // keying on `Arc::as_ptr` to skip an append -- or the
            // row-to-value mapping stops being one-to-one.
            dict(
                len,
                256,
                len * 20,
                rows.clone().map(|r| Some(&*r.group_path)),
            ),
            Arc::new(UInt32Array::from_iter_values(
                rows.clone().map(|r| r.handle),
            )),
            dict(
                len,
                256,
                len * 16,
                rows.clone().map(|r| r.field_name.as_deref()),
            ),
            Arc::new(UInt32Array::from_iter(
                rows.clone().map(|r| r.compatible_checksum),
            )),
            Arc::new(UInt32Array::from_iter_values(
                rows.clone().map(|r| r.bit_count),
            )),
            Arc::new(BinaryArray::from_iter(
                rows.clone().map(|r| r.raw_bits.as_deref()),
            )),
            Arc::new(Int64Array::from_iter(rows.clone().map(|r| r.value_i64))),
            Arc::new(Float64Array::from_iter(rows.clone().map(|r| r.value_f64))),
            Arc::new(BooleanArray::from_iter(rows.clone().map(|r| r.value_bool))),
            // The decoded typed values are highly repetitive -- enum strings,
            // JSON blobs, repeated struct JSON -- so a dictionary shrinks the
            // column and speeds up downstream readers.
            dict(len, 2048, len * 32, rows.map(|r| r.value_str.as_deref())),
        ]
    }

    /// The `actors` columns in `actors_schema` order.
    #[cfg(any(feature = "actors", feature = "checkpoint-context"))]
    pub(super) fn actor_columns<'a>(
        rows: impl Iterator<Item = &'a crate::record::ActorRecord> + Clone,
        len: usize,
    ) -> Vec<ArrayRef> {
        use arrow_array::{Float32Array, StringArray, UInt32Array};

        vec![
            Arc::new(UInt32Array::from_iter_values(
                rows.clone().map(|r| r.time_ms),
            )) as ArrayRef,
            Arc::new(UInt32Array::from_iter_values(
                rows.clone().map(|r| r.packet_id),
            )),
            Arc::new(UInt32Array::from_iter_values(
                rows.clone().map(|r| r.channel_index),
            )),
            Arc::new(UInt32Array::from_iter_values(
                rows.clone().map(|r| r.actor_net_guid),
            )),
            Arc::new(StringArray::from_iter_values(rows.clone().map(|r| r.event))),
            dict(
                len,
                128,
                len * 30,
                rows.clone().map(|r| r.class_path.as_deref()),
            ),
            dict(
                len,
                128,
                len * 30,
                rows.clone().map(|r| r.archetype_path.as_deref()),
            ),
            Arc::new(Float32Array::from_iter(rows.clone().map(|r| r.spawn_x))),
            Arc::new(Float32Array::from_iter(rows.clone().map(|r| r.spawn_y))),
            Arc::new(Float32Array::from_iter(rows.clone().map(|r| r.spawn_z))),
            Arc::new(Float32Array::from_iter(rows.clone().map(|r| r.spawn_pitch))),
            Arc::new(Float32Array::from_iter(rows.clone().map(|r| r.spawn_yaw))),
            Arc::new(Float32Array::from_iter(rows.map(|r| r.spawn_roll))),
        ]
    }

    /// The `net_guids` columns in `net_guids_schema` order.
    #[cfg(any(feature = "net-guids", feature = "checkpoint-context"))]
    pub(super) fn net_guid_columns<'a>(
        rows: impl Iterator<Item = &'a crate::record::NetGuidRecord> + Clone,
        len: usize,
    ) -> Vec<ArrayRef> {
        use arrow_array::UInt32Array;

        vec![
            Arc::new(UInt32Array::from_iter_values(
                rows.clone().map(|r| r.net_guid),
            )) as ArrayRef,
            dict(
                len,
                1024,
                len * 40,
                rows.clone().map(|r| Some(r.path.as_str())),
            ),
            Arc::new(UInt32Array::from_iter(rows.map(|r| r.outer_net_guid))),
        ]
    }
}
