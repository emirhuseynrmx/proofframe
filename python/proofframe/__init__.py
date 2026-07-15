"""ProofFrame: Rust-native contracts, canonical fingerprints, and proof receipts."""

from ._proofframe import __version__
from .api import (
    detect_leakage,
    diff,
    generate_keypair,
    profile,
    scan_pii,
    sign_receipt,
    validate,
    verify_receipt,
)

__all__ = [
    "__version__",
    "detect_leakage",
    "diff",
    "generate_keypair",
    "profile",
    "scan_pii",
    "sign_receipt",
    "validate",
    "verify_receipt",
]
