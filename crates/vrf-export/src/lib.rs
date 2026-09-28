//! Columnar (Parquet) output for decoded replay records.
//!
//! Parquet because a replay yields ~1.25 M field rows and ~1.8 M movement
//! samples, and readers (pandas, DuckDB, Spark) decode only the columns they
//! need; the NDJSON pipeline before it spent 84% of its time parsing JSON.
//!
//! # Layout
//!
//! - [`record`] -- the input structs. No Arrow types, never feature-gated.
//! - `schema` -- the Arrow schema for each table, defined once so writer and
//!   reader agree.
//! - `writer` -- the buffer-and-flush row-group machinery, written once.
//! - `tables` -- one module per table, supplying the three things that
//!   actually differ between them.
//!
//! A `fields` row carries at most one typed value, in four sparse nullable
//! columns rather than an Arrow Union: nulls compress to almost nothing and
//! every reader handles them. Why, and which columns get a Parquet dictionary:
//! docs/PERFORMANCE_NOTES.md#sparse-nullable-columns-vs-arrow-union and
//! docs/PERFORMANCE_NOTES.md#dictionary-encoding-is-chosen-per-column.
//!
//! Row groups stay large (128 Ki rows for `fields`, 256 Ki for `movement`) for
//! compression and predicate pushdown, while memory is bounded by converting
//! much smaller batches to Arrow; `writer::MAX_BUFFERED_ROWS` has the one
//! constraint that ties the two together.
//!
//! # Feature flags
//!
//! | feature | default | effect |
//! |---|---|---|
//! | `parquet` | yes | arrow + parquet + the writer machinery |
//! | `fields`, `movement`, `actors`, `net-guids`, `events`, `partials` | yes | one writer each; each implies `parquet` |
//! | `checkpoint-context` | yes | the seven checkpoint tables' writers; implies `parquet` |
//! | `snappy` | no | adds Snappy to the Parquet codec set |
//!
//! With `--no-default-features` the crate is the record structs and
//! [`ExportError`] alone, without arrow, parquet or zstd: what `vrfkit
//! validate` uses, since it drives the whole decode and writes no file. Why
//! ZSTD is always on and Snappy opt-in: the `[features]` comment in Cargo.toml.

#![forbid(unsafe_code)]

mod error;
pub mod record;
#[cfg(feature = "parquet")]
pub mod schema;
#[cfg(feature = "parquet")]
pub mod tables;
#[cfg(feature = "parquet")]
pub mod writer;

pub use error::ExportError;
pub use record::{
    ActorRecord, CheckpointActorRecord, CheckpointBlockRecord, CheckpointExportFieldRecord,
    CheckpointExportGroupRecord, CheckpointFieldRecord, CheckpointGuidEntryRecord,
    CheckpointIdentity, CheckpointNetGuidRecord, EventRecord, FieldRecord, MovementRecord,
    NetGuidRecord, PartialRecord, UNRESOLVED_CLASS_NET_CACHE_PAYLOAD_FIELD_NAME,
};
#[cfg(feature = "checkpoint-context")]
pub use tables::checkpoints::{
    CheckpointActorWriter, CheckpointActorsTable, CheckpointBlockWriter, CheckpointBlocksTable,
    CheckpointExportFieldWriter, CheckpointExportFieldsTable, CheckpointExportGroupWriter,
    CheckpointExportGroupsTable, CheckpointFieldWriter, CheckpointFieldsTable,
    CheckpointGuidEntriesTable, CheckpointGuidEntryWriter, CheckpointNetGuidWriter,
    CheckpointNetGuidsTable,
};

#[cfg(feature = "actors")]
pub use tables::actors::{ActorWriter, ActorsTable, DEFAULT_ACTOR_ROW_GROUP_SIZE};
#[cfg(feature = "events")]
pub use tables::events::{DEFAULT_EVENT_ROW_GROUP_SIZE, EventWriter, EventsTable};
#[cfg(feature = "fields")]
pub use tables::fields::{DEFAULT_ROW_GROUP_SIZE, FieldWriter, FieldsTable};
#[cfg(feature = "movement")]
pub use tables::movement::{DEFAULT_MOVEMENT_ROW_GROUP_SIZE, MovementTable, MovementWriter};
#[cfg(feature = "net-guids")]
pub use tables::net_guids::{DEFAULT_NET_GUID_ROW_GROUP_SIZE, NetGuidWriter, NetGuidsTable};
#[cfg(feature = "partials")]
pub use tables::partials::{PartialWriter, PartialsTable};
#[cfg(feature = "parquet")]
pub use writer::{Table, TableWriter};
