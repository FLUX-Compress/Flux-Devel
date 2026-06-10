# FLUX

FLUX is a novel, high-density, content-aware solid compression and archiving library written entirely in Rust. It combines adaptive stride transposition, dictionary matching (LZ77), prediction by partial matching (PPM), secondary symbol estimation (SSE), and a high-performance range Asymmetric Numeral Systems (rANS) entropy coder. 

FLUX is designed to maximize compression ratios across heterogeneous datasets (mixing text, multimedia, structures, and binaries) while guaranteeing integrity and cryptographic security.

---

## Quick Start

The public API is designed with a clean, developer-friendly builder pattern that completely hides low-level unsafe FFI bindings and FFI DLL details.

```rust
use std::path::Path;
use flux::{Archive, Compression};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Compress a directory structure into a secure solid archive
    let stats = Archive::compress("./my_project")
        .output("backup.flx")
        .level(Compression::Maximum)
        .password("my_secret_password")
        .on_progress(|p| {
            println!("{:.1}% — {}", p.percent(), p.current_file());
        })
        .run()?;

    println!("Compressed {} files! Ratio: {:.2}x", 
        stats.files_processed(), 
        stats.compression_ratio()
    );

    // Extract files back securely
    Archive::extract("backup.flx")
        .output("./restored")
        .password("my_secret_password")
        .verify(true)
        .run()?;

    Ok(())
}
```

---

## Features

- **Content-Aware Block Splitting & Classification**: Scans data ahead of time and routes distinct groups (Text, Multimedia, Binary, Raw/Incompressible) to specialized compression pipelines.
- **Adaptive Stride Transposition**: Automatically detects multi-byte strides (1–16 bytes) in real-time, performing plane transpositions and delta coding to exposed structural patterns.
- **Context-Mixed Entropy Coding**: Blends PPM-I order-8 predictive models with Secondary Symbol Estimation (SSE) via dynamic weighting and context mixing before entropy coding.
- **State-of-the-Art Entropy Coder**: High-speed, high-ratio range-Asymmetric Numeral Systems (rANS) implementation.
- **Secure & Hardened Cryptography**: Uses Argon2id (64MB memory, 3 iterations) for key derivation, AES-256-GCM for authenticated stream chunks, and pre-decryption incorrect password detection via sentinel chunks.
- **Multi-Level Integrity Hierarchy**: Complete verification of Archive levels (SHA-256), Block levels (SHA-256), and individual File levels (CRC32 and SHA-256).

---

## Compression Levels

FLUX exposes three balanced compression settings designed for different tradeoffs:

| Level | Tradeoff Description | Algorithm Routing |
| :--- | :--- | :--- |
| **Fast** | Prioritizes throughput speed over density. | Quick LZ77 matches, light transpositions, and raw bypasses for complex blocks. |
| **Balanced** | Default setting. Offers optimal ratio-to-speed. | Stride delta transpositions, standard LZ77 dictionary searches, and order-4 PPM models. |
| **Maximum** | Prioritizes compression density. Maximum ratio. | Full lazy match evaluations, order-8 PPM modeling, context mixed SSE, and heavy stride analysis. |

---

## Encryption & Security

Archives can be encrypted using standard password protection:
- **Key Derivation**: Argon2id is used with unique salt entropy to derive a 256-bit symmetric key. This is a CPU and memory-hard algorithm designed to prevent GPU-accelerated brute-force attacks.
- **Stream Encryption**: AES-256-GCM is used to encrypt 1MB solid blocks. To prevent block reordering or truncation attacks, the block index is embedded in the Galois counter mode additional authenticated data (AAD).
- **Sentinel Checks**: A password verification sentinel is written in the plaintext bootstrap section, allowing instantaneous incorrect-password detection before the decompression pipeline consumes CPU resources or writes file data.

---

## Building from Source

To compile the library with native optimizations and target architecture SIMD vector instructions, build with:

```bash
RUSTFLAGS="-C target-cpu=native" cargo build --release
```

---

## License

FLUX is dual-licensed under:
- **GNU General Public License v3** (see [LICENSE-GPL.txt](../../LICENSE-GPL.txt))
- **FLUX Commercial License** (see [LICENSE-COMMERCIAL.txt](../../LICENSE-COMMERCIAL.txt))

Commercial licensing is required for proprietary integrations and closed-source applications.

---

## Contributing

We welcome contributions! Please review our guidelines in [CONTRIBUTING.md](../../CONTRIBUTING.md) before submitting pull requests.
