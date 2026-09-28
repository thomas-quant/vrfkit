//! Schema errors: explicit variants, never panics, so a corrupt replay can be
//! told apart from a parser bug.

use vrf_bitio::BitError;

/// Errors that can occur while reading or maintaining the dynamic schema.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SchemaError {
    /// The underlying bit stream was truncated or malformed.
    #[error("bit-level read failed: {0}")]
    Bitio(#[from] BitError),

    /// A net-field export references a `path_name_index` never registered: the
    /// stream is corrupt or out of order.
    #[error("net-field export references unknown path name index {index}")]
    UnknownPathIndex {
        /// The unregistered index.
        index: u32,
    },

    /// A live export group declared more field slots than the checkpoint form
    /// permits.
    #[error("net-field export declares {count} field slots, maximum is {max}")]
    FieldCountOverflow {
        /// The declared slot count.
        count: u32,
        max: u32,
    },

    /// Reserving storage for a bounded live export group failed.
    #[error("could not reserve {count} net-field export slots")]
    FieldAllocationFailed {
        /// The requested, already-bounded slot count.
        count: u32,
    },

    /// An incoming export group's path and index identify two different
    /// canonical groups.
    #[error(
        "net-field export path '{path}' identifies '{path_group}', but index {path_name_index} identifies '{index_group}'"
    )]
    CrossedExportGroupIdentity {
        path: String,
        path_name_index: u32,
        /// Canonical path selected by the incoming path.
        path_group: String,
        /// Canonical path selected by the incoming index.
        index_group: String,
    },

    /// An export GUID payload declared a negative size.
    #[error("export GUID payload size is negative: {size}")]
    NegativePayloadSize {
        /// The declared size.
        size: i32,
    },

    /// The export GUID payload was not fully consumed after reading.
    #[error("export GUID payload has {remaining} trailing byte(s)")]
    TrailingPayloadData {
        /// Whole bytes left over.
        remaining: usize,
    },

    /// NetGUID object recursion exceeded the safety limit.
    #[error("net GUID object recursion depth exceeded {limit}")]
    RecursionLimitExceeded {
        /// The maximum depth.
        limit: u32,
    },

    /// A checkpoint guid-cache entry's path discriminator was neither 0 nor 1.
    /// Only those occur across 17,186,645 corpus entries, so a third value
    /// means the cursor is misaligned.
    #[error("checkpoint guid entry {entry}: path discriminator is {byte}, expected 0 or 1")]
    CheckpointBadPathKind {
        /// Index of the entry being read.
        entry: u32,
        byte: u8,
    },

    /// A checkpoint GUID entry's path index points past the literal paths that
    /// precede it in that checkpoint.
    #[error(
        "checkpoint guid entry {entry}: path index {index} exceeds {literals} preceding literals"
    )]
    CheckpointPathIndexOutOfBounds {
        entry: u32,
        index: u32,
        literals: u32,
    },

    /// A checkpoint export-group slot declared a handle other than its own
    /// index. `handle == slot` holds for all 11,529,869 exported corpus slots;
    /// a mismatch means the stream desynchronised and would attach real names
    /// to the wrong handles.
    #[error("checkpoint group '{group}' slot {slot}: declared handle {handle}")]
    CheckpointHandleNotSlot {
        /// Path of the group being read.
        group: String,
        slot: u32,
        handle: u32,
    },

    /// The export-group map did not end where the prologue says the DemoFrame
    /// begins. `map_end == prologue_offset + 8` holds for all 4,024 corpus
    /// checkpoints; this is the only end-to-end check on both table parses.
    #[error("checkpoint tables ended at {map_end}, prologue implies {expected}")]
    CheckpointFrameOffsetMismatch {
        /// Where parsing finished; `expected` is where the prologue says.
        map_end: usize,
        expected: usize,
    },

    /// A reserved prologue word (bytes 4, 8, 12; zero in all 4,024 corpus
    /// checkpoints) was non-zero: an unknown field, and every later offset is
    /// suspect.
    #[error("checkpoint prologue word at byte {offset} is {value}, expected 0")]
    CheckpointReservedWordSet {
        /// Byte offset of the word.
        offset: usize,
        value: u32,
    },

    /// A checkpoint count field exceeded its sanity bound.
    #[error("checkpoint {field}: count {count} exceeds maximum {max}")]
    CheckpointCountOverflow {
        /// Which count overflowed.
        field: &'static str,
        count: u32,
        max: u32,
    },

    /// A checkpoint declared an export-group path or index more than once.
    #[error("checkpoint export group '{path}' collides at path-name index {path_name_index}")]
    CheckpointGroupCollision {
        /// The later declaration's path.
        path: String,
        path_name_index: u32,
    },
}

/// Result alias for schema operations.
pub type Result<T> = core::result::Result<T, SchemaError>;
