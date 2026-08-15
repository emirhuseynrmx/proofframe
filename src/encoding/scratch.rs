/// Reusable storage for nested V1 values whose encoded length precedes bytes.
#[derive(Default)]
pub(crate) struct Scratch {
    pub(crate) bytes: Vec<u8>,
}
