"""Stable Python exception classes exported by the native engine."""

from ._proofframe import (
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
]
