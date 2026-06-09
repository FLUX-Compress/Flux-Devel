//! # Range-Asymmetric Numeral Systems (rANS) Entropy Coding
//!
//! Range Asymmetric Numeral Systems (rANS) is a modern entropy coding technique
//! that provides compression density close to Shannon's optimal limit (similar to
//! Arithmetic Coding) while matching the execution speeds of Huffman Coding.
//!
//! ## Concept and Mathematical Formulation
//!
//! ANS is based on a bijection between sequences of symbols and single large integers.
//! The state of the coder is represented by a single integer $x$.
//!
//! ### Encoding Step
//! To encode a symbol $s$ with frequency $f_s$ and cumulative frequency $C_s$:
//! 1. **Renormalize**: We must ensure that the state $x$ does not grow indefinitely.
//!    If $x$ exceeds a frequency-dependent limit, we emit the lower byte to the
//!    output stream and shift $x$ down by 8 bits (1 byte):
//!    ```text
//!    while x >= (f_s << (32 - RANS_SCALE_BITS)) {
//!        emit_byte(x & 0xFF);
//!        x >>= 8;
//!    }
//!    ```
//! 2. **Update State**: The state transitions to a new value using the formula:
//!    $$x_{new} = \lfloor x / f_s \rfloor \cdot M + C_s + (x \pmod{f_s})$$
//!    where $M = 2^{\text{RANS\_SCALE\_BITS}}$ is the total probability scale (4096).
//!
//! ### Decoding Step
//! Because rANS acts like a Last-In, First-Out (LIFO) stack, decoding is the exact reverse
//! of encoding (decoding symbols in the reverse order they were encoded):
//! 1. **Find Slot**: Calculate the cumulative frequency slot:
//!    $$slot = x \pmod{M}$$
//! 2. **Find Symbol**: Lookup the symbol $s$ corresponding to this cumulative slot.
//! 3. **Update State**: Reconstruct the prior state before this symbol was encoded:
//!    $$x_{prev} = f_s \cdot \lfloor x / M \rfloor + slot - C_s$$
//! 4. **Renormalize**: If the state falls below the effective lower bound, we read 8 bits (1 byte)
//!    from the compressed stream and shift them into the state:
//!    ```text
//!    while x < EFFECTIVE_LOWER_BOUND {
//!        x = (x << 8) | read_byte();
//!    }
//!    ```
//!
//! ## Probability Precision and Scaling
//! The total probability scale is defined by $M = 2^{\text{RANS\_SCALE\_BITS}}$.
//! We use `RANS_SCALE_BITS = 12`, meaning the sum of all frequencies in the distribution
//! must equal exactly 4096. Any floating-point probabilities from PPM or the context mixer
//! are scaled and rounded using a correction algorithm to guarantee this sum, which is
//! critical for coder state synchronization.

use crate::compress::ppm::PpmEvent;

/// Probability scale precision: 12 bits = 4096 probability levels.
/// This balances compression density with memory lookup footprint and speed.
pub const RANS_SCALE_BITS: u32 = 12;

/// Lower bound of the rANS state interval.
/// Documented requirement is 1 << 23, but to align perfectly with the encoder limit
/// of `freq << (32 - RANS_SCALE_BITS)` (which is `freq << 20`), the effective state lower
/// bound during byte-by-byte renormalization is `1 << 24`.
pub const RANS_LOWER_BOUND: u32 = 1 << 23;

/// Number of bytes emitted or read per renormalization step (4 bytes = 32 bits).
pub const OUTPUT_CHUNK_SIZE: usize = 4;

/// Represents a single symbol's statistical representation scaled for rANS.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RansSymbol {
    /// Symbol frequency scaled to sum exactly to `1 << RANS_SCALE_BITS`.
    pub freq: u32,
    /// Cumulative frequency of all symbols prior to this one in alphabetical order.
    pub cumfreq: u32,
}

/// Probability table containing scaled symbol frequencies and direct alias tables for O(1) decoding.
#[derive(Clone, Debug)]
pub struct ProbabilityTable {
    /// Individual stats for each of the 256 symbols.
    pub symbols: Vec<RansSymbol>,
    /// O(1) lookup mapping each cumulative frequency slot in `[0, 4096)` directly to its symbol index.
    pub alias_table: Vec<u32>,
}

impl ProbabilityTable {
    /// Creates a new empty probability table with pre-allocated memory.
    pub fn new_empty() -> Self {
        Self {
            symbols: Vec::with_capacity(256),
            alias_table: vec![0u32; 4096],
        }
    }

    /// Updates the probability table from a probability array, reusing existing allocations.
    pub fn update_from_array(&mut self, probs: &[f32; 256]) {
        let mut freq = [0u32; 256];
        let mut last_p = -1.0f32;
        let mut last_val = 0u32;
        let mut sum = 0u32;
        let mut max_idx = 0;
        let mut max_val = 0u32;

        // 1. Initial scaling and round with fast single-element cache
        for sym in 0..256 {
            let p = probs[sym];
            if p > 0.0 {
                let val = if p == last_p {
                    last_val
                } else {
                    let f = (p * 4096.0 + 0.5) as u32;
                    let val = f.max(1);
                    last_p = p;
                    last_val = val;
                    val
                };
                freq[sym] = val;
                sum += val;
                if val > max_val {
                    max_val = val;
                    max_idx = sym;
                }
            }
        }

        // 2. Fallback to uniform distribution if the sum is zero
        if sum == 0 {
            for val in freq.iter_mut() {
                *val = 16; // 256 * 16 = 4096
            }
            sum = 4096;
        }

        // 3. Rounding adjustment: the symbol with the largest frequency absorbs the error
        let error = 4096_i32 - sum as i32;
        if error != 0 {
            if max_val > 0 {
                let new_val = freq[max_idx] as i32 + error;
                freq[max_idx] = new_val.max(1) as u32;
            }

            // Re-verify exact sum matches 4096 in case clamping was triggered
            let final_sum: u32 = freq.iter().sum();
            if final_sum != 4096 {
                if let Some(idx) = freq.iter().position(|&f| f > 0) {
                    let diff = 4096_i32 - final_sum as i32;
                    freq[idx] = (freq[idx] as i32 + diff).max(1) as u32;
                }
            }
        }

        // 4. Construct RansSymbol list with cumulative frequencies
        self.symbols.clear();
        let mut cumfreq = 0;
        for &f in &freq {
            self.symbols.push(RansSymbol { freq: f, cumfreq });
            cumfreq += f;
        }

        // 5. Populate direct O(1) lookup table
        if self.alias_table.len() < 4096 {
            self.alias_table.resize(4096, 0);
        }
        for (sym, s_info) in self.symbols.iter().enumerate() {
            for offset in 0..s_info.freq {
                let slot = (s_info.cumfreq + offset) as usize;
                if slot < 4096 {
                    self.alias_table[slot] = sym as u32;
                }
            }
        }
    }

    /// Builds a probability table from a slice of (symbol, probability) pairs.
    ///
    /// Scales the probabilities to sum to exactly 4096 (`1 << RANS_SCALE_BITS`).
    /// Ensures that any symbol with non-zero probability receives at least 1 slot,
    /// and that any surplus or deficit resulting from roundings is adjusted by modifying
    /// the frequencies of the highest-frequency symbols to minimize prediction bias.
    pub fn from_probabilities(probs: &[(u8, f32)]) -> Self {
        let mut freq = vec![0u32; 256];

        // 1. Initial scaling and round
        for &(sym, p) in probs {
            if p > 0.0 {
                let f = (p * 4096.0 + 0.5) as u32;
                freq[sym as usize] = f.max(1);
            }
        }

        let mut sum: u32 = freq.iter().sum();

        // 2. Fallback to uniform distribution if the sum is zero
        if sum == 0 {
            for val in freq.iter_mut() {
                *val = 16; // 256 * 16 = 4096
            }
            sum = 4096;
        }

        // 3. Rounding adjustment: the symbol with the largest frequency absorbs the error
        let error = 4096_i32 - sum as i32;
        if error != 0 {
            if let Some((max_idx, _)) = freq
                .iter()
                .enumerate()
                .filter(|&(_, &f)| f > 0)
                .max_by_key(|&(_, &f)| f)
            {
                let new_val = freq[max_idx] as i32 + error;
                freq[max_idx] = new_val.max(1) as u32;
            }

            // Re-verify exact sum matches 4096 in case clamping was triggered
            let final_sum: u32 = freq.iter().sum();
            if final_sum != 4096 {
                if let Some(idx) = freq.iter().position(|&f| f > 0) {
                    let diff = 4096_i32 - final_sum as i32;
                    freq[idx] = (freq[idx] as i32 + diff).max(1) as u32;
                }
            }
        }

        // 4. Construct RansSymbol list with cumulative frequencies
        let mut symbols = Vec::with_capacity(256);
        let mut cumfreq = 0;
        for &f in &freq {
            symbols.push(RansSymbol { freq: f, cumfreq });
            cumfreq += f;
        }

        // 5. Populate direct O(1) lookup table
        let mut alias_table = vec![0u32; 4096];
        for (sym, s_info) in symbols.iter().enumerate() {
            for offset in 0..s_info.freq {
                let slot = (s_info.cumfreq + offset) as usize;
                if slot < 4096 {
                    alias_table[slot] = sym as u32;
                }
            }
        }

        Self {
            symbols,
            alias_table,
        }
    }

    /// Builds a probability table from a slice of observed PPM events.
    pub fn from_ppm_events(events: &[PpmEvent]) -> Self {
        let mut counts = vec![0u32; 256];
        let mut total = 0;

        for event in events {
            if let PpmEvent::Symbol { symbol, .. } = event {
                counts[*symbol as usize] += 1;
                total += 1;
            }
        }

        let mut probs = Vec::new();
        if total > 0 {
            for (sym, &count) in counts.iter().enumerate() {
                if count > 0 {
                    probs.push((sym as u8, count as f32 / total as f32));
                }
            }
        }

        Self::from_probabilities(&probs)
    }

    /// Resolves a symbol index given a cumulative frequency slot in `[0, 4096)` in $O(1)$ time.
    #[inline]
    pub fn symbol_at_cumfreq(&self, cumfreq: u32) -> u8 {
        self.alias_table[cumfreq as usize] as u8
    }

    pub fn rans_symbol_for_symbol(probs: &[(u8, f32)], symbol: u8) -> RansSymbol {
        let mut freq = [0u32; 256];
        let mut sum = 0u32;
        let mut max_idx = 0;
        let mut max_val = 0u32;

        // 1. Initial scaling and round
        for &(sym, p) in probs {
            if p > 0.0 {
                let f = (p * 4096.0 + 0.5) as u32;
                let val = f.max(1);
                freq[sym as usize] = val;
                sum += val;
                if val > max_val {
                    max_val = val;
                    max_idx = sym as usize;
                }
            }
        }

        // 2. Fallback to uniform distribution if the sum is zero
        if sum == 0 {
            for val in freq.iter_mut() {
                *val = 16; // 256 * 16 = 4096
            }
            sum = 4096;
        }

        // 3. Rounding adjustment: the symbol with the largest frequency absorbs the error
        let error = 4096_i32 - sum as i32;
        if error != 0 {
            if max_val > 0 {
                let new_val = freq[max_idx] as i32 + error;
                freq[max_idx] = new_val.max(1) as u32;
            }

            // Re-verify exact sum matches 4096 in case clamping was triggered
            let final_sum: u32 = freq.iter().sum();
            if final_sum != 4096 {
                if let Some(idx) = freq.iter().position(|&f| f > 0) {
                    let diff = 4096_i32 - final_sum as i32;
                    freq[idx] = (freq[idx] as i32 + diff).max(1) as u32;
                }
            }
        }

        // Compute cumulative frequency for the target symbol
        let mut cumfreq = 0;
        for &f in freq.iter().take(symbol as usize) {
            cumfreq += f;
        }

        RansSymbol {
            freq: freq[symbol as usize],
            cumfreq,
        }
    }

    pub fn rans_symbol_for_array(probs: &[f32; 256], symbol: u8) -> RansSymbol {
        let mut freq = [0u32; 256];
        let mut last_p = -1.0f32;
        let mut last_val = 0u32;
        let mut sum = 0u32;
        let mut max_idx = 0;
        let mut max_val = 0u32;

        // 1. Initial scaling and round with fast single-element cache
        for sym in 0..256 {
            let p = probs[sym];
            if p > 0.0 {
                let val = if p == last_p {
                    last_val
                } else {
                    let f = (p * 4096.0 + 0.5) as u32;
                    let val = f.max(1);
                    last_p = p;
                    last_val = val;
                    val
                };
                freq[sym] = val;
                sum += val;
                if val > max_val {
                    max_val = val;
                    max_idx = sym;
                }
            }
        }

        // 2. Fallback to uniform distribution if the sum is zero
        if sum == 0 {
            for val in freq.iter_mut() {
                *val = 16; // 256 * 16 = 4096
            }
            sum = 4096;
        }

        // 3. Rounding adjustment: the symbol with the largest frequency absorbs the error
        let error = 4096_i32 - sum as i32;
        if error != 0 {
            if max_val > 0 {
                let new_val = freq[max_idx] as i32 + error;
                freq[max_idx] = new_val.max(1) as u32;
            }

            // Re-verify exact sum matches 4096 in case clamping was triggered
            let final_sum: u32 = freq.iter().sum();
            if final_sum != 4096 {
                if let Some(idx) = freq.iter().position(|&f| f > 0) {
                    let diff = 4096_i32 - final_sum as i32;
                    freq[idx] = (freq[idx] as i32 + diff).max(1) as u32;
                }
            }
        }

        // Compute cumulative frequency for the target symbol
        let mut cumfreq = 0;
        for &f in freq.iter().take(symbol as usize) {
            cumfreq += f;
        }

        RansSymbol {
            freq: freq[symbol as usize],
            cumfreq,
        }
    }
}

/// rANS Encoder.
pub struct RansEncoder {
    /// The current 64-bit encoder state. Starts at effective lower bound 1 << 24.
    pub state: u64,
    /// Renormalization byte output buffer.
    pub output: Vec<u8>,
}

impl RansEncoder {
    /// Creates a new rANS Encoder initialized to the effective state lower bound (1 << 24).
    pub fn new() -> Self {
        Self {
            state: (1 << 24) as u64,
            output: Vec::new(),
        }
    }

    /// Encodes a single symbol using its probability table.
    ///
    /// 1. **Renormalize**: Emits the lowest byte of the state if keeping the symbol
    ///    would cause the updated state to overflow the limits.
    /// 2. **Apply bijection**: Transforms the state using the rANS state update formula:
    ///    $$x = \lfloor x / f_s \rfloor \cdot M + C_s + (x \pmod{f_s})$$
    pub fn encode_symbol(&mut self, symbol: u8, table: &ProbabilityTable) {
        let sym_info = &table.symbols[symbol as usize];
        let freq = sym_info.freq as u64;
        let cumfreq = sym_info.cumfreq as u64;

        assert!(
            freq > 0,
            "Attempted to encode symbol '{}' with 0 frequency",
            symbol
        );

        // Normalize state to fit within target range
        let limit = freq << (32 - RANS_SCALE_BITS);
        while self.state >= limit {
            self.output.push((self.state & 0xFF) as u8);
            self.state >>= 8;
        }

        // Apply state transition
        let q = self.state / freq;
        let r = self.state % freq;
        self.state = (q << RANS_SCALE_BITS) + cumfreq + r;
    }

    /// Encodes a symbol using its pre-computed frequency and cumulative frequency.
    ///
    /// This bypasses the need for a full `ProbabilityTable` lookup.
    pub fn encode_symbol_with_freq(&mut self, freq: u32, cumfreq: u32) {
        let freq = freq as u64;
        let cumfreq = cumfreq as u64;

        assert!(freq > 0, "Attempted to encode symbol with 0 frequency");

        // Normalize state to fit within target range
        let limit = freq << (32 - RANS_SCALE_BITS);
        while self.state >= limit {
            self.output.push((self.state & 0xFF) as u8);
            self.state >>= 8;
        }

        // Apply state transition
        let q = self.state / freq;
        let r = self.state % freq;
        self.state = (q << RANS_SCALE_BITS) + cumfreq + r;
    }

    /// Flushes the final state of the encoder to the output.
    ///
    /// Appends the final 64-bit state in big-endian format (8 bytes) to the output stream.
    /// The final bytes represent the entry point for the LIFO decoder.
    pub fn flush(&mut self) -> Vec<u8> {
        let mut final_out = self.output.clone();
        final_out.extend_from_slice(&self.state.to_be_bytes());
        final_out
    }
}

impl Default for RansEncoder {
    fn default() -> Self {
        Self::new()
    }
}

/// rANS Decoder.
pub struct RansDecoder {
    /// The current 64-bit decoder state.
    pub state: u64,
    /// Renormalization input byte buffer.
    pub input: Vec<u8>,
    /// Read position (points to the next byte to read backwards from the end of the input buffer).
    pub pos: usize,
}

impl RansDecoder {
    /// Creates a new rANS Decoder initialized from flushed compressed bytes.
    ///
    /// The last 8 bytes are extracted to restore the final state. The remaining
    /// stream is processed in reverse (LIFO).
    pub fn new(data: Vec<u8>) -> Self {
        assert!(data.len() >= 8, "Data too short to initialize rANS decoder");
        let len = data.len();
        let state_bytes = &data[len - 8..];
        let state = u64::from_be_bytes([
            state_bytes[0],
            state_bytes[1],
            state_bytes[2],
            state_bytes[3],
            state_bytes[4],
            state_bytes[5],
            state_bytes[6],
            state_bytes[7],
        ]);

        let input = data[..len - 8].to_vec();
        let pos = input.len();

        Self { state, input, pos }
    }

    /// Decodes a single symbol using its probability table.
    ///
    /// 1. **Identify**: Extract the cumulative frequency slot: `slot = state % M`.
    /// 2. **Resolve**: Lookup symbol $s$ corresponding to the slot using the $O(1)$ alias table.
    /// 3. **Reverse step**: Update state to the prior state:
    ///    $$x = f_s \cdot \lfloor x / M \rfloor + slot - C_s$$
    /// 4. **Renormalize**: If state drops below effective lower bound (1 << 24), consume 8 bits (1 byte) from the input buffer.
    pub fn decode_symbol(&mut self, table: &ProbabilityTable) -> u8 {
        let mask = (1 << RANS_SCALE_BITS) - 1;
        let slot = (self.state & mask) as u32;

        let symbol = table.symbol_at_cumfreq(slot);
        let sym_info = &table.symbols[symbol as usize];
        let freq = sym_info.freq as u64;
        let cumfreq = sym_info.cumfreq as u64;

        // Restore state
        self.state = freq * (self.state >> RANS_SCALE_BITS) + (slot as u64) - cumfreq;

        // Renormalize if state drops below effective lower bound
        while self.state < (1 << 24) && self.pos > 0 {
            self.pos -= 1;
            let val = self.input[self.pos] as u64;
            self.state = (self.state << 8) | val;
        }

        symbol
    }

    /// Checks if decoding is complete (the coder returned to `1 << 24` and all input was consumed).
    pub fn is_complete(&self) -> bool {
        self.state == (1 << 24) && self.pos == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compress::lz77::{Lz77Encoder, Lz77Token};
    use crate::compress::mixer::ContextMixer;
    use crate::compress::ppm::PpmModel;

    #[test]
    fn test_probability_table_sums_to_scale() {
        // Awkward distribution that sums exactly to 1.0 but causes raw rounding issues
        let probs = vec![(b'a', 0.333), (b'b', 0.333), (b'c', 0.334)];
        let table = ProbabilityTable::from_probabilities(&probs);
        let sum: u32 = table.symbols.iter().map(|s| s.freq).sum();
        assert_eq!(sum, 1 << RANS_SCALE_BITS);

        // Awkward distribution that does not sum to 1.0
        let probs2 = vec![(b'a', 0.333), (b'b', 0.333), (b'c', 0.333)];
        let table2 = ProbabilityTable::from_probabilities(&probs2);
        let sum2: u32 = table2.symbols.iter().map(|s| s.freq).sum();
        assert_eq!(sum2, 1 << RANS_SCALE_BITS);
    }

    #[test]
    fn test_rans_encode_decode_single_symbol() {
        let probs = vec![(b'a', 1.0)];
        let table = ProbabilityTable::from_probabilities(&probs);

        let mut encoder = RansEncoder::new();
        encoder.encode_symbol(b'a', &table);
        let compressed = encoder.flush();

        let mut decoder = RansDecoder::new(compressed);
        let sym = decoder.decode_symbol(&table);
        assert_eq!(sym, b'a');
        assert!(decoder.is_complete());
    }

    #[test]
    fn test_rans_encode_decode_uniform_distribution() {
        let mut probs = Vec::new();
        for i in 0..256 {
            probs.push((i as u8, 1.0 / 256.0));
        }
        let table = ProbabilityTable::from_probabilities(&probs);

        let symbols = vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        let mut encoder = RansEncoder::new();
        for &sym in &symbols {
            encoder.encode_symbol(sym, &table);
        }
        let compressed = encoder.flush();

        let mut decoder = RansDecoder::new(compressed);
        let mut decoded = Vec::new();
        for _ in 0..symbols.len() {
            decoded.push(decoder.decode_symbol(&table));
        }

        // rANS is a stack: symbols are decoded in reverse order
        decoded.reverse();
        assert_eq!(decoded, symbols);
        assert!(decoder.is_complete());
    }

    #[test]
    fn test_rans_encode_decode_skewed_distribution() {
        // 'a' has 90% probability, 'b' has 10%
        let probs = vec![(b'a', 0.9), (b'b', 0.1)];
        let table = ProbabilityTable::from_probabilities(&probs);

        let symbols = vec![b'a', b'a', b'b', b'a', b'a'];
        let mut encoder = RansEncoder::new();
        for &sym in &symbols {
            encoder.encode_symbol(sym, &table);
        }
        let compressed = encoder.flush();

        let mut decoder = RansDecoder::new(compressed);
        let mut decoded = Vec::new();
        for _ in 0..symbols.len() {
            decoded.push(decoder.decode_symbol(&table));
        }
        decoded.reverse();
        assert_eq!(decoded, symbols);
    }

    #[test]
    fn test_rans_encode_decode_roundtrip_sequence() {
        let mut probs = Vec::new();
        for i in 0..256 {
            probs.push((i as u8, 1.0 / 256.0));
        }
        let table = ProbabilityTable::from_probabilities(&probs);

        // Encode 100 pseudo-random-like symbols
        let mut symbols = Vec::new();
        for i in 0..100 {
            symbols.push((i * 17 + 5) as u8);
        }

        let mut encoder = RansEncoder::new();
        for &sym in &symbols {
            encoder.encode_symbol(sym, &table);
        }
        let compressed = encoder.flush();

        let mut decoder = RansDecoder::new(compressed);
        let mut decoded = Vec::new();
        for _ in 0..symbols.len() {
            decoded.push(decoder.decode_symbol(&table));
        }
        decoded.reverse();
        assert_eq!(decoded, symbols);
        assert!(decoder.is_complete());
    }

    #[test]
    fn test_rans_decode_is_reverse_of_encode() {
        let probs = vec![(b'a', 0.4), (b'b', 0.6)];
        let table = ProbabilityTable::from_probabilities(&probs);

        let seq = vec![b'a', b'b', b'b', b'a'];

        let mut encoder = RansEncoder::new();
        for &sym in &seq {
            encoder.encode_symbol(sym, &table);
        }
        let compressed = encoder.flush();

        let mut decoder = RansDecoder::new(compressed);
        // The decoder yields the sequence in reverse order (LIFO)
        let s1 = decoder.decode_symbol(&table);
        let s2 = decoder.decode_symbol(&table);
        let s3 = decoder.decode_symbol(&table);
        let s4 = decoder.decode_symbol(&table);

        assert_eq!(s1, b'a');
        assert_eq!(s2, b'b');
        assert_eq!(s3, b'b');
        assert_eq!(s4, b'a');
    }

    #[test]
    fn test_full_pipeline_text() {
        // 1. Take a sample text string (at least 200 bytes, with repetition)
        let data = b"Prediction by Partial Matching (PPM) variant I (PPM-I) \
                     with adaptive order up to MAX_ORDER. PPM builds a statistical model \
                     of the data and predicts the probability of each next symbol. \
                     PPM builds a statistical model and predicts the probability. \
                     PPM builds a statistical model and predicts the probability.";

        // 2. Run through LZ77 encoder
        let mut lz77 = Lz77Encoder::new(true);
        let tokens = lz77.encode(data);

        // Extract literals to feed to PPM
        let mut literals = Vec::new();
        for token in &tokens {
            if let Lz77Token::Literal(b) = token {
                literals.push(*b);
            }
        }

        assert!(!literals.is_empty(), "LZ77 must produce some literals");

        // 3. Feed LZ77 literals into PPM model, get probability events, and blend via mixer
        let mut ppm = PpmModel::new(4);
        let mut mixer = ContextMixer::new();
        let mut tables = Vec::new();
        let mut encoded_symbols = Vec::new();

        for &sym in &literals {
            let events = ppm.encode_symbol(sym);
            // Blend with a mock secondary prediction (0.5 since we have no stride context here)
            let table = mixer.blend_to_table(&events, 0.5, sym);
            tables.push(table);
            encoded_symbols.push(sym);
        }

        // 4. Encode symbols with rANS
        let mut encoder = RansEncoder::new();
        for i in 0..encoded_symbols.len() {
            encoder.encode_symbol(encoded_symbols[i], &tables[i]);
        }
        let compressed = encoder.flush();

        // 5. Decode with rANS
        let mut decoder = RansDecoder::new(compressed);
        let mut decoded = vec![0u8; encoded_symbols.len()];
        // Since rANS is a stack, decode in reverse order
        for i in (0..encoded_symbols.len()).rev() {
            decoded[i] = decoder.decode_symbol(&tables[i]);
        }

        // 6. Verify decoded symbols match original PPM events
        assert_eq!(decoded, encoded_symbols);
        assert!(decoder.is_complete());
    }

    #[test]
    fn test_rans_symbol_for_array_equivalence() {
        let mut probs_array = [0.0f32; 256];
        probs_array[b'a' as usize] = 0.3;
        probs_array[b'b' as usize] = 0.5;
        probs_array[b'c' as usize] = 0.2;

        let probs_list = vec![(b'a', 0.3), (b'b', 0.5), (b'c', 0.2)];

        for &sym in b"abcd" {
            let expected = ProbabilityTable::rans_symbol_for_symbol(&probs_list, sym);
            let actual = ProbabilityTable::rans_symbol_for_array(&probs_array, sym);
            assert_eq!(expected, actual);
        }

        // Test with uniform fallback / zero distribution
        let zero_array = [0.0f32; 256];
        let zero_list = vec![];
        for sym in 0..=255 {
            let expected = ProbabilityTable::rans_symbol_for_symbol(&zero_list, sym);
            let actual = ProbabilityTable::rans_symbol_for_array(&zero_array, sym);
            assert_eq!(expected, actual);
        }
    }
}
