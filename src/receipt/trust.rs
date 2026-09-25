use std::collections::BTreeSet;

use ed25519_dalek::VerifyingKey;

#[derive(Debug, Clone, Default)]
pub struct TrustStore {
    keys: BTreeSet<[u8; 32]>,
}

impl TrustStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, key: VerifyingKey) -> bool {
        self.keys.insert(key.to_bytes())
    }

    #[must_use]
    pub fn contains(&self, key: &VerifyingKey) -> bool {
        self.keys.contains(&key.to_bytes())
    }
}

/// Who is allowed to have signed a receipt.
///
/// A signature proves only that the holder of *some* key signed the bytes. Anyone
/// can generate a key, so a receipt is authentic only when its key is one the
/// verifier already trusts.
#[derive(Debug, Clone)]
pub enum TrustPolicy {
    /// Check integrity and the signature, but trust no signer.
    ///
    /// A receipt verified under this policy is never `valid`: `signer_trusted` is
    /// always false. Use [`ReceiptVerification::intact`](super::ReceiptVerification::intact)
    /// when only integrity is wanted. Before 0.7.2 this policy trusted every
    /// signer, which let a receipt signed with a freshly generated key pass as valid.
    SignatureOnly,
    /// Trust exactly this key.
    ExpectedKey(VerifyingKey),
    /// Trust any key in the store.
    TrustStore(TrustStore),
}

impl TrustPolicy {
    pub(super) fn accepts(&self, key: &VerifyingKey) -> bool {
        match self {
            Self::SignatureOnly => false,
            Self::ExpectedKey(expected) => expected == key,
            Self::TrustStore(store) => store.contains(key),
        }
    }
}
