//! Typed error surface for ProofFrame operations.

use serde::Serialize;

/// Stable machine-readable category for every ProofFrame failure.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub enum ErrorCode {
    Arrow,
    Io,
    ContractInvalidJson,
    ContractUnknownField,
    ContractInvalidBound,
    ContractInvalidRatio,
    ContractInvalidType,
    ContractDraft,
    ContractDuplicateRule,
    ContractTypeMismatch,
    ReferenceUnbound,
    SchemaMismatch,
    MissingColumn,
    UnsupportedType,
    DuplicateKey,
    NoKeyColumns,
    ResourceLimit,
    CorruptPartition,
    FingerprintVersion,
    ReceiptInvalid,
    ReceiptInvalidSignature,
    ReceiptUntrustedSigner,
    Cancelled,
}

impl ErrorCode {
    /// Stable external spelling used by Python and serialized diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Arrow => "PF_ARROW",
            Self::Io => "PF_IO",
            Self::ContractInvalidJson => "PF_CONTRACT_INVALID_JSON",
            Self::ContractUnknownField => "PF_CONTRACT_UNKNOWN_FIELD",
            Self::ContractInvalidBound => "PF_CONTRACT_INVALID_BOUND",
            Self::ContractInvalidRatio => "PF_CONTRACT_INVALID_RATIO",
            Self::ContractInvalidType => "PF_CONTRACT_INVALID_TYPE",
            Self::ContractDraft => "PF_DRAFT_CONTRACT",
            Self::ContractDuplicateRule => "PF_CONTRACT_DUPLICATE_RULE",
            Self::ContractTypeMismatch => "PF_CONTRACT_TYPE_MISMATCH",
            Self::ReferenceUnbound => "PF_REFERENCE_UNBOUND",
            Self::SchemaMismatch => "PF_SCHEMA_MISMATCH",
            Self::MissingColumn => "PF_MISSING_COLUMN",
            Self::UnsupportedType => "PF_UNSUPPORTED_TYPE",
            Self::DuplicateKey => "PF_DUPLICATE_KEY",
            Self::NoKeyColumns => "PF_NO_KEY_COLUMNS",
            Self::ResourceLimit => "PF_RESOURCE_LIMIT",
            Self::CorruptPartition => "PF_CORRUPT_PARTITION",
            Self::FingerprintVersion => "PF_FINGERPRINT_VERSION",
            Self::ReceiptInvalid => "PF_RECEIPT_INVALID",
            Self::ReceiptInvalidSignature => "PF_RECEIPT_INVALID_SIGNATURE",
            Self::ReceiptUntrustedSigner => "PF_RECEIPT_UNTRUSTED_SIGNER",
            Self::Cancelled => "PF_CANCELLED",
        }
    }
}

/// Errors returned by ProofFrame profiling, validation, diffing, and receipts.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ProofFrameError {
    /// A versioned contract failed syntax or semantic validation.
    #[error("{message}")]
    Contract {
        code: ErrorCode,
        message: String,
        path: Option<String>,
    },

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

    /// A report or evidence document handed to the review renderer was not usable.
    #[error("{0}")]
    Review(String),

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

    /// An operation requested more memory, temporary storage, or output than allowed.
    #[error(
        "Resource limit exceeded for {resource}: requested {requested} with {used} used and {limit} allowed"
    )]
    ResourceLimit {
        resource: &'static str,
        requested: u64,
        used: u64,
        limit: u64,
    },

    /// A cooperative cancellation token stopped the operation.
    #[error("Operation cancelled")]
    Cancelled,
}

impl ProofFrameError {
    pub(crate) fn contract(
        code: ErrorCode,
        message: impl Into<String>,
        path: Option<String>,
    ) -> Self {
        Self::Contract {
            code,
            message: message.into(),
            path,
        }
    }

    /// Return the stable machine-readable category for this error.
    #[must_use]
    pub const fn code(&self) -> ErrorCode {
        match self {
            Self::Contract { code, .. } => *code,
            Self::Arrow(_) => ErrorCode::Arrow,
            Self::Io(_) => ErrorCode::Io,
            Self::Regex(_) => ErrorCode::ContractInvalidBound,
            Self::Json(_) | Self::InvalidContract(_) | Self::Review(_) => {
                ErrorCode::ContractInvalidJson
            }
            Self::Utf8(_) | Self::CorruptData(_) => ErrorCode::CorruptPartition,
            Self::MissingColumn(_) => ErrorCode::MissingColumn,
            Self::UnsupportedType(_) => ErrorCode::UnsupportedType,
            Self::SchemaMismatch(_) => ErrorCode::SchemaMismatch,
            Self::DuplicateKey(_) => ErrorCode::DuplicateKey,
            Self::NoKeyColumns => ErrorCode::NoKeyColumns,
            Self::InvalidReceipt(_) => ErrorCode::ReceiptInvalid,
            Self::ResourceLimit { .. } => ErrorCode::ResourceLimit,
            Self::Cancelled => ErrorCode::Cancelled,
        }
    }

    /// JSON-style path associated with a structured input failure.
    #[must_use]
    pub fn path(&self) -> Option<&str> {
        match self {
            Self::Contract { path, .. } => path.as_deref(),
            _ => None,
        }
    }
}
