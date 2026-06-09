//! # Delta Coding Transformation
//!
//! Delta coding replaces each value with the difference from the previous value.
//! For a given byte sequence $X$:
//!
//! $$D_i = X_i - X_{i-1} \pmod{256}$$
//!
//! When the input data has a structured multi-dimensional pattern (e.g. multi-channel audio
//! or floating-point arrays), we can apply delta coding to each stride-plane independently:
//!
//! $$D_i = X_i - X_{i-S} \pmod{256}$$
//!
//! where $S$ is the stride. The encoder maintains a carry state of length $S$ containing the
//! previous values across block boundaries. This allows continuous stream delta coding
//! even when split into discrete chunks.
//!
//! ## Concrete Example (Stride = 4)
//!
//! Consider a sequence of 3 IEEE 754 32-bit float values (represented as 12 bytes total):
//! - `1.0f` -> `[0x00, 0x00, 0x80, 0x3F]`
//! - `2.0f` -> `[0x00, 0x00, 0x00, 0x40]`
//! - `3.0f` -> `[0x00, 0x00, 0x40, 0x40]`
//!
//! Under stride = 4, the bytes are grouped into 4 distinct planes:
//! - **Plane 0 (bits 0-7, lowest mantissa)**: `[0x00, 0x00, 0x00]`
//! - **Plane 1 (bits 8-15, middle mantissa)**: `[0x00, 0x00, 0x00]`
//! - **Plane 2 (bits 16-23, high mantissa/low exponent)**: `[0x80, 0x00, 0x40]`
//! - **Plane 3 (bits 24-31, exponent/sign)**: `[0x3F, 0x40, 0x40]`
//!
//! After stride-aware delta coding, the values become differences within each plane independently:
//! - **Plane 0**: `[0x00, 0x00, 0x00]` (diffs: `0x00-0=0x00`, `0x00-0x00=0x00`, `0x00-0x00=0x00`) -> all zeros!
//! - **Plane 1**: `[0x00, 0x00, 0x00]` (diffs: `0x00-0=0x00`, `0x00-0x00=0x00`, `0x00-0x00=0x00`) -> all zeros!
//! - **Plane 2**: `[0x80, 0x80, 0x40]` (diffs: `0x80-0=0x80`, `0x00-0x80=0x80`, `0x40-0x00=0x40`) -> small deltas!
//! - **Plane 3**: `[0x3F, 0x01, 0x00]` (diffs: `0x3F-0=0x3F`, `0x40-0x3F=0x01`, `0x40-0x40=0x00`) -> small deltas!
//!
//! Because the float values change smoothly, the exponent and high-order mantissa bits
//! have very small variations. In the delta-coded output, these become runs of zero bytes and
//! highly clustered near-zero integers, which drastically reduces local entropy.

/// Stride-aware stateful delta encoder.
#[derive(Debug, Clone)]
pub struct DeltaEncoder {
    /// The stride (number of independent byte planes) being coded.
    pub stride: u8,
    /// Last values observed for each plane to support continuous coding across blocks.
    pub last_values: Vec<u8>,
}

impl DeltaEncoder {
    /// Creates a new stateful `DeltaEncoder` for a given stride.
    pub fn new(stride: u8) -> Self {
        let s = if stride == 0 { 1 } else { stride };
        Self {
            stride: s,
            last_values: vec![0; s as usize],
        }
    }

    /// Encodes a block of data with stride-aware delta coding.
    ///
    /// Values are coded against the previous byte in the same plane.
    /// The state carries across calls to support stream fragmentation.
    pub fn encode(&mut self, data: &[u8]) -> Vec<u8> {
        let mut result = Vec::with_capacity(data.len());
        let s = self.stride as usize;
        for (i, &val) in data.iter().enumerate() {
            let plane_idx = i % s;
            let prev = self.last_values[plane_idx];
            let diff = val.wrapping_sub(prev);
            self.last_values[plane_idx] = val;
            result.push(diff);
        }
        result
    }

    /// Decodes a block of data, reversing the stride-aware delta coding exactly.
    ///
    /// Must be the perfect inverse of [`Self::encode`].
    pub fn decode(&mut self, data: &[u8]) -> Vec<u8> {
        let mut result = Vec::with_capacity(data.len());
        let s = self.stride as usize;
        for (i, &diff) in data.iter().enumerate() {
            let plane_idx = i % s;
            let prev = self.last_values[plane_idx];
            let val = diff.wrapping_add(prev);
            self.last_values[plane_idx] = val;
            result.push(val);
        }
        result
    }

    /// Resets the encoder carry state to zero.
    pub fn reset(&mut self) {
        for val in self.last_values.iter_mut() {
            *val = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_delta_encode_decode_stride1() {
        let original = vec![10, 12, 15, 20, 25, 24, 20, 100, 105];
        let mut encoder = DeltaEncoder::new(1);
        let encoded = encoder.encode(&original);

        let mut decoder = DeltaEncoder::new(1);
        let decoded = decoder.decode(&encoded);

        assert_eq!(decoded, original);
    }

    #[test]
    fn test_delta_encode_decode_stride4() {
        let original = vec![
            10, 20, 30, 40, 12, 22, 32, 42, 15, 25, 35, 45, 20, 30, 40, 50,
        ];
        let mut encoder = DeltaEncoder::new(4);
        let encoded = encoder.encode(&original);

        let mut decoder = DeltaEncoder::new(4);
        let decoded = decoder.decode(&encoded);

        assert_eq!(decoded, original);
    }

    #[test]
    fn test_delta_carries_across_blocks() {
        let block1 = vec![10, 20, 30];
        let block2 = vec![15, 25, 35];

        let mut encoder = DeltaEncoder::new(1);
        let enc1 = encoder.encode(&block1);
        let enc2 = encoder.encode(&block2);

        // Block 1 should encode against 0 seed:
        // [10, 10, 10]
        assert_eq!(enc1, vec![10, 10, 10]);
        // Block 2 should carry the last value of block1 (30):
        // [15-30, 25-15, 35-25] -> [241, 10, 10]
        assert_eq!(enc2, vec![15u8.wrapping_sub(30), 10, 10]);

        // Decode must match exactly with carried state
        let mut decoder = DeltaEncoder::new(1);
        let dec1 = decoder.decode(&enc1);
        let dec2 = decoder.decode(&enc2);

        assert_eq!(dec1, block1);
        assert_eq!(dec2, block2);
    }

    #[test]
    fn test_delta_concrete_float_example() {
        // 1.0f, 2.0f, 3.0f in IEEE 754 little endian
        let float_bytes = vec![
            0x00, 0x00, 0x80, 0x3F, // 1.0f
            0x00, 0x00, 0x00, 0x40, // 2.0f
            0x00, 0x00, 0x40, 0x40, // 3.0f
        ];

        let mut encoder = DeltaEncoder::new(4);
        let encoded = encoder.encode(&float_bytes);

        // Verification of individual planes:
        // Index mapping: plane = i % 4
        // Plane 0: indices 0, 4, 8 -> original [0x00, 0x00, 0x00] -> encoded [0x00, 0x00, 0x00]
        // Plane 1: indices 1, 5, 9 -> original [0x00, 0x00, 0x00] -> encoded [0x00, 0x00, 0x00]
        // Plane 2: indices 2, 6, 10 -> original [0x80, 0x00, 0x40] -> encoded [0x80, 0x80, 0x40]
        // Plane 3: indices 3, 7, 11 -> original [0x3F, 0x40, 0x40] -> encoded [0x3F, 0x01, 0x00]

        // Concatenated interleaved output:
        // E0: [0x00, 0x00, 0x80, 0x3F]
        // E1: [0x00, 0x00, 0x80, 0x01]
        // E2: [0x00, 0x00, 0x40, 0x00]
        let expected = vec![
            0x00, 0x00, 0x80, 0x3F, 0x00, 0x00, 0x80, 0x01, 0x00, 0x00, 0x40, 0x00,
        ];

        assert_eq!(encoded, expected);

        // Verify roundtrip
        let mut decoder = DeltaEncoder::new(4);
        let decoded = decoder.decode(&encoded);
        assert_eq!(decoded, float_bytes);
    }
}
