//! Typed error surface for ProofFrame operations.

/// Errors returned by ProofFrame profiling, validation, diffing, and receipts.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ProofFrameError {
    /// An error surfaced by the underlying Arrow library.
    #[error("{0}")]
    Arrow(#[from] arrow::error::ArrowError),

    /// A filesystem error from the disk-backed diff.
    #[error("{0}")]
    Io(#[from] std::io::Error),

    /// A contract regular expression failed to compile.
    #[error("{0}")]
    Regex(#[from] regex::Error),

    /// A JSON document could not be produced or consumed.
    #[error("{0}")]
    Json(#[from] serde_json::Error),

    /// Persisted diff bytes were not valid UTF-8.
    #[error("{0}")]
    Utf8(#[from] std::string::FromUtf8Error),

    /// A validation contract could not be parsed.
    #[error("{0}")]
    InvalidContract(String),

    /// A required key or contract column is absent from the schema.
    #[error("Key column `{0}` is missing")]
    MissingColumn(String),

    /// An Arrow type has no canonical fingerprint encoding.
    #[error("Unsupported Arrow type `{0}` for canonical fingerprinting")]
    UnsupportedType(String),

    /// Two datasets have incompatible schemas for the requested operation.
    #[error("Schemas differ; {0}")]
    SchemaMismatch(String),

    /// A diff key column contained duplicate values.
    #[error("Duplicate key `{0}`; diff keys must be unique")]
    DuplicateKey(String),

    /// A diff was requested without any key columns.
    #[error("At least one key column is required")]
    NoKeyColumns,

    /// Persisted diff partition data was truncated or corrupt.
    #[error("{0}")]
    CorruptData(String),

    /// A proof receipt was malformed or failed a structural check.
    #[error("{0}")]
    InvalidReceipt(String),
}
