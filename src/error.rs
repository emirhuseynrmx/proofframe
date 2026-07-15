//! Typed error surface for ProofFrame operations.

/// Errors returned by ProofFrame profiling, validation, diffing, and receipts.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ProofFrameError {
    /// An error surfaced by the underlying Arrow library.
    #[error("Arrow error: {0}")]
    Arrow(#[from] arrow::error::ArrowError),

    /// A filesystem error from the disk-backed diff.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// A contract regular expression failed to compile.
    #[error("invalid regular expression: {0}")]
    Regex(#[from] regex::Error),

    /// A JSON document could not be produced or consumed.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// Persisted diff bytes were not valid UTF-8.
    #[error("invalid UTF-8 in diff data: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),

    /// A validation contract could not be parsed.
    #[error("invalid contract: {0}")]
    InvalidContract(String),

    /// A required key or contract column is absent from the schema.
    #[error("column `{0}` is missing")]
    MissingColumn(String),

    /// An Arrow type has no canonical fingerprint encoding.
    #[error("unsupported Arrow type `{0}` for canonical fingerprinting")]
    UnsupportedType(String),

    /// Two datasets have incompatible schemas for the requested operation.
    #[error("schemas differ: {0}")]
    SchemaMismatch(String),

    /// A diff key column contained duplicate values.
    #[error("duplicate key `{0}`; diff keys must be unique")]
    DuplicateKey(String),

    /// A diff was requested without any key columns.
    #[error("at least one key column is required")]
    NoKeyColumns,

    /// Persisted diff partition data was truncated or corrupt.
    #[error("corrupt diff partition data: {0}")]
    CorruptData(String),

    /// A proof receipt was malformed or failed a structural check.
    #[error("invalid proof receipt: {0}")]
    InvalidReceipt(String),
}
