//! # Media-Specific specialized filters
//!
//! Exposes transformations tailored to structured media layouts to drastically reduce entropy:
//! - **AudioDeltaFilter**: Applies inter-channel and inter-sample delta coding. Because audio waveforms
//!   change smoothly, difference values group tightly around zero.
//! - **FloatSplitFilter**: Segregates 32-bit float bitfields. Float exponent bytes change slowly compared
//!   to rapidly varying mantissas. Splitting them exposes highly compressible uniform byte runs.
//! - **RgbSplitFilter**: Segregates R, G, B channels and performs horizontal delta coding. Adjacent pixels
//!   often carry near-identical color values.
//! - **RgbaDeltaFilter**: Segregates R, G, B, A channels and performs delta coding.

use crate::transform::MediaFilterType;

/// Trait defining the interface for reversible media-specific prep filters.
pub trait MediaFilter {
    /// Applies the transformation to the data slice, returning the transformed vector.
    fn apply(&self, data: &[u8]) -> Vec<u8>;
    /// Reverses the transformation on the data slice, recovering the original bytes.
    fn reverse(&self, data: &[u8]) -> Vec<u8>;
    /// Returns the corresponding enum type descriptor.
    fn filter_type(&self) -> MediaFilterType;
}

// =========================================================================
// 1. Audio Delta Filter
// =========================================================================

/// Specialized delta coder for PCM audio streams.
#[derive(Debug, Clone, Copy)]
pub struct AudioDeltaFilter {
    /// Number of channels (1 = mono, 2 = stereo).
    pub channels: u8,
    /// Bit depth per sample (8, 16, 24, 32).
    pub bit_depth: u8,
}

impl AudioDeltaFilter {
    /// Creates a new `AudioDeltaFilter`.
    pub fn new(channels: u8, bit_depth: u8) -> Self {
        Self { channels, bit_depth }
    }
}

impl MediaFilter for AudioDeltaFilter {
    fn filter_type(&self) -> MediaFilterType {
        MediaFilterType::AudioDelta {
            channels: self.channels,
            bit_depth: self.bit_depth,
        }
    }

    fn apply(&self, data: &[u8]) -> Vec<u8> {
        let bytes_per_sample = (self.bit_depth / 8) as usize;
        let channels = self.channels as usize;
        let frame_size = channels * bytes_per_sample;
        if frame_size == 0 || data.len() < frame_size {
            return data.to_vec();
        }

        let num_frames = data.len() / frame_size;
        let mut output = data.to_vec();

        let read_sample = |buf: &[u8], frame_idx: usize, ch_idx: usize| -> u32 {
            let idx = frame_idx * frame_size + ch_idx * bytes_per_sample;
            match bytes_per_sample {
                1 => buf[idx] as u32,
                2 => u16::from_le_bytes([buf[idx], buf[idx + 1]]) as u32,
                3 => (buf[idx] as u32) | ((buf[idx + 1] as u32) << 8) | ((buf[idx + 2] as u32) << 16),
                _ => u32::from_le_bytes([buf[idx], buf[idx + 1], buf[idx + 2], buf[idx + 3]]),
            }
        };

        let write_sample = |buf: &mut [u8], frame_idx: usize, ch_idx: usize, val: u32| {
            let idx = frame_idx * frame_size + ch_idx * bytes_per_sample;
            match bytes_per_sample {
                1 => buf[idx] = val as u8,
                2 => {
                    let bytes = (val as u16).to_le_bytes();
                    buf[idx] = bytes[0];
                    buf[idx + 1] = bytes[1];
                }
                3 => {
                    buf[idx] = val as u8;
                    buf[idx + 1] = (val >> 8) as u8;
                    buf[idx + 2] = (val >> 16) as u8;
                }
                _ => {
                    let bytes = val.to_le_bytes();
                    buf[idx] = bytes[0];
                    buf[idx + 1] = bytes[1];
                    buf[idx + 2] = bytes[2];
                    buf[idx + 3] = bytes[3];
                }
            }
        };

        let mut prev_c0 = 0u32;
        let mask = match bytes_per_sample {
            1 => 0xFF,
            2 => 0xFFFF,
            3 => 0xFFFFFF,
            _ => 0xFFFFFFFF,
        };

        for f in 0..num_frames {
            let c0 = read_sample(data, f, 0);
            let diff_c0 = c0.wrapping_sub(prev_c0) & mask;
            write_sample(&mut output, f, 0, diff_c0);
            prev_c0 = c0;

            for ch in 1..channels {
                let c_ch = read_sample(data, f, ch);
                let diff_ch = c_ch.wrapping_sub(c0) & mask;
                write_sample(&mut output, f, ch, diff_ch);
            }
        }

        output
    }

    fn reverse(&self, data: &[u8]) -> Vec<u8> {
        let bytes_per_sample = (self.bit_depth / 8) as usize;
        let channels = self.channels as usize;
        let frame_size = channels * bytes_per_sample;
        if frame_size == 0 || data.len() < frame_size {
            return data.to_vec();
        }

        let num_frames = data.len() / frame_size;
        let mut output = data.to_vec();

        let read_sample = |buf: &[u8], frame_idx: usize, ch_idx: usize| -> u32 {
            let idx = frame_idx * frame_size + ch_idx * bytes_per_sample;
            match bytes_per_sample {
                1 => buf[idx] as u32,
                2 => u16::from_le_bytes([buf[idx], buf[idx + 1]]) as u32,
                3 => (buf[idx] as u32) | ((buf[idx + 1] as u32) << 8) | ((buf[idx + 2] as u32) << 16),
                _ => u32::from_le_bytes([buf[idx], buf[idx + 1], buf[idx + 2], buf[idx + 3]]),
            }
        };

        let write_sample = |buf: &mut [u8], frame_idx: usize, ch_idx: usize, val: u32| {
            let idx = frame_idx * frame_size + ch_idx * bytes_per_sample;
            match bytes_per_sample {
                1 => buf[idx] = val as u8,
                2 => {
                    let bytes = (val as u16).to_le_bytes();
                    buf[idx] = bytes[0];
                    buf[idx + 1] = bytes[1];
                }
                3 => {
                    buf[idx] = val as u8;
                    buf[idx + 1] = (val >> 8) as u8;
                    buf[idx + 2] = (val >> 16) as u8;
                }
                _ => {
                    let bytes = val.to_le_bytes();
                    buf[idx] = bytes[0];
                    buf[idx + 1] = bytes[1];
                    buf[idx + 2] = bytes[2];
                    buf[idx + 3] = bytes[3];
                }
            }
        };

        let mut prev_c0 = 0u32;
        let mask = match bytes_per_sample {
            1 => 0xFF,
            2 => 0xFFFF,
            3 => 0xFFFFFF,
            _ => 0xFFFFFFFF,
        };

        for f in 0..num_frames {
            let diff_c0 = read_sample(data, f, 0);
            let c0 = diff_c0.wrapping_add(prev_c0) & mask;
            write_sample(&mut output, f, 0, c0);
            prev_c0 = c0;

            for ch in 1..channels {
                let diff_ch = read_sample(data, f, ch);
                let c_ch = diff_ch.wrapping_add(c0) & mask;
                write_sample(&mut output, f, ch, c_ch);
            }
        }

        output
    }
}

// =========================================================================
// 2. Float Split Filter
// =========================================================================

/// Splits 32-bit float buffers into sign/exponent and mantissa planes.
#[derive(Debug, Clone, Copy)]
pub struct FloatSplitFilter;

impl MediaFilter for FloatSplitFilter {
    fn filter_type(&self) -> MediaFilterType {
        MediaFilterType::FloatSplit
    }

    fn apply(&self, data: &[u8]) -> Vec<u8> {
        let n = data.len() / 4;
        if n == 0 {
            return data.to_vec();
        }
        let mut output = vec![0; data.len()];

        let p0_start = 0;
        let p1_start = n;
        let p2_start = 3 * n;

        for i in 0..n {
            let idx = i * 4;
            let val = u32::from_le_bytes([data[idx], data[idx + 1], data[idx + 2], data[idx + 3]]);
            let sign = (val >> 31) & 1;
            let exponent = (val >> 23) & 0xFF;
            let mantissa = val & 0x7FFFFF;

            let mantissa_high = (mantissa >> 7) as u16;
            let mantissa_low = (mantissa & 0x7F) as u8;

            output[p0_start + i] = exponent as u8;
            
            let m_high_bytes = mantissa_high.to_le_bytes();
            output[p1_start + 2 * i] = m_high_bytes[0];
            output[p1_start + 2 * i + 1] = m_high_bytes[1];
            
            output[p2_start + i] = mantissa_low | ((sign as u8) << 7);
        }

        if !data.len().is_multiple_of(4) {
            let remainder_start = n * 4;
            output[remainder_start..].copy_from_slice(&data[remainder_start..]);
        }

        output
    }

    fn reverse(&self, data: &[u8]) -> Vec<u8> {
        let n = data.len() / 4;
        if n == 0 {
            return data.to_vec();
        }
        let mut output = vec![0; data.len()];

        let p0_start = 0;
        let p1_start = n;
        let p2_start = 3 * n;

        for i in 0..n {
            let exponent = data[p0_start + i] as u32;
            
            let m_high_0 = data[p1_start + 2 * i];
            let m_high_1 = data[p1_start + 2 * i + 1];
            let mantissa_high = u16::from_le_bytes([m_high_0, m_high_1]) as u32;
            
            let p2_val = data[p2_start + i];
            let mantissa_low = (p2_val & 0x7F) as u32;
            let sign = ((p2_val >> 7) & 1) as u32;

            let mantissa = (mantissa_high << 7) | mantissa_low;
            let val = (sign << 31) | (exponent << 23) | mantissa;

            let val_bytes = val.to_le_bytes();
            let idx = i * 4;
            output[idx] = val_bytes[0];
            output[idx + 1] = val_bytes[1];
            output[idx + 2] = val_bytes[2];
            output[idx + 3] = val_bytes[3];
        }

        if !data.len().is_multiple_of(4) {
            let remainder_start = n * 4;
            output[remainder_start..].copy_from_slice(&data[remainder_start..]);
        }

        output
    }
}

// =========================================================================
// 2b. Float Channel Split Filter
// =========================================================================

fn compute_entropy(data: &[u8]) -> f32 {
    if data.is_empty() {
        return 0.0;
    }
    let mut counts = [0usize; 256];
    for &b in data {
        counts[b as usize] += 1;
    }
    let len = data.len() as f32;
    let mut entropy = 0.0;
    for &c in &counts {
        if c > 0 {
            let p = c as f32 / len;
            entropy -= p * p.log2();
        }
    }
    entropy
}

/// Splits multi-channel 32-bit float buffers into separate exponent and mantissa planes.
#[derive(Debug, Clone)]
pub struct FloatChannelSplitFilter {
    pub channels: u8,
    pub mask: std::cell::Cell<u8>,
}

impl FloatChannelSplitFilter {
    pub fn new(channels: u8, mask: u8) -> Self {
        Self {
            channels,
            mask: std::cell::Cell::new(mask),
        }
    }
}

impl MediaFilter for FloatChannelSplitFilter {
    fn filter_type(&self) -> MediaFilterType {
        MediaFilterType::FloatChannelSplit {
            channels: self.channels,
            mask: self.mask.get(),
        }
    }

    #[allow(clippy::needless_range_loop)]
    fn apply(&self, data: &[u8]) -> Vec<u8> {
        let channels = self.channels as usize;
        let n = data.len() / (4 * channels);
        if n == 0 {
            return data.to_vec();
        }

        let plane_size = n * channels;
        let mut output = vec![0u8; data.len()];

        // De-interleave and convert to u32 (unmodified/raw)
        let mut channel_u32s = vec![vec![0u32; n]; channels];
        for i in 0..n {
            for ch in 0..channels {
                let idx = (i * channels + ch) * 4;
                let val = u32::from_le_bytes([
                    data[idx],
                    data[idx + 1],
                    data[idx + 2],
                    data[idx + 3],
                ]);
                channel_u32s[ch][i] = val;
            }
        }

        // Split into global byte planes
        let mut planes = vec![vec![0u8; plane_size]; 4];
        for ch in 0..channels {
            for i in 0..n {
                let val = channel_u32s[ch][i];
                for byte_idx in 0..4 {
                    planes[byte_idx][ch * n + i] = (val >> (byte_idx * 8)) as u8;
                }
            }
        }

        let mut computed_mask = 0u8;

        // Decide delta coding per global byte-plane
        for plane_idx in 0..4 {
            let plane_bytes = &planes[plane_idx];
            let entropy_raw = compute_entropy(plane_bytes);

            // Compute delta-1 on this plane
            let mut delta_plane = vec![0u8; plane_size];
            if plane_size > 0 {
                delta_plane[0] = plane_bytes[0];
                for i in 1..plane_size {
                    delta_plane[i] = plane_bytes[i].wrapping_sub(plane_bytes[i - 1]);
                }
            }
            let entropy_delta = compute_entropy(&delta_plane);

            let delta_applied = entropy_delta < entropy_raw;
            let final_plane_bytes = if delta_applied {
                computed_mask |= 1 << plane_idx; // Bits 0..3: true = delta applied
                &delta_plane
            } else {
                plane_bytes
            };

            // Identify low-entropy planes for min_match = 3
            let final_ent = compute_entropy(final_plane_bytes);
            if final_ent < 5.0 {
                computed_mask |= 1 << (plane_idx + 4); // Bits 4..7: true = low-entropy
            }

            let out_start = (3 - plane_idx) * plane_size;
            output[out_start..out_start + plane_size].copy_from_slice(final_plane_bytes);
        }

        self.mask.set(computed_mask);
        crate::flux_debug!("[FloatChannelSplit apply] computed mask = {:08b}, plane_size = {}, total_len = {}", computed_mask, plane_size, data.len());

        // Append remainder bytes
        let processed_len = n * 4 * channels;
        if data.len() > processed_len {
            output[processed_len..].copy_from_slice(&data[processed_len..]);
        }

        output
    }

    #[allow(clippy::needless_range_loop)]
    fn reverse(&self, data: &[u8]) -> Vec<u8> {
        let channels = self.channels as usize;
        let n = data.len() / (4 * channels);
        if n == 0 {
            return data.to_vec();
        }

        let plane_size = n * channels;
        let mut output = vec![0u8; data.len()];
        let mask = self.mask.get();
        crate::flux_debug!("[FloatChannelSplit reverse] using mask = {:08b}, plane_size = {}, total_len = {}", mask, plane_size, data.len());

        // Reconstruct planes from data
        let mut planes = vec![vec![0u8; plane_size]; 4];
        for plane_idx in 0..4 {
            let out_start = (3 - plane_idx) * plane_size;
            planes[plane_idx].copy_from_slice(&data[out_start..out_start + plane_size]);
        }

        // Reverse delta per plane if applied
        for plane_idx in 0..4 {
            let delta_applied = (mask & (1 << plane_idx)) != 0;
            if delta_applied {
                let delta_plane = &planes[plane_idx];
                let mut plane = vec![0u8; plane_size];
                if plane_size > 0 {
                    plane[0] = delta_plane[0];
                    for i in 1..plane_size {
                        plane[i] = delta_plane[i].wrapping_add(plane[i - 1]);
                    }
                }
                planes[plane_idx] = plane;
            }
        }

        // Reconstruct channel u32s from planes
        let mut channel_u32s = vec![vec![0u32; n]; channels];
        for ch in 0..channels {
            for i in 0..n {
                let mut val = 0u32;
                for byte_idx in 0..4 {
                    let byte = planes[byte_idx][ch * n + i];
                    val |= (byte as u32) << (byte_idx * 8);
                }
                channel_u32s[ch][i] = val;
            }
        }

        // Interleave back
        for i in 0..n {
            for ch in 0..channels {
                let val = channel_u32s[ch][i];
                let val_bytes = val.to_le_bytes();
                let idx = (i * channels + ch) * 4;
                output[idx] = val_bytes[0];
                output[idx + 1] = val_bytes[1];
                output[idx + 2] = val_bytes[2];
                output[idx + 3] = val_bytes[3];
            }
        }

        // Append remainder bytes
        let processed_len = n * 4 * channels;
        if data.len() > processed_len {
            output[processed_len..].copy_from_slice(&data[processed_len..]);
        }

        output
    }
}

// =========================================================================
// 3. RGB Split Filter
// =========================================================================

/// Splits RGB pixel data into contiguous R, G, B planes with horizontal deltas.
#[derive(Debug, Clone, Copy)]
pub struct RgbSplitFilter;

impl MediaFilter for RgbSplitFilter {
    fn filter_type(&self) -> MediaFilterType {
        MediaFilterType::RgbSplit
    }

    fn apply(&self, data: &[u8]) -> Vec<u8> {
        let n = data.len() / 3;
        if n == 0 {
            return data.to_vec();
        }
        let mut output = vec![0; data.len()];

        let r_start = 0;
        let g_start = n;
        let b_start = 2 * n;

        // Separate
        for i in 0..n {
            output[r_start + i] = data[i * 3];
            output[g_start + i] = data[i * 3 + 1];
            output[b_start + i] = data[i * 3 + 2];
        }

        // Apply 1D delta per plane
        let apply_plane_delta = |buf: &mut [u8], start: usize, len: usize| {
            if len <= 1 {
                return;
            }
            let mut prev = buf[start];
            for idx in 1..len {
                let curr = buf[start + idx];
                buf[start + idx] = curr.wrapping_sub(prev);
                prev = curr;
            }
        };

        apply_plane_delta(&mut output, r_start, n);
        apply_plane_delta(&mut output, g_start, n);
        apply_plane_delta(&mut output, b_start, n);

        if !data.len().is_multiple_of(3) {
            let remainder_start = n * 3;
            output[remainder_start..].copy_from_slice(&data[remainder_start..]);
        }

        output
    }

    fn reverse(&self, data: &[u8]) -> Vec<u8> {
        let n = data.len() / 3;
        if n == 0 {
            return data.to_vec();
        }
        let mut output = vec![0; data.len()];

        let r_start = 0;
        let g_start = n;
        let b_start = 2 * n;

        let mut temp = data.to_vec();

        // Reverse deltas
        let reverse_plane_delta = |buf: &mut [u8], start: usize, len: usize| {
            if len <= 1 {
                return;
            }
            let mut prev = buf[start];
            for idx in 1..len {
                let diff = buf[start + idx];
                let val = diff.wrapping_add(prev);
                buf[start + idx] = val;
                prev = val;
            }
        };

        reverse_plane_delta(&mut temp, r_start, n);
        reverse_plane_delta(&mut temp, g_start, n);
        reverse_plane_delta(&mut temp, b_start, n);

        // Interleave
        for i in 0..n {
            output[i * 3] = temp[r_start + i];
            output[i * 3 + 1] = temp[g_start + i];
            output[i * 3 + 2] = temp[b_start + i];
        }

        if !data.len().is_multiple_of(3) {
            let remainder_start = n * 3;
            output[remainder_start..].copy_from_slice(&data[remainder_start..]);
        }

        output
    }
}

// =========================================================================
// 4. RGBA Delta Filter
// =========================================================================

/// Splits RGBA pixel data into contiguous R, G, B, A planes with horizontal deltas.
#[derive(Debug, Clone, Copy)]
pub struct RgbaDeltaFilter;

impl MediaFilter for RgbaDeltaFilter {
    fn filter_type(&self) -> MediaFilterType {
        MediaFilterType::RgbaDelta
    }

    fn apply(&self, data: &[u8]) -> Vec<u8> {
        let n = data.len() / 4;
        if n == 0 {
            return data.to_vec();
        }
        let mut output = vec![0; data.len()];

        let r_start = 0;
        let g_start = n;
        let b_start = 2 * n;
        let a_start = 3 * n;

        // Separate
        for i in 0..n {
            output[r_start + i] = data[i * 4];
            output[g_start + i] = data[i * 4 + 1];
            output[b_start + i] = data[i * 4 + 2];
            output[a_start + i] = data[i * 4 + 3];
        }

        // Apply 1D delta per plane
        let apply_plane_delta = |buf: &mut [u8], start: usize, len: usize| {
            if len <= 1 {
                return;
            }
            let mut prev = buf[start];
            for idx in 1..len {
                let curr = buf[start + idx];
                buf[start + idx] = curr.wrapping_sub(prev);
                prev = curr;
            }
        };

        apply_plane_delta(&mut output, r_start, n);
        apply_plane_delta(&mut output, g_start, n);
        apply_plane_delta(&mut output, b_start, n);
        apply_plane_delta(&mut output, a_start, n);

        if !data.len().is_multiple_of(4) {
            let remainder_start = n * 4;
            output[remainder_start..].copy_from_slice(&data[remainder_start..]);
        }

        output
    }

    fn reverse(&self, data: &[u8]) -> Vec<u8> {
        let n = data.len() / 4;
        if n == 0 {
            return data.to_vec();
        }
        let mut output = vec![0; data.len()];

        let r_start = 0;
        let g_start = n;
        let b_start = 2 * n;
        let a_start = 3 * n;

        let mut temp = data.to_vec();

        // Reverse deltas
        let reverse_plane_delta = |buf: &mut [u8], start: usize, len: usize| {
            if len <= 1 {
                return;
            }
            let mut prev = buf[start];
            for idx in 1..len {
                let diff = buf[start + idx];
                let val = diff.wrapping_add(prev);
                buf[start + idx] = val;
                prev = val;
            }
        };

        reverse_plane_delta(&mut temp, r_start, n);
        reverse_plane_delta(&mut temp, g_start, n);
        reverse_plane_delta(&mut temp, b_start, n);
        reverse_plane_delta(&mut temp, a_start, n);

        // Interleave
        for i in 0..n {
            output[i * 4] = temp[r_start + i];
            output[i * 4 + 1] = temp[g_start + i];
            output[i * 4 + 2] = temp[b_start + i];
            output[i * 4 + 3] = temp[a_start + i];
        }

        if !data.len().is_multiple_of(4) {
            let remainder_start = n * 4;
            output[remainder_start..].copy_from_slice(&data[remainder_start..]);
        }

        output
    }
}

// =========================================================================
// Dispatch Functions
// =========================================================================

/// Appends a media-specific filter to the data slice, dispatching dynamically.
pub fn apply_filter(filter_type: MediaFilterType, data: &[u8]) -> (Vec<u8>, MediaFilterType) {
    match filter_type {
        MediaFilterType::None => (data.to_vec(), MediaFilterType::None),
        MediaFilterType::AudioDelta { channels, bit_depth } => {
            let filter = AudioDeltaFilter::new(channels, bit_depth);
            (filter.apply(data), filter_type)
        }
        MediaFilterType::FloatSplit => {
            let filter = FloatSplitFilter;
            (filter.apply(data), filter_type)
        }
        MediaFilterType::RgbSplit => {
            let filter = RgbSplitFilter;
            (filter.apply(data), filter_type)
        }
        MediaFilterType::RgbaDelta => {
            let filter = RgbaDeltaFilter;
            (filter.apply(data), filter_type)
        }
        MediaFilterType::FloatChannelSplit { channels, mask } => {
            let filter = FloatChannelSplitFilter::new(channels, mask);
            let transformed = filter.apply(data);
            (transformed, MediaFilterType::FloatChannelSplit { channels, mask: filter.mask.get() })
        }
    }
}

/// Reverses a media-specific filter to recover original bytes, dispatching dynamically.
pub fn reverse_filter(filter_type: MediaFilterType, data: &[u8]) -> Vec<u8> {
    match filter_type {
        MediaFilterType::None => data.to_vec(),
        MediaFilterType::AudioDelta { channels, bit_depth } => {
            AudioDeltaFilter::new(channels, bit_depth).reverse(data)
        }
        MediaFilterType::FloatSplit => {
            FloatSplitFilter.reverse(data)
        }
        MediaFilterType::RgbSplit => {
            RgbSplitFilter.reverse(data)
        }
        MediaFilterType::RgbaDelta => {
            RgbaDeltaFilter.reverse(data)
        }
        MediaFilterType::FloatChannelSplit { channels, mask } => {
            FloatChannelSplitFilter::new(channels, mask).reverse(data)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audio_delta_roundtrip() {
        let original = vec![
            0x10, 0x20, // Stereo 8-bit frame 0
            0x12, 0x1F, // Stereo 8-bit frame 1
            0x15, 0x22, // Stereo 8-bit frame 2
        ];
        
        let filter = AudioDeltaFilter::new(2, 8);
        let filtered = filter.apply(&original);
        let reversed = filter.reverse(&filtered);
        
        assert_eq!(reversed, original);
    }

    #[test]
    fn test_float_split_roundtrip() {
        // Float values: 1.25f, -99.5f, 0.0003f
        let f1: f32 = 1.25;
        let f2: f32 = -99.5;
        let f3: f32 = 0.0003;
        
        let mut original = Vec::new();
        original.extend_from_slice(&f1.to_le_bytes());
        original.extend_from_slice(&f2.to_le_bytes());
        original.extend_from_slice(&f3.to_le_bytes());

        let filter = FloatSplitFilter;
        let filtered = filter.apply(&original);
        let reversed = filter.reverse(&filtered);

        assert_eq!(reversed, original);
    }

    #[test]
    fn test_float_channel_split_roundtrip() {
        // 3 channels of f32, 2 frames
        // ch0: 1.25, 2.50
        // ch1: -99.5, -99.4
        // ch2: 0.0003, 0.0004
        let f0_0: f32 = 1.25;
        let f0_1: f32 = 2.50;
        let f1_0: f32 = -99.5;
        let f1_1: f32 = -99.4;
        let f2_0: f32 = 0.0003;
        let f2_1: f32 = 0.0004;

        let mut original = Vec::new();
        original.extend_from_slice(&f0_0.to_le_bytes());
        original.extend_from_slice(&f1_0.to_le_bytes());
        original.extend_from_slice(&f2_0.to_le_bytes());
        original.extend_from_slice(&f0_1.to_le_bytes());
        original.extend_from_slice(&f1_1.to_le_bytes());
        original.extend_from_slice(&f2_1.to_le_bytes());

        let filter = FloatChannelSplitFilter::new(3, 0);
        let filtered = filter.apply(&original);
        let reversed = filter.reverse(&filtered);

        assert_eq!(reversed, original);
    }

    #[test]
    fn test_per_plane_adaptive_roundtrip() {
        let mut original = Vec::new();
        for i in 0..100 {
            let ch0 = 1.0f32;
            let ch1 = i as f32 * 0.1;
            let ch2 = (i as f32 * 543.21).sin() * 1000.0;
            
            original.extend_from_slice(&ch0.to_le_bytes());
            original.extend_from_slice(&ch1.to_le_bytes());
            original.extend_from_slice(&ch2.to_le_bytes());
        }

        let filter = FloatChannelSplitFilter::new(3, 0);
        let filtered = filter.apply(&original);
        
        let computed_mask = filter.mask.get();
        let filter_reversed = FloatChannelSplitFilter::new(3, computed_mask);
        let reversed = filter_reversed.reverse(&filtered);
        assert_eq!(reversed, original);
    }

    #[test]
    fn test_rgb_split_roundtrip() {
        let original = vec![
            255, 0, 128, // RGB 0
            250, 5, 120, // RGB 1
            245, 8, 110, // RGB 2
        ];

        let filter = RgbSplitFilter;
        let filtered = filter.apply(&original);
        let reversed = filter.reverse(&filtered);

        assert_eq!(reversed, original);
    }

    #[test]
    fn test_rgba_delta_roundtrip() {
        let original = vec![
            255, 0, 128, 64,  // RGBA 0
            250, 5, 120, 64,  // RGBA 1
            245, 8, 110, 64,  // RGBA 2
        ];

        let filter = RgbaDeltaFilter;
        let filtered = filter.apply(&original);
        let reversed = filter.reverse(&filtered);

        assert_eq!(reversed, original);
    }

    #[test]
    fn test_filter_dispatch() {
        let original = vec![
            255, 0, 128,
            250, 5, 120,
            245, 8, 110,
        ];
        
        let (filtered, filter_type) = apply_filter(MediaFilterType::RgbSplit, &original);
        assert_eq!(filter_type, MediaFilterType::RgbSplit);
        
        let reversed = reverse_filter(filter_type, &filtered);
        assert_eq!(reversed, original);
    }
}
