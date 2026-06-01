//! # Byte Plane Transposition
//!
//! Byte plane transposition reorganizes structured, multi-dimensional stream records
//! into contiguous stride-aligned planes. This groups bytes that share the same structural
//! "role" (e.g. RGB color channels, floating-point sign/exponents, or audio samples) adjacent
//! to each other, creating highly uniform sequences that are significantly easier to compress.
//!
//! ## Worked Example (Stride = 4, 3 Elements)
//!
//! Consider a 12-byte block with a structured stride of 4:
//! - Input: `[A0, A1, A2, A3, B0, B1, B2, B3, C0, C1, C2, C3]`
//!
//! After transposition with stride = 4, the bytes are grouped into 4 contiguous planes:
//! - **Plane 0**: `[A0, B0, C0]`
//! - **Plane 1**: `[A1, B1, C1]`
//! - **Plane 2**: `[A2, B2, C2]`
//! - **Plane 3**: `[A3, B3, C3]`
//!
//! Concatenated Output:
//! - `[A0, B0, C0, A1, B1, C1, A2, B2, C2, A3, B3, C3]`
//!
//! All byte-0 elements (e.g., `A0`, `B0`, `C0`) are now adjacent. If these are float exponents
//! or similar slow-changing header fields, they correlate strongly, reducing local entropy.

/// Stride-based byte transposer.
#[derive(Debug, Clone, Copy)]
pub struct ByteTransposer {
    /// The stride length (in bytes) used to partition the planes.
    pub stride: u8,
}

impl ByteTransposer {
    /// Creates a new `ByteTransposer` with a given stride.
    pub fn new(stride: u8) -> Self {
        Self { stride }
    }

    /// Transposes the byte slice into stride-based planes.
    ///
    /// For a stride $S$ and input of length $N$:
    ///
    /// $$\text{Output}[\text{plane} \times \lceil N/S \rceil + \text{position}] = \text{Input}[\text{position} \times S + \text{plane}]$$
    ///
    /// where $\text{plane} \in [0, S-1]$ and $\text{position}$ tracks the offset in each plane.
    /// Supports lengths that are not multiples of the stride.
    pub fn transpose(&self, data: &[u8]) -> Vec<u8> {
        let s = self.stride as usize;
        if s <= 1 || data.len() <= s {
            return data.to_vec();
        }
        let len = data.len();
        let mut transposed = vec![0; len];
        
        let num_blocks = len / s;
        let remainder = len % s;

        let mut plane_offsets = vec![0; s];
        let mut current_offset = 0;
        for (p, offset) in plane_offsets.iter_mut().enumerate() {
            *offset = current_offset;
            let plane_len = num_blocks + if p < remainder { 1 } else { 0 };
            current_offset += plane_len;
        }

        let mut plane_counters = vec![0; s];

        for (n, &byte) in data.iter().enumerate() {
            let plane = n % s;
            let dest_idx = plane_offsets[plane] + plane_counters[plane];
            transposed[dest_idx] = byte;
            plane_counters[plane] += 1;
        }

        transposed
    }

    /// Reverses the byte plane transposition exactly.
    ///
    /// Must satisfy `inverse_transpose(transpose(data)) == data`.
    pub fn inverse_transpose(&self, data: &[u8]) -> Vec<u8> {
        let s = self.stride as usize;
        if s <= 1 || data.len() <= s {
            return data.to_vec();
        }
        let len = data.len();
        let mut untransposed = vec![0; len];
        
        let num_blocks = len / s;
        let remainder = len % s;

        let mut plane_offsets = vec![0; s];
        let mut current_offset = 0;
        for (p, offset) in plane_offsets.iter_mut().enumerate() {
            *offset = current_offset;
            let plane_len = num_blocks + if p < remainder { 1 } else { 0 };
            current_offset += plane_len;
        }

        let mut plane_counters = vec![0; s];

        for (n, slot) in untransposed.iter_mut().enumerate() {
            let plane = n % s;
            let src_idx = plane_offsets[plane] + plane_counters[plane];
            *slot = data[src_idx];
            plane_counters[plane] += 1;
        }

        untransposed
    }

    /// In-place version of transposition.
    pub fn transpose_inplace(&self, data: &mut Vec<u8>) {
        *data = self.transpose(data);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_transpose_stride2() {
        let original = vec![1, 2, 10, 20, 100, 200];
        let transposer = ByteTransposer::new(2);
        let transposed = transposer.transpose(&original);
        assert_eq!(transposed, vec![1, 10, 100, 2, 20, 200]);
    }

    #[test]
    fn test_transpose_stride4() {
        // Worked example from documentation
        let original = vec![
            10, 11, 12, 13, // A
            20, 21, 22, 23, // B
            30, 31, 32, 33, // C
        ];
        let transposer = ByteTransposer::new(4);
        let transposed = transposer.transpose(&original);
        
        // Expected concatenated planes:
        // P0: [10, 20, 30]
        // P1: [11, 21, 31]
        // P2: [12, 22, 32]
        // P3: [13, 23, 33]
        let expected = vec![
            10, 20, 30,
            11, 21, 31,
            12, 22, 32,
            13, 23, 33,
        ];
        assert_eq!(transposed, expected);
    }

    #[test]
    fn test_inverse_transpose_roundtrip_stride2() {
        let original = vec![1, 2, 3, 4, 5, 6, 7, 8];
        let transposer = ByteTransposer::new(2);
        let transposed = transposer.transpose(&original);
        let untransposed = transposer.inverse_transpose(&transposed);
        assert_eq!(untransposed, original);
    }

    #[test]
    fn test_inverse_transpose_roundtrip_stride4() {
        let original = vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
        let transposer = ByteTransposer::new(4);
        let transposed = transposer.transpose(&original);
        let untransposed = transposer.inverse_transpose(&transposed);
        assert_eq!(untransposed, original);
    }

    #[test]
    fn test_inverse_transpose_roundtrip_stride8() {
        let original = vec![
            1, 2, 3, 4, 5, 6, 7, 8,
            9, 10, 11, 12, 13, 14, 15, 16,
        ];
        let transposer = ByteTransposer::new(8);
        let transposed = transposer.transpose(&original);
        let untransposed = transposer.inverse_transpose(&transposed);
        assert_eq!(untransposed, original);
    }

    #[test]
    fn test_transpose_non_multiple_length() {
        let original = vec![1, 2, 3, 4, 5, 6, 7];
        let transposer = ByteTransposer::new(3);
        let transposed = transposer.transpose(&original);
        // Plane lengths:
        // P0 (rem: 7%3=1 -> len 3): indices 0, 3, 6 -> [1, 4, 7]
        // P1 (len 2): indices 1, 4 -> [2, 5]
        // P2 (len 2): indices 2, 5 -> [3, 6]
        // concatenated: [1, 4, 7, 2, 5, 3, 6]
        assert_eq!(transposed, vec![1, 4, 7, 2, 5, 3, 6]);

        let untransposed = transposer.inverse_transpose(&transposed);
        assert_eq!(untransposed, original);
    }
}
