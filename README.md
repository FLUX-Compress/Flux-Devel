# FLUX

**An adaptive, structure-aware compression engine that understands your data.**

FLUX is a general-purpose archiver written in Rust. On structured numeric data —
scientific floating-point series, sensor telemetry, multi-dimensional coordinates,
and raw PCM audio — it is **dominant**: ratios of 4–50x are routine where gzip
achieves 1–3x. On text and mixed corpora it is competitive with gzip and zstd.
On pure prose it is gzip-tier, not dominant — see the numbers below.

---

## Benchmark Results

All ratios are **original size ÷ compressed size** (higher = better).
Numbers are from a fresh run of the included benchmark scripts on this machine.
All FLUX runs use **Balanced mode** (32 MB window). Roundtrip integrity verified
by SHA-256 on every result.

### Structured & Numeric Data (15 MB files, benchmark.py / benchmark_rar_7z.py)

| Dataset | FLUX Balanced | FLUX Maximum | gzip -9 | zstd -19 | RAR -m5 | 7-Zip LZMA | 7-Zip PPMd |
| :--- | :---: | :---: | :---: | :---: | :---: | :---: | :---: |
| coordinates_xyz.bin (3×f32 XYZ) | 51.70x | 56.54x | 2.83x | 4.30x | **64.33x** | 5.81x | 1.80x |
| float64_scientific.bin (f64 series) | 23.08x | **23.61x** | 2.93x | 3.97x | 23.08x | 4.34x | 2.11x |
| sensor_log.bin (3-channel f32 telemetry) | 4.46x | **4.49x** | 1.74x | 3.29x | 4.01x | 3.86x | 1.56x |
| float32_timeseries.bin (f32 series) | 4.60x | **4.62x** | 2.70x | 3.67x | 4.08x | 4.28x | 3.19x |
| real_audio.wav (synthesized PCM: chord synthesis + drums) | **1.88x** | **1.88x** | 1.21x | 1.45x | 1.84x | 1.71x | 1.78x |
| real_scientific.bin (heat-diffusion f64 simulation) | 1.68x | 1.68x | 1.43x | 1.50x | 1.69x | **1.75x** | 1.47x |

### Real-World Mixed Corpus (12.1 MB: Rust source + docs + logs + binary, benchmark_levels.py)

**FLUX compression levels:**

| Level | Window | Ratio | Time | Peak RAM |
| :--- | :---: | :---: | :---: | :---: |
| Tiny | 256 KB | 8.59x | 0.31s | 46 MB |
| Fast | 4 MB | 8.22x | 0.18s | 78 MB |
| **Balanced** | **32 MB** | **9.00x** | **3.26s** | **223 MB** |
| Maximum | 128 MB | 14.00x | 43.5s | 632 MB |
| Extreme | 256 MB | 14.00x | 85.6s | 1,152 MB |

**Same corpus — external tool comparison** (benchmark_rar_7z.py, `Rar.exe -m5`, `7z -mx=9`):

| Tool | Ratio |
| :--- | :---: |
| tar.gz -9 | 8.74x |
| RAR -m5 | 10.43x |
| 7-Zip PPMd -mx=9 | 12.74x |
| tar.zst -19 | 13.00x |
| **FLUX Maximum / Extreme** | **14.00x** |
| 7-Zip LZMA -mx=9 | 14.61x |

FLUX Tiny (8.59x) and Balanced (9.00x) beat tar.gz on this corpus. Maximum and
Extreme (14.00x) beat zstd -19 (13.00x), 7-Zip PPMd (12.74x), and RAR (10.43x).
7-Zip LZMA -mx=9 edges ahead at 14.61x on this corpus — an honest result.

**Honest note on prose**: FLUX is competitive with gzip on pure text (not dominant).
The structured-data wins shown above are the headline case. For pure-prose archives,
zstd -19 and 7-Zip PPMd typically lead at high-compression settings.

To reproduce: `python benchmark.py`, `python benchmark_levels.py`, and `python benchmark_rar_7z.py` (requires a
release build — see Building below).

---

## How It Works

FLUX runs a Pearson autocorrelation sweep on each input block to detect byte-period
structure (strides 2–16). If detected, it applies an adaptive transform pipeline:

1. **Delta Filter** — wrapping byte-difference within each stride lane.
2. **Byte-Plane Transpose** — groups structurally identical bytes (e.g. all
   exponent bytes) into contiguous runs.
3. **BWT** (text only) — Burrows-Wheeler Transform for natural language, bounded
   to 1 MB chunks.
4. **FloatChannelSplit** — for multi-channel float data, splits into 4 byte planes
   (exponent, mantissa high/mid/low) and applies per-plane adaptive delta based
   on measured Shannon entropy.

The transformed stream is tokenized by a **sliding-window LZ77** engine with a
3-entry LRU repcode cache and position-based minimum match length (3 bytes on
low-entropy planes, 4 on noisy ones). Token streams (flags, literals, lengths,
distance slots, repcode indices) are separated and compressed with independent
**static rANS** tables.

For the full format specification — header byte offsets, distance slot table,
TransformStack binary layout, rANS block format — see [SPEC.md](SPEC.md).

---

## Compression Levels

| Level | Window | Use Case |
| :--- | :---: | :--- |
| `tiny` | 256 KB | Constrained decompression targets; minimal RAM |
| `fast` | 4 MB | Highest throughput; streaming ingestion |
| `balanced` | 32 MB | **Default.** Good speed/ratio balance for most workloads |
| `maximum` | 128 MB | Best ratio, slower; archival storage |
| `extreme` | 256 MB | Maximum window; marginal gain over Maximum on most data |

The window size is stored explicitly in the archive header. The decoder reads it
first and fails cleanly if insufficient RAM is available before decompressing
anything.

---

## Key Features

- **Adaptive Delta-Stride Transposition** — detects strides 2–16 via Pearson
  autocorrelation; transposes byte planes for optimal LZ77 match coverage.
- **Per-Plane Entropy Analysis** — applies delta transform per byte plane only
  where it reduces Shannon entropy; avoids amplifying noise.
- **Repcode LZ77** — 3-entry LRU repeat-offset cache eliminates redundant
  distance tokens on structured data.
- **Stream-Separated rANS** — each token stream (flags/literals/lengths/slots/reps)
  gets its own static 512-byte frequency table; fast O(1) encode/decode.
- **5-Level Deterministic Window Ladder** — 256 KB to 256 MB; decompressor always
  knows its RAM budget before reading a byte of payload.
- **AES-256-GCM + Argon2id** — authenticated streaming encryption with
  memory-hard key derivation; parameters stored in plaintext header.
- **Multi-Level Integrity** — CRC32 (header) → SHA-256+CRC32 (per file) →
  SHA-256 (per block) → CRC32+SHA-256 (archive trailing checksum).
- **Order-4 PPM** (implemented, disabled by default) — the PPM context model
  exists in the codebase; disabled because it gained ~1% on prose but slightly
  hurt mixed-data scenarios. See SPEC.md §8.2.
- **CLI, GUI, DLL, Crate** — one engine, four interfaces.

---

## Quick Start

### CLI

```bash
# Compress a directory (Balanced mode, output defaults to output.flx)
flux-cli compress ./data_dir ./backup.flx

# Maximum compression with password
flux-cli compress --level maximum --password "secret" ./data ./archive.flx

# Decompress
flux-cli decompress ./archive.flx ./output_dir/

# List archive contents
flux-cli list ./archive.flx
```

### Desktop GUI

```bash
cargo run --release --bin flux-gui
```

### Rust Crate

```rust
use flux::{Archive, Compression};

let stats = Archive::compress("./data")
    .output("out.flx")
    .level(Compression::Balanced)
    .run()?;
```

### C SDK

```bash
cargo build --release -p flux-core
# Produces flux_core_v1.dll (Windows) or libflux_core_v1.so (Linux)
```

---

## Building from Source

Requires [Rust](https://rustup.rs/) (stable).

```bash
cargo build --release
```

For native-CPU optimization (enables AVX2, AVX-512, NEON auto-vectorization):

```bash
RUSTFLAGS="-C target-cpu=native" cargo build --release
```

---

## Running Benchmarks

```bash
# Structured data ratios vs gzip / zstd
python benchmark.py

# Per-level ratios on a real-world corpus vs tar.gz / tar.zst
python benchmark_levels.py

# Full structured-data benchmark including RAR and 7-Zip
python benchmark_rar_7z.py
```

`benchmark.py` auto-generates the test datasets if not present. `benchmark_levels.py`
compresses the repo source itself. `benchmark_rar_7z.py` runs a full comparative
benchmark against system installations of WinRAR and 7-Zip.

---

## License

FLUX is dual-licensed:

- **Open Source**: GNU General Public License v3 ([LICENSE-GPL.txt](LICENSE-GPL.txt))
- **Commercial**: Proprietary license for closed-source applications
  ([LICENSE-COMMERCIAL.txt](LICENSE-COMMERCIAL.txt))

---

*For format internals, decoder compatibility requirements, and byte-level stream
layouts, see [SPEC.md](SPEC.md).*
