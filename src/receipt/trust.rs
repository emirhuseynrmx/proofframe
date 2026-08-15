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

#[derive(Debug, Clone)]
pub enum TrustPolicy {
    SignatureOnly,
    ExpectedKey(VerifyingKey),
    TrustStore(TrustStore),
}

impl TrustPolicy {
    pub(super) fn accepts(&self, key: &VerifyingKey) -> bool {
        match self {
            Self::SignatureOnly => true,
            Self::ExpectedKey(expected) => expected == key,
            Self::TrustStore(store) => store.contains(key),
        }
    }
}
