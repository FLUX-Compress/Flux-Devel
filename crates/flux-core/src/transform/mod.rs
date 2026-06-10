//! # Transform Group
//!
//! ## Purpose
//! Implements reversible data transformations designed to shape raw byte streams
//! into forms that are significantly more compressible before reaching the entropy coding stage.
//!
//! ## Order of Transformations
//! During compression, transformations are applied in the following order:
//! 1. **Delta Filter**: Runs on multimedia/float data to compute difference symbols within stride planes.
//! 2. **Byte Plane Transpose**: Concatenates identical struct-offset bytes into contiguous blocks.
//! 3. **BWT (Burrows-Wheeler Transform)**: Groups repetitive contexts in textual patterns.
//! 4. **Media-Specific Filter**: Extracts specialized channels (like RGB, RGBA, Audio) into separate planes.
//!
//! During decompression, the stack is reversed exactly:
//! `Media-Specific Filter` -> `BWT` -> `Byte Plane Transpose` -> `Delta Filter`.
//!
//! ## Content Types & Trigger Rules
//! - **Multimedia/Float**: Triggers Delta Filter and Byte Plane Transposition.
//! - **Structured Structs/Audio/WAV**: Triggers Byte Plane Transposition and specialized filters (e.g. `AudioDelta`).
//! - **Text**: Triggers optional BWT (if below the entropy threshold of 5.5).
//! - **Images (RGB/RGBA)**: Triggers `RgbSplit` or `RgbaDelta` filters.
//!
//! ## Perfect Reversibility
//! Every transformation implements a mathematically sound inverse function, ensuring
//! byte-for-byte exact recovery of the original input stream upon decompression.
//!
//! ## Transform Stack ASCII Diagram
//!
//! ```text
//!       Original Data
//!             │
//!             ▼
//!       [Delta Filter]          (if multimedia/float)
//!             │
//!             ▼
//!    [Byte Plane Transpose]     (if stride confirmed)
//!             │
//!             ▼
//!            [BWT]              (if text, optional)
//!             │
//!             ▼
//!   [Media-Specific Filter]     (if applicable)
//!             │
//!             ▼
//!     To Compression Pipeline
//! ```
//!
//! ## Archive Storage
//! A [`TransformStack`] structure is serialized and stored inside the archive header per-block.
//! This ensures the decompressor never has to guess which transformations were applied.

use serde::{Deserialize, Serialize};

pub mod bcj;
pub mod bwt;
pub mod delta;
pub mod filters;
pub mod transpose;

/// Errors returned when deserializing a TransformStack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransformDeserializationError {
    /// The input byte slice is too short.
    TooShort,
    /// The media filter type code is not supported.
    UnsupportedFilterType(u8),
}

impl std::fmt::Display for TransformDeserializationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransformDeserializationError::TooShort => write!(f, "Data too short for TransformStack"),
            TransformDeserializationError::UnsupportedFilterType(t) => {
                write!(f, "Unsupported media filter type: {}", t)
            }
        }
    }
}

impl std::error::Error for TransformDeserializationError {}

/// Media-specific specialized filters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MediaFilterType {
    /// No media-specific filter applied.
    None,
    /// Audio sample delta filter tracking inter-channel and inter-sample deltas.
    AudioDelta {
        /// Number of channels (1 = mono, 2 = stereo).
        channels: u8,
        /// Bit depth per sample (8, 16, 24, 32).
        bit_depth: u8,
    },
    /// Split 32-bit float bytes into sign+exponent and mantissa planes.
    FloatSplit,
    /// Split RGB pixels into contiguous R, G, and B planes with horizontal deltas.
    RgbSplit,
    /// Split RGBA pixels into contiguous R, G, B, and A planes with horizontal deltas.
    RgbaDelta,
    /// Split multi-channel 32-bit float bytes into separate exponent and mantissa planes.
    FloatChannelSplit {
        /// Number of float channels.
        channels: u8,
        /// Mask byte storing delta_per_plane and min_match_per_plane decisions.
        mask: u8,
    },
    /// BCJ filter for x86 executables (added in v1.4).
    BcjX86,
}

/// Stores information about which transformations were applied to a block of data,
/// allowing the decompressor to reconstruct the original stream by reversing them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransformStack {
    /// Whether the delta filter was applied.
    pub delta_applied: bool,
    /// Stride used for the delta filter (0 if not applied).
    pub delta_stride: u8,
    /// Whether byte plane transposition was applied.
    pub transpose_applied: bool,
    /// Stride used for byte plane transposition (0 if not applied).
    pub transpose_stride: u8,
    /// Whether the Burrows-Wheeler Transform was applied.
    pub bwt_applied: bool,
    /// The primary index required to reverse the Burrows-Wheeler Transform.
    pub bwt_primary_index: u32,
    /// Whether a media-specific filter was applied.
    pub media_filter_applied: bool,
    /// The type of media-specific filter applied.
    pub media_filter_type: MediaFilterType,
    /// Whether the PPM context model was applied.
    pub ppm_applied: bool,
    /// The PPM arena size class (8MB, 32MB, 64MB, or 0 if none).
    pub ppm_arena_size: usize,
}

impl Default for TransformStack {
    fn default() -> Self {
        Self {
            delta_applied: false,
            delta_stride: 0,
            transpose_applied: false,
            transpose_stride: 0,
            bwt_applied: false,
            bwt_primary_index: 0,
            media_filter_applied: false,
            media_filter_type: MediaFilterType::None,
            ppm_applied: false,
            ppm_arena_size: 0,
        }
    }
}

impl TransformStack {
    /// Serializes the `TransformStack` to a fixed-size byte layout (13 bytes).
    pub fn serialize(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(13);
        buf.push(self.delta_applied as u8);
        buf.push(self.delta_stride);
        buf.push(self.transpose_applied as u8);
        buf.push(self.transpose_stride);

        let ppm_class = match (self.ppm_applied, self.ppm_arena_size) {
            (true, 8_388_608) => 1,
            (true, 33_554_432) => 2,
            (true, 67_108_864) => 3,
            _ => 0,
        };
        buf.push(self.bwt_applied as u8 | (ppm_class << 1));

        buf.extend_from_slice(&self.bwt_primary_index.to_le_bytes());
        buf.push(self.media_filter_applied as u8);
        match self.media_filter_type {
            MediaFilterType::None => {
                buf.push(0);
                buf.push(0);
                buf.push(0);
            }
            MediaFilterType::AudioDelta {
                channels,
                bit_depth,
            } => {
                buf.push(1);
                buf.push(channels);
                buf.push(bit_depth);
            }
            MediaFilterType::FloatSplit => {
                buf.push(2);
                buf.push(0);
                buf.push(0);
            }
            MediaFilterType::RgbSplit => {
                buf.push(3);
                buf.push(0);
                buf.push(0);
            }
            MediaFilterType::RgbaDelta => {
                buf.push(4);
                buf.push(0);
                buf.push(0);
            }
            MediaFilterType::FloatChannelSplit { channels, mask } => {
                buf.push(5);
                buf.push(channels);
                buf.push(mask);
            }
            MediaFilterType::BcjX86 => {
                buf.push(6);
                buf.push(0);
                buf.push(0);
            }
        }
        buf
    }

    /// Deserializes a `TransformStack` from a byte slice.
    pub fn deserialize(data: &[u8]) -> Result<(Self, &[u8]), TransformDeserializationError> {
        if data.len() < 13 {
            return Err(TransformDeserializationError::TooShort);
        }
        let delta_applied = data[0] != 0;
        let delta_stride = data[1];
        let transpose_applied = data[2] != 0;
        let transpose_stride = data[3];

        let bwt_byte = data[4];
        let bwt_applied = (bwt_byte & 1) != 0;
        let ppm_class = (bwt_byte >> 1) & 3;
        let (ppm_applied, ppm_arena_size) = match ppm_class {
            1 => (true, 8_388_608),
            2 => (true, 33_554_432),
            3 => (true, 67_108_864),
            _ => (false, 0),
        };

        let bwt_primary_index = u32::from_le_bytes(data[5..9].try_into().unwrap());
        let media_filter_applied = data[9] != 0;
        let media_type = data[10];
        let param1 = data[11];
        let param2 = data[12];

        let media_filter_type = match media_type {
            0 => MediaFilterType::None,
            1 => MediaFilterType::AudioDelta {
                channels: param1,
                bit_depth: param2,
            },
            2 => MediaFilterType::FloatSplit,
            3 => MediaFilterType::RgbSplit,
            4 => MediaFilterType::RgbaDelta,
            5 => MediaFilterType::FloatChannelSplit {
                channels: param1,
                mask: param2,
            },
            // 6 is BCJ x86 filter (introduced in v1.4).
            // Newer/older readers handling is additive: if an archive does not use BCJ,
            // older readers (v1.3) can still read it cleanly. If BCJ is used (media_type == 6),
            // older readers will fail with an error mapped to ErrUnsupportedFormat.
            6 => MediaFilterType::BcjX86,
            _ => return Err(TransformDeserializationError::UnsupportedFilterType(media_type)),
        };

        Ok((
            Self {
                delta_applied,
                delta_stride,
                transpose_applied,
                transpose_stride,
                bwt_applied,
                bwt_primary_index,
                media_filter_applied,
                media_filter_type,
                ppm_applied,
                ppm_arena_size,
            },
            &data[13..],
        ))
    }


    /// Applies the transform stack in compression order:
    /// `Delta Filter` -> `Byte Plane Transpose` -> `BWT` -> `Media Filter`.
    pub fn apply(
        &mut self,
        mut data: Vec<u8>,
        entropy: f32,
        content_type: &crate::threads::signals::ContentType,
        stride: Option<u8>,
        bwt_enabled: bool,
    ) -> Vec<u8> {
        // 1. Delta Filter
        if let Some(s) = stride {
            if s > 1 {
                let mut delta_enc = delta::DeltaEncoder::new(s);
                data = delta_enc.encode(&data);
                self.delta_applied = true;
                self.delta_stride = s;
            }
        }

        // 2. Byte Plane Transpose
        if let Some(s) = stride {
            if s > 1 {
                let transposer = transpose::ByteTransposer::new(s);
                data = transposer.transpose(&data);
                self.transpose_applied = true;
                self.transpose_stride = s;
            }
        }

        // 3. BWT
        let bwt = bwt::BurrowsWheelerTransform::new(1024 * 1024);
        if bwt_enabled && bwt.should_apply(entropy, content_type) {
            let (transformed, primary_idx) = bwt.transform(&data);
            data = transformed;
            self.bwt_applied = true;
            self.bwt_primary_index = primary_idx;
        }

        // 4. Media-Specific Filter (if already configured)
        if self.media_filter_applied {
            let (transformed, updated_filter) =
                filters::apply_filter(self.media_filter_type, &data);
            data = transformed;
            self.media_filter_type = updated_filter;
        }

        data
    }

    /// Reverses the transform stack in decompression order:
    /// `Media Filter` -> `BWT` -> `Byte Plane Transpose` -> `Delta Filter`.
    pub fn reverse(&self, mut data: Vec<u8>) -> Vec<u8> {
        // 1. Reverse Media-Specific Filter
        if self.media_filter_applied {
            data = filters::reverse_filter(self.media_filter_type, &data);
        }

        // 2. Reverse BWT
        if self.bwt_applied {
            let bwt = bwt::BurrowsWheelerTransform::new(1024 * 1024);
            data = bwt.inverse_transform(&data, self.bwt_primary_index);
        }

        // 3. Reverse Transpose
        if self.transpose_applied {
            let transposer = transpose::ByteTransposer::new(self.transpose_stride);
            data = transposer.inverse_transpose(&data);
        }

        // 4. Reverse Delta
        if self.delta_applied {
            let mut delta_enc = delta::DeltaEncoder::new(self.delta_stride);
            data = delta_enc.decode(&data);
        }

        data
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_transform_stack_bcj_roundtrip() {
        let ts = TransformStack {
            delta_applied: true,
            delta_stride: 4,
            transpose_applied: true,
            transpose_stride: 8,
            bwt_applied: false,
            bwt_primary_index: 0,
            media_filter_applied: true,
            media_filter_type: MediaFilterType::BcjX86,
            ppm_applied: false,
            ppm_arena_size: 0,
        };

        let serialized = ts.serialize();
        assert_eq!(serialized.len(), 13);
        assert_eq!(serialized[10], 6); // media filter type index should be 6
        assert_eq!(serialized[11], 0);
        assert_eq!(serialized[12], 0);

        let (deserialized, remaining) = TransformStack::deserialize(&serialized).unwrap();
        assert_eq!(deserialized, ts);
        assert!(remaining.is_empty());
    }

    #[test]
    fn test_transform_stack_deserialize_errors() {
        // Too short
        let data_short = vec![0u8; 12];
        let res_short = TransformStack::deserialize(&data_short);
        assert_eq!(res_short, Err(TransformDeserializationError::TooShort));

        // Unsupported media filter type (e.g. 7)
        let mut data_unsupported = vec![0u8; 13];
        data_unsupported[9] = 1; // media_filter_applied = true
        data_unsupported[10] = 7; // type code 7
        let res_unsupported = TransformStack::deserialize(&data_unsupported);
        assert_eq!(
            res_unsupported,
            Err(TransformDeserializationError::UnsupportedFilterType(7))
        );
    }
}

