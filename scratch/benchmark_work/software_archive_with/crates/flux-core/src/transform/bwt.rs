//! # Burrows-Wheeler Transform (BWT)
//!
//! The Burrows-Wheeler Transform (BWT) is a reversible algorithm that rearranges a block of
//! characters to group similar contexts together. This does not change the data's content,
//! but dramatically lowers its local entropy, preparing it for highly effective context-based
//! compression (like Prediction by Partial Matching - PPM, or Run-Length Encoding).
//!
//! ## How BWT Works (Forward Transform)
//!
//! 1. Form all $N$ cyclic rotations of the input block of length $N$.
//! 2. Sort the rotations lexicographically.
//! 3. Extract the last column of the sorted rotation matrix. This column is the BWT result.
//! 4. Record the primary index $I$, which is the 0-indexed row containing the original unrotated string.
//!
//! To build the sorted cyclic rotations in $O(N \log N)$ time, we construct a cyclic suffix array
//! using the prefix-doubling algorithm (inspired by Larsson-Sadakane).
//!
//! ## LF-Mapping (Inverse Transform)
//!
//! The BWT is reversed in $O(N)$ time using the Last-to-First (LF) mapping property.
//! The LF-mapping maps the $i$-th character of the last column $L$ to its position in the
//! first column $F$ (which is simply $L$ sorted).
//!
//! Since the relative order of identical characters is identical in both $F$ and $L$:
//!
//! $$\text{LF}\[i\] = C\[L\[i\]\] + \text{occ}(L\[i\], i)$$
//!
//! where:
//! - $C\[c\]$ is the count of characters in $L$ that are lexicographically smaller than $c$.
//! - $\text{occ}(c, i)$ is the number of times character $c$ appears in $L\[0..i\]$.
//!
//! Using the LF-mapping and starting from the primary index $I$, we trace the characters
//! backwards to reconstruct the original block.

use crate::threads::signals::ContentType;

/// Performs Burrows-Wheeler Transform and its inverse on data blocks.
#[derive(Debug, Clone, Copy)]
pub struct BurrowsWheelerTransform {
    /// Maximum block size limit. Capped at 900KB (matching bzip2) to balance memory usage
    /// and O(N log N) sorting costs for large files.
    pub block_size: usize,
}

impl BurrowsWheelerTransform {
    /// Creates a new `BurrowsWheelerTransform` with a specified block size.
    pub fn new(block_size: usize) -> Self {
        Self { block_size }
    }

    /// Determines whether BWT is likely to improve compression.
    ///
    /// BWT is recommended only for `ContentType::Text` and when the block entropy is below 5.5.
    /// High-entropy files or binary streams (e.g. compressed files) do not benefit from cyclic
    /// context sorting and incur sorting overhead.
    pub fn should_apply(&self, entropy: f32, content_type: &ContentType) -> bool {
        *content_type == ContentType::Text && entropy < 5.5
    }

    /// Computes the forward Burrows-Wheeler Transform.
    ///
    /// Returns the transformed bytes and the primary index (location of the original rotation).
    pub fn transform(&self, data: &[u8]) -> (Vec<u8>, u32) {
        let n = data.len();
        if n == 0 {
            return (Vec::new(), 0);
        }

        const CHUNK_LIMIT: usize = 1_048_576;

        if n <= CHUNK_LIMIT {
            return self.transform_chunk(data);
        }

        // Multi-chunk BWT
        let mut chunks = Vec::new();
        let mut primary_indices = Vec::new();
        let mut pos = 0;

        while pos < n {
            let chunk_end = (pos + CHUNK_LIMIT).min(n);
            let chunk = &data[pos..chunk_end];
            let (tx_chunk, primary_idx) = self.transform_chunk(chunk);
            chunks.push(tx_chunk);
            primary_indices.push(primary_idx);
            pos = chunk_end;
        }

        let num_chunks = primary_indices.len() as u32;
        let mut transformed = Vec::new();
        transformed.extend_from_slice(&num_chunks.to_le_bytes());
        for &idx in &primary_indices {
            transformed.extend_from_slice(&idx.to_le_bytes());
        }
        for chunk in chunks {
            transformed.extend_from_slice(&chunk);
        }

        (transformed, 0)
    }

    fn transform_chunk(&self, data: &[u8]) -> (Vec<u8>, u32) {
        let n = data.len();
        if n == 0 {
            return (Vec::new(), 0);
        }

        // Build cyclic suffix array using prefix doubling algorithm
        let mut sa: Vec<usize> = (0..n).collect();
        let mut rank: Vec<usize> = data.iter().map(|&x| x as usize).collect();
        let mut tmp = vec![0; n];

        let mut k = 1;
        while k < n {
            sa.sort_by_key(|&i| (rank[i], rank[(i + k) % n]));

            tmp[sa[0]] = 0;
            for i in 1..n {
                let prev = sa[i - 1];
                let curr = sa[i];
                let prev_pair = (rank[prev], rank[(prev + k) % n]);
                let curr_pair = (rank[curr], rank[(curr + k) % n]);
                tmp[curr] = tmp[prev] + if curr_pair > prev_pair { 1 } else { 0 };
            }
            rank.copy_from_slice(&tmp);
            if rank[sa[n - 1]] == n - 1 {
                break;
            }
            k *= 2;
        }

        let mut transformed = Vec::with_capacity(n);
        let mut primary_index = 0u32;

        for (i, &rot_idx) in sa.iter().enumerate() {
            if rot_idx == 0 {
                primary_index = i as u32;
            }
            let last_char_idx = if rot_idx == 0 { n - 1 } else { rot_idx - 1 };
            transformed.push(data[last_char_idx]);
        }

        (transformed, primary_index)
    }

    /// Reverses the Burrows-Wheeler Transform exactly using LF-mapping.
    pub fn inverse_transform(&self, data: &[u8], primary_index: u32) -> Vec<u8> {
        let n = data.len();
        if n == 0 {
            return Vec::new();
        }

        const CHUNK_LIMIT: usize = 1_048_576;

        if n <= CHUNK_LIMIT {
            return self.inverse_transform_chunk(data, primary_index);
        }

        // Parse chunked BWT
        if n < 4 {
            return Vec::new(); // corrupt
        }
        let num_chunks = u32::from_le_bytes(data[0..4].try_into().unwrap()) as usize;
        let header_len = 4 + 4 * num_chunks;
        if n < header_len {
            return Vec::new(); // corrupt
        }

        let mut primary_indices = Vec::with_capacity(num_chunks);
        for i in 0..num_chunks {
            let idx = u32::from_le_bytes(data[4 + i * 4..8 + i * 4].try_into().unwrap());
            primary_indices.push(idx);
        }

        let payload = &data[header_len..];
        let payload_len = payload.len();
        let mut decoded = Vec::new();
        let mut pos = 0;

        for (i, &prim_idx) in primary_indices.iter().enumerate() {
            let chunk_size = if i == num_chunks - 1 {
                payload_len - pos
            } else {
                CHUNK_LIMIT
            };
            if pos + chunk_size > payload_len {
                return Vec::new(); // corrupt
            }
            let chunk = &payload[pos..pos + chunk_size];
            let decoded_chunk = self.inverse_transform_chunk(chunk, prim_idx);
            decoded.extend_from_slice(&decoded_chunk);
            pos += chunk_size;
        }

        decoded
    }

    fn inverse_transform_chunk(&self, data: &[u8], primary_index: u32) -> Vec<u8> {
        let n = data.len();
        if n == 0 {
            return Vec::new();
        }
        assert!(
            (primary_index as usize) < n,
            "Primary index out of bounds for BWT decode"
        );

        let mut counts = [0; 256];
        for &byte in data {
            counts[byte as usize] += 1;
        }

        let mut c = [0; 256];
        let mut sum = 0;
        for i in 0..256 {
            c[i] = sum;
            sum += counts[i];
        }

        let mut lf = vec![0; n];
        let mut occ = [0; 256];
        for (i, &byte) in data.iter().enumerate() {
            let b = byte as usize;
            lf[i] = c[b] + occ[b];
            occ[b] += 1;
        }

        let mut decoded = vec![0; n];
        let mut curr = primary_index as usize;
        for step in (0..n).rev() {
            decoded[step] = data[curr];
            curr = lf[curr];
        }

        decoded
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bwt_transform_known_string() {
        // Classic banana example
        let original = b"banana";
        let bwt = BurrowsWheelerTransform::new(900_000);
        let (transformed, primary_index) = bwt.transform(original);

        // "banana" rotates to sorted list:
        // 0: abanan (last: 'n')
        // 1: anaban (last: 'n')
        // 2: ananab (last: 'b')
        // 3: banana (last: 'a')  <-- original is at index 3
        // 4: nabana (last: 'a')
        // 5: nanaba (last: 'a')
        // Output: "nnbaaa", primary_index: 3
        assert_eq!(transformed, b"nnbaaa");
        assert_eq!(primary_index, 3);
    }

    #[test]
    fn test_bwt_inverse_roundtrip_short() {
        let original = b"compression";
        let bwt = BurrowsWheelerTransform::new(900_000);
        let (transformed, primary_index) = bwt.transform(original);
        let reconstructed = bwt.inverse_transform(&transformed, primary_index);
        assert_eq!(reconstructed, original);
    }

    #[test]
    fn test_bwt_inverse_roundtrip_longer() {
        let original = b"The Burrows-Wheeler Transform is an algorithm used in data compression \
                         techniques, such as bzip2. It was invented by Michael Burrows and \
                         David Wheeler in 1994. When a character string is transformed by the BWT, \
                         the transformation permutes the order of the characters. If the original \
                         string had several substrings that occurred often, then the transformed \
                         string will have several places where a single character is repeated \
                         multiple times in a row.";
        let bwt = BurrowsWheelerTransform::new(900_000);
        let (transformed, primary_index) = bwt.transform(original);
        let reconstructed = bwt.inverse_transform(&transformed, primary_index);
        assert_eq!(reconstructed, original);
    }

    #[test]
    fn test_bwt_should_apply() {
        let bwt = BurrowsWheelerTransform::new(900_000);

        // Text, low entropy -> apply
        assert!(bwt.should_apply(4.2, &ContentType::Text));

        // Text, high entropy -> don't apply
        assert!(!bwt.should_apply(6.1, &ContentType::Text));

        // Non-text -> don't apply
        assert!(!bwt.should_apply(3.5, &ContentType::Binary));
        assert!(!bwt.should_apply(2.0, &ContentType::Multimedia));
    }
}
