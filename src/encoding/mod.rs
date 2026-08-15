//! Canonical, versioned Arrow encoders.

use std::collections::BTreeSet;
use std::fmt::{Display, Formatter};

use arrow::record_batch::RecordBatchReader;
use serde::{Deserialize, Serialize};

mod plan;
mod scratch;
mod v1;
mod v2;

use crate::ProofFrameError;

/// Canonical fingerprint algorithm selected for an operation.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FingerprintVersion {
    V1,
    V2,
}

impl FingerprintVersion {
    const fn prefix(self) -> &'static str {
        match self {
            Self::V1 => "pf-fp-v1:",
            Self::V2 => "pf-fp-v2:",
        }
    }
}

/// Typed canonical digest with its algorithm version and row count.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct Fingerprint {
    version: FingerprintVersion,
    digest: [u8; 32],
    rows: u64,
}

impl Fingerprint {
    pub(crate) const fn new(version: FingerprintVersion, digest: [u8; 32], rows: u64) -> Self {
        Self {
            version,
            digest,
            rows,
        }
    }

    #[must_use]
    pub const fn version(&self) -> FingerprintVersion {
        self.version
    }

    #[must_use]
    pub const fn rows(&self) -> u64 {
        self.rows
    }

    #[must_use]
    pub const fn digest(&self) -> &[u8; 32] {
        &self.digest
    }

    /// Render the externally tagged digest with one exact-capacity allocation.
    #[must_use]
    pub fn to_tagged_string(&self) -> String {
        let hex = blake3::Hash::from_bytes(self.digest).to_hex();
        let mut tagged = String::with_capacity(self.version.prefix().len() + hex.len());
        tagged.push_str(self.version.prefix());
        tagged.push_str(hex.as_str());
        tagged
    }
}

impl Display for Fingerprint {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.version.prefix())?;
        Display::fmt(&blake3::Hash::from_bytes(self.digest).to_hex(), formatter)
    }
}

/// Inputs that affect canonical fingerprint semantics.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct FingerprintOptions {
    version: FingerprintVersion,
    metadata_keys: BTreeSet<String>,
}

impl FingerprintOptions {
    #[must_use]
    pub const fn new(version: FingerprintVersion) -> Self {
        Self {
            version,
            metadata_keys: BTreeSet::new(),
        }
    }

    #[must_use]
    pub fn with_metadata_key(mut self, key: impl Into<String>) -> Self {
        self.metadata_keys.insert(key.into());
        self
    }

    #[must_use]
    pub const fn version(&self) -> FingerprintVersion {
        self.version
    }

    #[must_use]
    pub fn metadata_keys(&self) -> &BTreeSet<String> {
        &self.metadata_keys
    }
}

impl Default for FingerprintOptions {
    fn default() -> Self {
        Self::new(FingerprintVersion::V2)
    }
}

pub(crate) fn fingerprint<R>(
    reader: R,
    options: &FingerprintOptions,
) -> Result<Fingerprint, ProofFrameError>
where
    R: RecordBatchReader,
{
    match options.version {
        FingerprintVersion::V1 => v1::fingerprint_v1(reader),
        FingerprintVersion::V2 => v2::fingerprint_v2(reader, options),
    }
}

pub(crate) use v1::fingerprint_v1;
