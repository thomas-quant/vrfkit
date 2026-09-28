//! Crate-wide error type.

use thiserror::Error;

/// Errors that can occur while writing export files.
///
/// `#[non_exhaustive]` because the `Parquet` variant exists only with the
/// `parquet` feature: a caller's `match` has to compile either way, so it needs
/// a wildcard arm.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ExportError {
    /// The underlying Parquet writer encountered a codec, schema, or IO error.
    /// Only with `parquet`: naming the type would drag the dependency back in.
    #[cfg(feature = "parquet")]
    #[error("parquet write failed: {0}")]
    Parquet(#[from] parquet::errors::ParquetError),

    /// A standard IO error (file creation, flush, etc.).
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// Caller misuse. The writers return it for a row-group size of zero.
    #[error("{0}")]
    Usage(String),
}
