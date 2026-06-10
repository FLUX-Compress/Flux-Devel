# Contributing to FLUX

Thank you for your interest in contributing to FLUX! We welcome issues, suggestions, and pull requests to help improve the project.

---

## 1. Setting Up Your Development Environment

FLUX is written in standard Rust. You will need:
*   Rust toolchain (v1.75+ recommended, stable channel). Install via [rustup](https://rustup.rs/).
*   Cargo package manager (included with Rust).
*   Python 3 (for running benchmarks).

---

## 2. Codebase Architecture Overview

FLUX is modularized to ensure separation of concerns. Familiarize yourself with the workspace layout before making changes:

*   **[`crates/flux-core`](file:///d:/Chat%20Server/FLUX/crates/flux-core)**: The engine core.
    *   **[`buffer`](file:///d:/Chat%20Server/FLUX/crates/flux-core/src/buffer)**: Sliding windows and high-performance circular buffers.
    *   **[`analysis`](file:///d:/Chat%20Server/FLUX/crates/flux-core/src/analysis)**: Content classification, stride auto-correlation detection, and Shannon entropy estimation.
    *   **[`transform`](file:///d:/Chat%20Server/FLUX/crates/flux-core/src/transform)**: Reversible transpositions, delta filters, BWT, and specialized float-channel split filters.
    *   **[`compress`](file:///d:/Chat%20Server/FLUX/crates/flux-core/src/compress)**: Tokenized LZ77 encoders with repcode caches and rANS entropy coder.
    *   **[`crypto`](file:///d:/Chat%20Server/FLUX/crates/flux-core/src/crypto)**: Argon2id key derivation and AES-256-GCM authenticated encryption.
    *   **[`integrity`](file:///d:/Chat%20Server/FLUX/crates/flux-core/src/integrity)**: Multi-level validation, including parallelized CRC32 and SHA-256 workers.
    *   **[`archive`](file:///d:/Chat%20Server/FLUX/crates/flux-core/src/archive)**: Formats, directory trees, file indexes, and solid block builders.
    *   **[`threads`](file:///d:/Chat%20Server/FLUX/crates/flux-core/src/threads)**: Multi-threaded pipeline signals and background execution workers.
*   **[`crates/flux-cli`](file:///d:/Chat%20Server/FLUX/crates/flux-cli)**: Command-line interface.
*   **[`crates/flux-gui`](file:///d:/Chat%20Server/FLUX/crates/flux-gui)**: Native desktop application using `egui` and `eframe`.

---

## 3. Coding Style and Expectations

To maintain code quality and prevent technical debt, we enforce the following rules:

### A. Strict Clippy Checks
All target files must compile cleanly with Clippy. There must be zero warnings or lints.
Run clippy with the following command:
```bash
cargo clippy --all-targets -- -D warnings
```

### B. Consistent Code Formatting
Use standard `rustfmt` to format code before committing:
```bash
cargo fmt --all -- --check
```

### C. Documentation
*   Every public module and public API must be documented with clean docstrings (`///`).
*   Include usage examples where helpful.
*   Maintain block-diagram and ASCII architectures in module headers (`mod.rs`).

### D. Perfect Reversibility
**All compression transformations must be 100% byte-perfect reversible.** There can be no rounding errors or lost metadata. Add roundtrip tests for any new transformation filter or compression codec.

---

## 4. Building and Testing

Ensure your changes compile and pass the entire test suite:

```bash
# Build the project in debug mode
cargo build

# Run unit and integration tests (Balanced Mode)
cargo test --release

# Run Clippy checks
cargo clippy --all-targets -- -D warnings
```

---

## 5. Benchmarking

We use Python scripts to evaluate compression ratios and verify there are no regressions. Before submitting a pull request, run the benchmark script to confirm performance:

```bash
python scratch/benchmark.py
```

Ensure your changes do not regress the compression ratios or speeds of existing winning cases (e.g., coordinates, PCM audio, or timeseries float data).
