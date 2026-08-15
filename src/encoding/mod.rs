//! Canonical, versioned Arrow encoders.

mod plan;
mod scratch;
mod v1;

pub(crate) use v1::fingerprint_v1;
