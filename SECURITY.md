# Security policy

Please report suspected vulnerabilities privately to the maintainer before public disclosure.

ProofFrame processes untrusted tabular values without executing them. Regex patterns and contracts
are trusted configuration in v0.1. Set `max_findings` to bound output size, and apply normal input
size controls when exposing ProofFrame as a network service.

