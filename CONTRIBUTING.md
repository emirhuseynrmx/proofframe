# Contributing to ProofFrame

Thanks for contributing. Please keep changes focused, add regression coverage for behavior changes, and avoid combining unrelated formatting with functional edits.

## Supported development environment

CI exercises Rust 1.85 and Python 3.10 through 3.13 on Ubuntu, macOS, and Windows. Install a supported Rust toolchain, Python, and the project development dependencies:

```bash
python -m pip install -e ".[dev]"
```

Build the native extension before running Python tests when it has changed:

```bash
maturin develop --release --locked
```

## Local checks

Run the checks relevant to your change before opening a pull request:

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets --all-features
ruff check python tests benchmarks scripts
python -m pytest -q
```

For release-candidate packaging, run:

```bash
maturin build --release --locked --out target/contrib-wheel --compatibility pypi
maturin sdist --out target/contrib-sdist
cargo package --locked
```

Do not publish packages or create release tags from a contribution branch.

## Benchmarks

Do not describe a local benchmark as a general performance result. Use the documented release-gate runner and preserve its raw samples, dataset identity, hardware, compiler, and resource-limit metadata. See [`docs/testing.md`](docs/testing.md) for the benchmark contract and smoke-run command.

## Security and data handling

Do not include credentials, access tokens, private keys, production receipts, or sensitive row values in commits, issues, pull requests, logs, or benchmark artifacts. Report suspected vulnerabilities privately through the [security policy](SECURITY.md), not in a public issue.

## Pull requests

Explain the problem, the observable behavior change, and how you tested it. Update user-facing documentation when public behavior changes. Keep compatibility implications explicit, especially for contract and evidence formats.
