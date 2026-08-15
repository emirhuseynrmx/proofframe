"""ProofFrame: Rust-native contracts, canonical fingerprints, and proof receipts."""

from ._proofframe import __version__
from .api import (
    check,
    detect_leakage,
    diff,
    fingerprint,
    generate_keypair,
    profile,
    scan_pii,
    sign_receipt,
    validate,
    verify_receipt,
)
from .errors import (
    ContractError,
    CorruptDataError,
    ProofFrameError,
    ReceiptError,
    ResourceLimitError,
    SchemaError,
)

__all__ = [
    "ContractError",
    "CorruptDataError",
    "ProofFrameError",
    "ReceiptError",
    "ResourceLimitError",
    "SchemaError",
    "__version__",
    "check",
    "detect_leakage",
    "diff",
    "fingerprint",
    "generate_keypair",
    "profile",
    "scan_pii",
    "sign_receipt",
    "validate",
    "verify_receipt",
]
