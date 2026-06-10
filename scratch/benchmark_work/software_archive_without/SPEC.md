# FLUX Format and Algorithm Specification
**Version 1.2 — June 2026**

This document describes the archive file format, compression pipeline, and stream
specifications of the FLUX compression engine. It is intended to be accurate enough
for an independent developer to build a compatible decoder. All byte offsets and
field layouts were derived directly from the serialization/deserialization code in
`crates/flux-core/src/`.

> **Version 1.2 note:** This release adds an optimal parser (binary-tree match
> finding + cost-based parsing) at the Maximum/Extreme levels (§5.6.1). This is an
> **encoder-only** change — the on-disk format is **unchanged from 1.1**, and
> archives produced by 1.1 and 1.2 are mutually compatible.

---

## 1. Overview and Design Philosophy

FLUX is a structured-data-aware compression engine designed to bridge the gap between
heavy, layout-agnostic general compressors (such as `gzip` and `zstd`) and specialized
scientific format filters (such as HDF5 byte filters).

The core philosophy of FLUX is **adaptive structural decomposition**. Raw bytes from
multi-dimensional coordinates, scientific float tables, or stereo sensor telemetry
contain separate byte "planes" playing distinct mathematical roles. For example, a
32-bit floating-point stream has exponent/sign bytes that vary slowly (low entropy)
alongside mantissa bytes containing physical measurement noise (high entropy).

By analyzing the structure of these streams dynamically:
1. FLUX de-interleaves and groups bytes of the same role together.
2. It measures Shannon entropy to decide whether to apply delta filters on each
   individual plane.
3. It scales the minimum match lengths of the sliding-window encoder dynamically
   based on plane predictability.
4. It compresses the resulting separated streams using a fast, static
   range-Asymmetric Numeral Systems (rANS) entropy coder.

---

## 2. Compression Levels

FLUX provides five preset compression levels. The level controls the sliding-window
size (and therefore block size and memory usage), the match-finding strategy, and
the parser.

Source: `crates/flux-core/src/ffi.rs` — `FluxCompressionLevel` enum and
`window_size_for_level()`.

| Level | Enum Value | Window / Block Size | Description |
| :--- | :---: | :---: | :--- |
| **Tiny** | 0 | 256 KB (262,144 bytes) | Minimal memory footprint; for constrained decompression targets. |
| **Fast** | 1 | 4 MB (4,194,304 bytes) | Greedy matching, highest throughput, lower ratio. |
| **Balanced** | 2 | 32 MB (33,554,432 bytes) | Default. Hash-chain lazy matching. Good balance of speed and ratio. |
| **Maximum** | 3 | 128 MB (134,217,728 bytes) | Binary-tree match finder + cost-based optimal parser (best ratio; slow to compress). |
| **Extreme** | 4 | 256 MB (268,435,456 bytes) | As Maximum, with the largest window. Slowest compression. |

Levels Tiny/Fast/Balanced use a hash-chain match finder with greedy/lazy parsing
(fast). Levels Maximum/Extreme use a binary-tree match finder with a cost-based
optimal parser (§5.6.1) — substantially slower to compress but achieving the best
ratios. **Decompression speed is identical across all levels**; the parser choice is
an encode-time concern only.

The `window_size` stored in the plaintext bootstrap header is the **decoder's source
of truth** for memory allocation. The decoder reads and allocates the window before
processing any compressed data, and fails cleanly if insufficient RAM is available
(see §6).

---

## 3. Archive File Layout

A FLUX solid archive is stored linearly and is structured to support random-access
file directory traversal, robust error recovery, and optional password-based
encryption.

```
+--------------------------------------------------------------+
| 3.1 Plaintext Bootstrap Header (variable, ≥100 bytes)       |
+--------------------------------------------------------------+
| 3.2 Encrypted Header Section (70 bytes raw / 98 bytes GCM)  |
+--------------------------------------------------------------+
| 3.3 Front File Index Payload (variable length, encrypted)    |
+--------------------------------------------------------------+
| 3.4 Solid Block 1 (66-byte block header + sub-blocks)        |
+--------------------------------------------------------------+
| 3.4 Solid Block 2 ...                                        |
+--------------------------------------------------------------+
| 3.5 Back File Index Backup (exact copy of Front Index)       |
+--------------------------------------------------------------+
| 3.6 Archive Trailing Checksum (36 bytes: CRC32 + SHA-256)   |
+--------------------------------------------------------------+
```

### 3.1 Plaintext Bootstrap Header

The archive opens with a plaintext header. A CRC32 of all preceding bytes is
appended as the final 4 bytes, allowing the decoder to detect corruption before
trusting any field.

Source: `crates/flux-core/src/crypto/header.rs` — `PlaintextHeader::serialize()` /
`PlaintextHeader::deserialize()`.

All multi-byte integers are **little-endian**.

| Byte Offset | Field | Size | Type | Description |
| :--- | :--- | :---: | :---: | :--- |
| 0–3 | **Magic** | 4 | `[u8; 4]` | `0x46 0x4C 0x55 0x58` ("FLUX") |
| 4 | **Version Major** | 1 | `u8` | Format major version (currently 1) |
| 5 | **Version Minor** | 1 | `u8` | Format minor version (currently 0) |
| 6 | **Compression Level** | 1 | `u8` | Level enum value (0–4, see §2) |
| 7–10 | **Window Size** | 4 | `u32` LE | Decompression window size in bytes — **source of truth**; decoder allocates this before reading any block |
| 11–14 | **Block Size** | 4 | `u32` LE | Nominal block size in bytes (equals window size for that level) |
| 15 | **Flags** | 1 | `u8` | Bit 0: `is_encrypted` (1 = archive body is AES-256-GCM encrypted) |
| 16–19 | **Argon2 Memory** | 4 | `u32` LE | KDF memory parameter in KB (default: 65,536) |
| 20–23 | **Argon2 Iterations** | 4 | `u32` LE | KDF iteration count (default: 3) |
| 24–27 | **Argon2 Parallelism** | 4 | `u32` LE | KDF parallelism degree (default: 4) |
| 28–43 | **KDF Salt** | 16 | `[u8; 16]` | Salt for Argon2id key derivation |
| 44–55 | **Encrypted Header IV** | 12 | `[u8; 12]` | GCM IV for the encrypted header section (§3.2) |
| 56–63 | **Sentinel Chunk Index** | 8 | `u64` LE | Chunk index of the password-verification sentinel |
| 64–75 | **Sentinel IV** | 12 | `[u8; 12]` | GCM IV for the sentinel |
| 76–91 | **Sentinel Auth Tag** | 16 | `[u8; 16]` | GCM authentication tag for the sentinel |
| 92–95 | **Sentinel CT Length** | 4 | `u32` LE | Length in bytes of the sentinel ciphertext |
| 96–(96+N−1) | **Sentinel Ciphertext** | N | `[u8; N]` | AES-256-GCM encrypted sentinel payload |
| (96+N)–(99+N) | **Header CRC32** | 4 | `u32` LE | CRC32 of all preceding bytes (bytes 0 through 95+N) |

**Minimum total size**: 100 bytes (with N=0 sentinel; in practice N ≥ 1 so ≥ 101 bytes).

The decoder **must** verify the CRC32 before reading any other field. If it fails,
return `CorruptData`.

### 3.2 Encrypted Header Section

Immediately follows the plaintext bootstrap header. This section contains archive
metadata readable only with the correct key.

The raw payload is 70 bytes, serialized in this order:

| Field | Size | Type | Description |
| :--- | :---: | :---: | :--- |
| File Index Offset | 8 | `u64` LE | Absolute byte offset to the Front File Index |
| File Index Size | 8 | `u64` LE | Length of the file index payload in bytes |
| Solid Block Count | 4 | `u32` LE | Total number of solid blocks |
| Compression Level | 1 | `u8` | Level enum (informational, duplicates plaintext header) |
| Transform Flags | 1 | `u8` | Transform pipeline flags |
| Original Total Size | 8 | `u64` LE | Sum of all uncompressed file sizes |
| Compressed Total Size | 8 | `u64` LE | Sum of all compressed block sizes |
| Archive SHA-256 | 32 | `[u8; 32]` | SHA-256 of the entire archive body |

If `is_encrypted` is 1: the 70-byte payload is AES-256-GCM encrypted. The encrypted
section stored on disk is `IV (12 bytes) + Ciphertext+Tag (70+16 = 86 bytes)` = **98 bytes**.

If `is_encrypted` is 0: the 70-byte payload is stored as raw plaintext.

### 3.3 The File Index

The File Index maps directories and files to solid blocks. It is serialized in a
custom binary format, placed at the front of the archive body, and duplicated at
the back (§3.5) to protect against truncation or header corruption.

An index entry for a file includes:
- `path` (variable-length UTF-8 string)
- `original_size` (`u64`)
- `permissions` (`u32`)
- `modified_time`, `created_time`, `accessed_time` (`u64` each, Unix timestamps)
- `solid_block_id` (`u32`)
- `block_offset` (`u64`)
- `original_sha256` (`[u8; 32]`) and `original_crc32` (`u32`)

### 3.4 Solid Block Structure

Each solid block has a fixed **66-byte block header** followed by the compressed
payload.

Source: `crates/flux-core/src/lib.rs` — block header write/read at offsets 0..66.

#### Solid Block Header (66 bytes)

| Offset | Field | Size | Type | Description |
| :--- | :--- | :---: | :---: | :--- |
| 0–3 | Block ID | 4 | `u32` LE | Sequential block identifier |
| 4 | Block Type Tag | 1 | `u8` | 0=Text, 1=Binary, 2=Multimedia, 3=Mixed, 4=Raw |
| 5–12 | Compressed Size | 8 | `u64` LE | Compressed payload size in bytes |
| 13–20 | Uncompressed Size | 8 | `u64` LE | Uncompressed payload size in bytes |
| 21–52 | Compressed Hash | 32 | `[u8; 32]` | SHA-256 of the compressed payload |
| 53–65 | TransformStack | 13 | Binary | Default (zeroed) transform stack (see §5.8) |

The block payload is the concatenation of **sub-blocks** (§3.5). If
`is_encrypted` is 1, the payload is AES-256-GCM encrypted as a single chunk with
its own 12-byte IV and 16-byte auth tag prepended.

### 3.5 Sub-Block Structure

Within each solid block, data is divided into sub-blocks of up to 1 MB
(1,048,576 bytes) of uncompressed input. Each sub-block has a **21-byte header**
followed immediately by its compressed data.

Source: `crates/flux-core/src/lib.rs` — sub-block header read at `sub_pos`.

#### Sub-Block Header (21 bytes)

| Offset | Field | Size | Type | Description |
| :--- | :--- | :---: | :---: | :--- |
| 0–3 | Uncompressed Size | 4 | `u32` LE | Original uncompressed size of this sub-block |
| 4–7 | Compressed Size | 4 | `u32` LE | Compressed size of this sub-block (bytes that follow) |
| 8–20 | TransformStack | 13 | Binary | Serialized transform decisions (see §5.8) |

The `compressed_size` bytes immediately following the header are the rANS-encoded
LZ77 token stream (§5.9).

### 3.6 Archive Trailing Checksum

A 36-byte block at the end of the archive:
- **CRC32** (`u32` LE): CRC32 of the entire archive body.
- **SHA-256** (`[u8; 32]`): SHA-256 of the entire archive body.

---

## 4. Compression Pipeline Overview

```
       [Input File Data]
              │
              ▼
     [Content Classifier]
     Pearson autocorrelation for strides S ∈ [2,16]
              │
    ┌─────────┴─────────┐
    ▼ (stride detected)  ▼ (no stride)
[Transform Stack]        │
 1. Delta Filter         │
 2. Byte Plane Transpose │
 3. BWT (text only)      │
 4. Media Filter         │
    │                    │
    └──────────┬─────────┘
               ▼
     [LZ77 Tokenizer]
  - 3-entry LRU repcode cache
  - Position-based min_match (3 or 4)
  - Greedy/lazy (Tiny/Fast/Balanced)
    or optimal parser (Maximum/Extreme, §5.6.1)
               │
               ▼
  [Stream-Separated Serialization]
  Flags / Literals / Lengths / Slots / Reps / Extra bits
               │
               ▼
     [rANS Entropy Coding]
  Static 512-byte frequency tables (sum=4096)
               │
               ▼
  [Optional AES-256-GCM encryption]
               │
               ▼
       [Compressed Sub-Block]
```

---

## 5. Compression Pipeline — Detailed

### 5.1 Content Classification & Stride Detection

For every sub-block (≤ 1 MB), a Pearson autocorrelation coefficient is computed for
all strides S ∈ [2, 16]. The Pearson coefficient r at stride S over sample X of
size N is:

$$r(S) = \frac{(N-S)\sum x_i y_i - \sum x_i \sum y_i}{\sqrt{[(N-S)\sum x_i^2 - (\sum x_i)^2][(N-S)\sum y_i^2 - (\sum y_i)^2]}}$$

where x_i = X[i] and y_i = X[i+S].

A stride S is confirmed if r(S) ≥ 0.50. If confirmed, the block is routed to the
specialized transform pipeline.

Source: `crates/flux-core/src/analysis/classifier.rs`.

### 5.2 Delta Filter

When a stride S is detected, a wrapping byte-level difference transform is applied
across the stride:

    output[i] = input[i] − input[i − S]  (mod 256)

This decorrelates smoothly varying fields (temperatures, positions) into near-zero
residuals that LZ77 can match as long runs.

The delta filter is only applied if it reduces the Shannon entropy of the block.

Source: `crates/flux-core/src/transform/delta.rs`.

### 5.3 Byte Plane Transpose

After delta-filtering, the byte stream is transposed by grouping together bytes at
the same stride offset:

    [b0, b1, ..., bS-1, bS, bS+1, ..., b2S-1, ...]
    → [b0, bS, b2S, ... | b1, bS+1, b2S+1, ... | ...]

This concentrates structurally similar bytes into contiguous runs, improving LZ77
match lengths significantly.

Source: `crates/flux-core/src/transform/transpose.rs`.

### 5.4 Burrows-Wheeler Transform (BWT)

BWT is applied only to text-like content (entropy < 5.5 bits/byte) where it
re-orders the stream to group common context suffixes together, dramatically
improving LZ77 match quality on natural language.

BWT is internally bounded to **1,048,576-byte (1 MB) chunks**. Data larger than
this limit is split into independent 1 MB chunks, each BWT-transformed separately.
The on-disk layout for multi-chunk BWT is:
- `chunk_count` (`u32` LE)
- `primary_index[0]` through `primary_index[chunk_count-1]` (`u32` LE each)
- Concatenated BWT-transformed chunk data

The `bwt_primary_index` field in the TransformStack stores the primary index for
single-chunk transforms; multi-chunk decoding uses the embedded count+index array.

Source: `crates/flux-core/src/transform/bwt.rs` — `CHUNK_LIMIT = 1_048_576`.

### 5.5 Media-Specific Filters (FloatChannelSplit)

When the detected stride is a multiple of 4 (S = 4×C), the block contains C
floating-point channels. `FloatChannelSplit` applies:

1. **De-interleaving**: Split the input into C streams of `f32` values.
2. **Byte-plane splitting**: Reorganize into 4 global planes of size N×C:
   - **Plane 3**: Exponent & sign (MSB, byte 3 of each float).
   - **Plane 2**: High mantissa (byte 2).
   - **Plane 1**: Mid mantissa (byte 1).
   - **Plane 0**: Low mantissa (LSB, byte 0).
3. **Adaptive per-plane delta**: For each plane, compute Shannon entropy before
   and after delta-1 wrapping subtraction (X_i − X_{i-1} mod 256). Apply delta
   only if it reduces entropy.
4. **Low-entropy flagging**: If the final entropy of a plane is < 5.0 bits/byte,
   mark it as low-entropy (allowing 3-byte LZ77 matches).
5. **Mask byte serialization**: The per-plane decisions are packed into an 8-bit
   `mask` byte stored in the TransformStack:
   - **Bits 0–3**: Delta-applied flag for planes 0–3.
   - **Bits 4–7**: Low-entropy flag (min_match = 3 allowed) for planes 0–3.

Other media filter types:
- `AudioDelta`: Inter-sample/inter-channel delta for PCM audio.
- `FloatSplit`: Simple byte-plane split for single-channel f32 streams.
- `RgbSplit`: Split RGB pixels into contiguous R, G, B planes.
- `RgbaDelta`: Split RGBA pixels with per-channel deltas.

### 5.6 LZ77 with Repcodes and Position-Based Match Length

The LZ77 engine parses the (possibly transformed) buffer using a sliding window of
size equal to the level's window size (up to 256 MB). It uses:

- **LRU Repcode Cache**: Tracks the 3 most recently encoded match distances.
  A `RepMatch` token costs only a 2-bit index, dramatically reducing overhead on
  structured data with repeating strides.
- **Position-based min_match**: Low-entropy planes (flagged in the mask byte)
  allow 3-byte minimum matches using a `hash3` multiplicative hash:
  $$H_3(pos) = ((X[pos] \ll 16) \mid (X[pos+1] \ll 8) \mid X[pos+2]) \times 50683 \pmod{2^{15}}$$
  Noisy planes enforce a 4-byte minimum using `hash4`.

**Token types:**

| Type | Encoding | Description |
| :--- | :--- | :--- |
| `Literal(u8)` | flag = 0 | Unmatched raw byte |
| `Match { distance, length }` | flag = 1 | Back-reference by distance slot + extra bits |
| `RepMatch { index, length }` | flag = 2 | Reference to repcode cache entry 0, 1, or 2 |

At the Tiny, Fast, and Balanced levels, matches are selected by a hash-chain match
finder with greedy (Fast) or lazy (Balanced) selection. At the Maximum and Extreme
levels, match selection is performed by the optimal parser described in §5.6.1.

### 5.6.1 Optimal Parsing (Maximum / Extreme levels)

At the Maximum and Extreme levels, FLUX replaces greedy/lazy match selection with a
binary-tree match finder feeding a cost-based optimal parser. This is an
**encoder-only** change: it selects a different (cheaper) sequence of the same token
types defined in §5.6, so the archive format and the decoder are unchanged. Lower
levels are unaffected.

Source: `crates/flux-core/src/compress/lz77.rs` — `MatchFinder::BinaryTree`,
`find_all_matches()`, `encode_optimal()`.

**Binary-tree match finder.** Window positions are indexed in a binary search tree
ordered by suffix comparison (two `u32` link arrays, `bt_left` / `bt_right`;
~8 bytes per window position, allocated only on Maximum/Extreme). Unlike the
depth-capped hash chains used at lower levels, the tree reliably returns the longest
match and can return *multiple* candidate matches (`find_all_matches`) for the parser
to choose among. Tree search is decoupled from tree insertion, so the parser may
inspect candidates at look-ahead positions without mutating the structure. Search
honors a node-comparison cap and a "good enough" target length for tunable speed.

**Two-pass entropy-aware pricing.** Because FLUX uses static per-block rANS tables
(§5.10), the parser needs cost estimates before final encoding. Each sub-block is
processed in two passes:

1. *Stats pass*: a fast provisional parse produces approximate token frequencies,
   from which per-symbol bit-prices are derived for each of the five streams (flags,
   literals, lengths, distance slots, repcode indices). Prices use a deterministic
   fixed-point log2 (a precomputed fractional table in 24.8 fixed-point) — no
   floating-point, ensuring identical output across platforms.
2. *Optimal pass*: a forward dynamic program computes, for each input position, the
   minimum-bit-cost path to reach it. State per position includes the accumulated
   price, a back-reference to recover the chosen token, and the 3-entry LRU
   repcode-cache state (because a future `RepMatch`'s price depends on the repcode
   cache at that position). The DP is evaluated in sliding windows of 16,384 bytes,
   carrying the repcode-cache state across window boundaries.

After the optimal token sequence is recovered (by walking the back-references), the
final rANS tables are built from that sequence and the sub-block is encoded normally
(§5.9–5.10).

**Why this helps.** Greedy/longest-match parsing can be far from bit-optimal: taking
a long match at a large (expensive) distance may cost more than a slightly shorter
match at a cheap, repcode-eligible distance followed by additional matches. The
optimal parser minimizes total coded bits, which both shrinks the distance stream on
text and improves match selection on structured data.

**Cost.** Optimal parsing is much slower to compress than greedy/lazy (the parser
evaluates many candidate paths). This is the intended trade-off for the
Maximum/Extreme "best ratio" levels. Decompression is unaffected.

### 5.7 Distance Slots

Distances are encoded using a slot system that covers windows up to 256 MB.
Slots 0–55 are used; slot 55 can encode distances up to ~603 MB (sufficient for
any supported window size).

Source: `crates/flux-core/src/compress/lz77.rs` — `distance_to_slot()` / `slot_base()`.

| Slot Range | Distance Range | Extra Bits |
| :--- | :--- | :---: |
| 0–3 | 1–4 (direct) | 0 |
| 4–5 | 5–8 | 1 |
| 6–7 | 9–16 | 2 |
| 8–9 | 17–32 | 3 |
| … | … | … |
| 54–55 | ~268M–603M | 26 |

Slot computation (for slot ≥ 4):

    exponent = slot / 2
    base (even slot) = (1 << exponent) + 1
    base (odd slot)  = (1 << exponent) + 1 + (1 << (exponent-1))
    extra_bits = (slot - 2) / 2

Slot 55 is the maximum; any distance ≥ base(55) saturates to slot 55.

### 5.8 TransformStack Binary Format (13 bytes)

The 13-byte TransformStack is written in the sub-block header and describes
all transforms applied to that sub-block.

Source: `crates/flux-core/src/transform/mod.rs` — `TransformStack::serialize()` /
`TransformStack::deserialize()`.

| Byte | Field | Description |
| :---: | :--- | :--- |
| 0 | `delta_applied` | `0` or `1` |
| 1 | `delta_stride` | Stride S (0 if not applied) |
| 2 | `transpose_applied` | `0` or `1` |
| 3 | `transpose_stride` | Stride S (0 if not applied) |
| 4 | `bwt_ppm_byte` | Bit 0 = `bwt_applied`; bits 1–2 = `ppm_class` (see below) |
| 5–8 | `bwt_primary_index` | `u32` LE primary index for inverse BWT (0 if not applied) |
| 9 | `media_filter_applied` | `0` or `1` |
| 10 | `media_filter_type` | 0=None, 1=AudioDelta, 2=FloatSplit, 3=RgbSplit, 4=RgbaDelta, 5=FloatChannelSplit |
| 11 | `filter_param1` | AudioDelta: channels; FloatChannelSplit: channels; others: 0 |
| 12 | `filter_param2` | AudioDelta: bit_depth; FloatChannelSplit: mask byte; others: 0 |

**PPM class encoding** (bits 1–2 of byte 4):

| ppm_class | ppm_applied | Arena Size |
| :---: | :--- | :--- |
| 0 | false | — |
| 1 | true | 8 MB (8,388,608 bytes) |
| 2 | true | 32 MB (33,554,432 bytes) |
| 3 | true | 64 MB (67,108,864 bytes) |

**PPM status**: An Order-4 PPM context model is implemented in
`crates/flux-core/src/compress/ppm.rs` but is **disabled by default** for all
compression levels (`ppm_applied = false`). The sub-block header `ppm_class` will
be 0 for all archives produced by the current release. A decoder **must** parse the
`ppm_class` field and be prepared to handle PPM-encoded literals if ppm_class ≠ 0
for forward-compatibility.

The PPM code is retained because it was measured to gain ~1% on pure prose but
slightly hurt mixed-data scenarios. The decision to disable it is recorded in the
source comments and can be reversed by setting `ppm_applied = true` for specific
levels. See §8.2.

### 5.9 Stream-Separated Serialization (LZ77 Block Layout)

After tokenization, the tokens are serialized into a compact binary block.

Source: `crates/flux-core/src/lib.rs` — `serialize_lz77_tokens()` /
`deserialize_lz77_tokens()`.

```
[u32 LE]  num_tokens                    — total token count
[u32 LE]  flags_encoded_len             — byte length of rANS-encoded flags stream
[u32 LE]  literals_encoded_len          — byte length of rANS-encoded literals stream
                                          (= 0 if ppm_class ≠ 0; PPM-coded instead)
[u32 LE]  lengths_encoded_len           — byte length of rANS-encoded lengths stream
[u32 LE]  slots_encoded_len             — byte length of rANS-encoded distance slots
[u32 LE]  rep_indices_encoded_len       — byte length of rANS-encoded repcode indices

[512 bytes]  flags frequency table      — normalized counts, sum=4096
[512 bytes]  literals frequency table   — omitted if ppm_class ≠ 0
[512 bytes]  lengths frequency table
[512 bytes]  distance slots freq table
[512 bytes]  rep_indices freq table

[flags_encoded_len bytes]      rANS-decoded flags (2-bit packed: 0=Lit, 1=Match, 2=Rep)
[literals_encoded_len bytes]   rANS-decoded literal bytes (or PPM-encoded if ppm_class≠0)
[lengths_encoded_len bytes]    rANS-decoded match lengths (stored as length − min_match)
[slots_encoded_len bytes]      rANS-decoded distance slots (0–55)
[rep_indices_encoded_len bytes] rANS-decoded repcode indices (2-bit packed, 0–2)
[remaining bytes]              raw extra bits for distance offsets (bit-packed, LSB first)
```

When `ppm_class` = 0 (all current archives): 5 × 512 = **2,560 bytes** of table
overhead per sub-block.

When `ppm_class` ≠ 0: the literals frequency table is omitted, so 4 × 512 = **2,048
bytes** of table overhead.

Note: the optimal parser (§5.6.1) does not change this layout. It only changes which
tokens are produced; the serialized stream format is identical regardless of parser.

### 5.10 rANS Entropy Coding

Each of the five symbol streams (Flags, Literals, Lengths, Distance Slots, Rep
Indices) is encoded and decoded independently using a static range-ANS (rANS)
coder.

- **Table**: A 512-byte table stores 256 normalized frequency counts as `u16`
  values (frequencies sum to 4096 = 2¹²). This allows the decoder to reconstruct
  the cumulative distribution instantly.
- **Decoding**: The decoder reads the frequency table first, then decodes exactly
  as many symbols as indicated by the corresponding length field.
- **Extra bits**: Distance offset extra bits are stored as raw bit-packed bytes
  (not rANS-coded) because their near-uniform distribution provides no entropy
  gain from range coding.

---

## 6. Memory Safety and Allocation

### Encode-Time Warning (warn-and-proceed)

Before compressing, the encoder estimates peak memory usage:

    est_peak = window_size + input_buffer_cap + hash_table_size + hash_chains_size + 16 MB

where `hash_table_size = (1 << hash_bits) × 4` and `hash_chains_size = window_size × 4`.

If `est_peak` exceeds available system RAM (queried via `sysinfo`), a warning is
printed to stderr and compression proceeds anyway. No data is ever discarded.

Source: `crates/flux-core/src/lib.rs` — RAM check before buffer allocation.

### Decode-Time Fail-Clean (allocate-or-fail)

The decoder reads `window_size` from the plaintext bootstrap header before touching
any compressed data. It then:
1. Queries available system RAM; returns an error if `window_size > available_ram`.
2. Attempts `try_reserve_exact(window_size)` on a test vector; returns an error if
   the OS allocation fails.
3. Drops the test allocation and proceeds with decoding.

If either check fails, the decoder returns `ArchiveError::Io` with a human-readable
message indicating required vs. available MB. No partial output is produced.

Source: `crates/flux-core/src/lib.rs` — decompression entry point, RAM check block.

---

## 7. Cryptography & Integrity

### 7.1 AES-256-GCM Encryption

Applied in a **compress-then-encrypt** pipeline. Each encrypted block (including
the file index and all solid block payloads) is encrypted using an independent
12-byte random IV and a 16-byte GCM authentication tag. Authentication failure
returns `ArchiveError::DecryptionFailed`.

### 7.2 Argon2id Key Derivation

The 256-bit AES key is derived from the user's password using Argon2id with
parameters stored in the plaintext bootstrap header:
- `argon2_memory_kb` (default: 65,536 KB = 64 MB)
- `argon2_iterations` (default: 3)
- `argon2_parallelism` (default: 4)
- `kdf_salt` (16 random bytes)

### 7.3 Password Sentinel

A short AES-256-GCM encrypted sentinel block is embedded in the plaintext bootstrap
header. Before attempting to decrypt the archive body, the decoder verifies the
sentinel with the derived key. If verification fails, `ArchiveError::WrongPassword`
is returned immediately (without reading any payload).

### 7.4 Multi-Level Integrity Verification

1. **Header level**: CRC32 of the plaintext bootstrap header (verified first).
2. **File level**: CRC32 and SHA-256 of raw uncompressed file contents stored in
   the encrypted index; verified after decompression.
3. **Block level**: SHA-256 of each compressed block payload stored in the solid
   block header.
4. **Archive level**: A trailing 36-byte block contains CRC32 and SHA-256 of the
   entire archive body.

---

## 8. Design Decisions & Trade-offs

### 8.1 Static rANS vs. Adaptive Coder

We chose a static range-coder over an adaptive context-mixing encoder for general
streams. Static rANS offers fast decompression throughputs (> 150 MB/s per thread)
and keeps the code footprint compact. The frequency table per sub-block (2,560
bytes overhead at 2,048–4,096 bytes compressed) is negligible on realistic block
sizes.

### 8.2 PPM: Implemented, Disabled by Default

An Order-4 PPM context model is implemented (see §5.8). Benchmarking showed it
gained approximately 1% on pure prose but introduced a slight regression on
mixed data due to escape probability overhead in the LZ77-first pipeline (literals
represent only ~7% of compressed output after LZ77 match coverage). The code is
retained for future research and the decoder is forward-compatible; re-enabling PPM
requires setting `ppm_applied = true` in the level configuration.

### 8.3 BWT Bounded to 1 MB Chunks

BWT time complexity is O(n log n) and memory O(n). Bounding to 1 MB keeps worst-case
BWT time and memory predictable while preserving suffix-context grouping benefits on
natural language text.

### 8.4 Case Study: Per-Plane Adaptive Delta on `sensor_log.bin`

If delta coding is applied to the low-mantissa plane of noisy telemetry, the
subtraction of two uncorrelated noise elements doubles the variance, amplifying
entropy from **7.58** to **7.99** bits/byte.

By employing per-plane adaptive entropy tests, FLUX detects this noise wall:
- It delta-codes the low-entropy exponent/high-mantissa planes (reducing entropy
  to near 0).
- It leaves the noisy Plane 0 raw (preserving it at 7.27 bits/byte instead of
  amplifying it).

This targeted approach, combined with the 3-byte match optimization, allows FLUX to
compress `sensor_log.bin` to **4.46x** (Balanced), easily beating `zstd -19` (3.29x).

### 8.5 Large Deterministic Window Ladder

The five-level window ladder (256 KB → 4 MB → 32 MB → 128 MB → 256 MB) provides
deterministic decompression memory requirements. The window size is written
explicitly into the plaintext header (§3.1) so the decoder can always make a
binary go/no-go allocation decision before touching any compressed data.

### 8.6 Optimal Parsing at High Levels

Lower levels prioritize speed with hash-chain greedy/lazy matching.
Maximum/Extreme add a binary-tree match finder and a cost-based optimal parser
(§5.6.1). The parser prices each encoding choice against the actual static-rANS
stream costs (via a two-pass scheme analogous to a pre-analysis pass) and selects
the minimum-bit-cost token sequence.

Measured impact (15 MB structured datasets / 11.87 MB prose):
- coordinates_xyz: 51.7x (Balanced) → 81.6x (Maximum), exceeding RAR -m5 (64.3x).
- Gutenberg prose: 2.62x → 3.30x, with the distance stream reduced by ~15.6%;
  competitive with zstd-19 and beating RAR -m5.
- All structured datasets improved with no regressions.

The trade-off is compression speed: the optimal parser is markedly slower than
greedy/lazy (the analysis evaluates many candidate paths). Decompression is
unaffected because the parser is encoder-only — it emits the standard token
vocabulary the existing decoder reads.

---

## 9. Glossary

| Term | Meaning |
| :--- | :--- |
| **rANS** | range-Asymmetric Numeral Systems — fast entropy coder with O(1) encode/decode per symbol |
| **BWT** | Burrows-Wheeler Transform — reversible block-sort that groups repeated contexts |
| **PPM** | Prediction by Partial Matching — adaptive context model (Order-4 in FLUX, currently disabled) |
| **Optimal parser** | Cost-based dynamic-programming parser (Maximum/Extreme) that selects the minimum-bit-cost token sequence |
| **Binary-tree match finder** | Suffix-ordered binary search tree over window positions; returns multiple candidate matches for the optimal parser |
| **Repcode** | Repeat-offset cache entry; encodes a back-reference to a recently used distance |
| **Stride** | Byte period of a structured data type (e.g., 12 bytes for 3×f32 XYZ coordinates) |
| **Plane** | One of the 4 byte positions within a multi-byte data element (e.g., exponent byte, mantissa byte) |
| **Sentinel** | A small AES-GCM encrypted block used to verify password correctness without decrypting the archive |
| **Window size** | The sliding-window history available to the LZ77 matcher; also the decompression buffer size |