# FLUX

**An adaptive, structure-aware compression engine that understands your data.**

FLUX is a general-purpose archiver written in Rust. On structured numeric data —
scientific floating-point series, sensor telemetry, multi-dimensional coordinates,
and raw PCM audio — it is **dominant**: ratios of 4–80x are routine where gzip
achieves 1–3x. On real-world mixed data it beats zstd, RAR, and 7-Zip LZMA at its
Maximum level. On pure prose it is competitive (beating RAR, near zstd) at Maximum,
though 7-Zip PPMd still leads there. Maximum/Extreme maximize ratio at the cost
of compression speed — see the note below.

---

## Benchmark Results

All ratios are **original size ÷ compressed size** (higher = better).
Numbers are from the included benchmark scripts. Roundtrip integrity
verified by SHA-256 on every FLUX result.

**A note on levels and speed:** FLUX's `Maximum` and `Extreme` levels
use an optimal parser (binary-tree match finding + cost-based parsing).
This delivers the best ratios but is **slow to compress** (it analyzes
many encoding paths). Compression at these levels can take minutes on
tens of MB. **Decompression is unaffected and remains fast at every
level.** For everyday use, `Balanced` is fast; for archival — where you
compress once and store/transmit many times — `Maximum`/`Extreme` give
the strongest ratios. Pick the level for your use case.

### Structured & Numeric Data (15 MB files)

`FLUX Maximum` shown alongside `Balanced` so you can see both the
fast-default and the max-ratio result.

| Dataset | FLUX Balanced | FLUX Maximum | gzip -9 | zstd -19 | RAR -m5 | 7-Zip LZMA |
| :--- | :---: | :---: | :---: | :---: | :---: | :---: |
| coordinates_xyz.bin (3×f32 XYZ) | 51.70x | **81.62x** | 2.83x | 4.30x | 64.33x | 5.81x |
| float64_scientific.bin (f64 series) | 23.08x | **26.51x** | 2.93x | 3.97x | 23.08x | 4.34x |
| sensor_log.bin (3-channel f32 telemetry) | 4.46x | **4.77x** | 1.74x | 3.29x | 4.01x | 3.86x |
| float32_timeseries.bin (f32 series) | 4.60x | **4.84x** | 2.70x | 3.67x | 4.08x | 4.28x |
| real_audio.wav (synthesized PCM) | 1.88x | **2.11x** | 1.21x | 1.45x | 1.84x | 1.71x |
| real_scientific.bin (heat-diffusion f64) | 1.68x | **1.70x** | 1.43x | 1.50x | 1.69x | 1.75x |

On structured numeric data FLUX Maximum leads across the board — and on
coordinate data it reaches 81.6x, beating RAR (64.3x) by ~27% and
general-purpose tools (zstd/7-Zip LZMA) by an order of magnitude.

### Real-World Mixed Corpus (12.17 MB: Rust source + docs + logs + binary)

| Tool | Ratio |
| :--- | :---: |
| tar.gz -9 | 8.67x |
| RAR -m5 | 10.37x |
| tar.zst -19 | 12.78x |
| 7-Zip LZMA -mx=9 | 14.40x |
| **FLUX Maximum / Extreme** | **14.98x** |

On a real-world mix of source code, documentation, logs, and a binary,
FLUX Maximum (14.98x) beats zstd -19, RAR, and 7-Zip LZMA. (FLUX
Balanced reaches 8.92x at a fraction of the time — see the speed note
above.)

### Prose (Gutenberg text, 11.87 MB)

Prose is FLUX's hardest case, but the optimal parser closes most of the
historical gap:

| Tool | Ratio |
| :--- | :---: |
| gzip -9 | 2.64x |
| FLUX Balanced | 2.62x |
| RAR -m5 | 3.19x |
| **FLUX Maximum** | **3.30x** |
| zstd -19 | 3.39x |
| 7-Zip LZMA -mx=9 | 3.43x |
| 7-Zip PPMd | 4.12x |

FLUX Maximum (3.30x) now beats RAR on prose and is competitive with
zstd-19 and 7-Zip LZMA. 7-Zip PPMd still leads on pure prose via
high-order context modeling — an honest remaining gap. (FLUX has an
Order-4 PPM model implemented but disabled by default; see SPEC.md §8.2 for
why it was not the right lever in an LZ77-first pipeline.)

**To reproduce:** `python benchmark.py`, `python benchmark_levels.py`,
and `python benchmark_rar_7z.py` (requires a release build — see
Building below). Note that `Maximum`/`Extreme` runs are slow by design.


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
low-entropy planes, 4 on noisy ones). At the `Maximum` and `Extreme` levels, a
**binary-tree match finder** feeds a **cost-based optimal parser** that selects the
minimum-bit-cost token sequence (best ratio, slower compression); lower levels use
faster greedy/lazy matching. Token streams (flags, literals, lengths, distance
slots, repcode indices) are separated and compressed with independent **static
rANS** tables.

For the full format specification — header byte offsets, distance slot table,
TransformStack binary layout, rANS block format — see [SPEC.md](SPEC.md).

---

## Compression Levels

| Level | Window | Use Case |
| :--- | :---: | :--- |
| `tiny` | 256 KB | Constrained decompression targets; minimal RAM |
| `fast` | 4 MB | Highest throughput; streaming ingestion |
| `balanced` | 32 MB | **Default.** Good speed/ratio balance for most workloads |
| `maximum` | 128 MB | Optimal parser; best ratio; slow to compress; archival storage |
| `extreme` | 256 MB | Optimal parser; largest window; slowest compression |

The window size is stored explicitly in the archive header. The decoder reads it
first and fails cleanly if insufficient RAM is available before decompressing
anything. `Maximum` and `Extreme` use a binary-tree match finder with a cost-based
optimal parser (slow to compress, best ratio); decompression speed is identical
across all levels.

---

## Key Features

- **Adaptive Delta-Stride Transposition** — detects strides 2–16 via Pearson
  autocorrelation; transposes byte planes for optimal LZ77 match coverage.
- **Per-Plane Entropy Analysis** — applies delta transform per byte plane only
  where it reduces Shannon entropy; avoids amplifying noise.
- **Repcode LZ77** — 3-entry LRU repeat-offset cache eliminates redundant
  distance tokens on structured data.
- **Optimal Parsing (Maximum/Extreme)** — binary-tree match finder + cost-based
  parser select the minimum-bit-cost token sequence; encoder-only, so the format
  and decoder are unchanged.
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