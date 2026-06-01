//! Prediction by Partial Matching (PPM) Order-4 Context Model
//!
//! Stored in a flat pre-allocated arena structure to avoid dynamic heap allocation
//! overhead and pointer fragmentation. Features inline single-symbol optimization
//! and block-level seekability.
//!
//! Dual licensed under GPL and commercial licenses for FLUX.
//! Credits: Cleary & Witten (PPM), Dmitry Shkarin (PPMd design concepts),
//! Igor Pavlov (LZMA optimal parsing insights).

use crate::compress::rans::RansSymbol;

/// Statistics representing a seen symbol and its frequency inside a context.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct PpmState {
    /// The observed literal byte.
    pub symbol: u8,
    /// Frequency count of the symbol in this context (clamped to 255).
    pub freq: u8,
    /// Offset/Index into the contexts arena for the successor context (0 = none).
    pub successor: u32,
}

/// A node in the PPM context trie structure.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct PpmContext {
    /// Number of unique symbols seen in this context.
    pub num_stats: u16,
    /// Suffix link pointing to the context of order-(k-1) (u32::MAX if none).
    pub suffix: u32,
    /// Index in the states arena where the symbol array begins (if num_stats > 1),
    /// or successor pointer index (if num_stats == 1).
    pub states_offset: u32,
    /// Inline symbol storage for single-symbol context optimization.
    pub inline_symbol: u8,
    /// Inline frequency storage for single-symbol context optimization.
    pub inline_freq: u8,
}

/// Represents an event generated during PPM encoding (compatibility enum).
#[derive(Clone, Debug, PartialEq)]
pub enum PpmEvent {
    /// Represents encoding of a symbol in the current context.
    Symbol {
        symbol: u8,
        probability: f32,
        cumulative: f32,
    },
    /// Represents fallback to a lower-order context.
    Escape {
        order: usize,
        escape_prob: f32,
    },
}

/// PPM Model with adaptive contexts up to order-4.
pub struct PpmModel {
    /// Arena storing all context nodes. Context 0 is always the root.
    pub contexts: Vec<PpmContext>,
    /// Arena storing all symbol frequency state slices.
    pub states: Vec<PpmState>,
    /// Active context indices for order 0, 1, 2, 3, 4.
    pub active_contexts: [u32; 5],
    /// Maximum allowed memory size in bytes for the arenas.
    pub max_arena_size: usize,
}

/// Represents an encoding event for rANS (high-performance struct).
#[derive(Clone, Copy, Debug)]
pub struct PpmEncodeEvent {
    /// Scaled frequency of the symbol or escape.
    pub freq: u32,
    /// Cumulative frequency of the symbol or escape.
    pub cumfreq: u32,
}

impl PpmModel {
    /// Creates a new PPM model with a pre-allocated arena limit.
    pub fn new(max_arena_size: usize) -> Self {
        let max_ctx_count = max_arena_size / std::mem::size_of::<PpmContext>();
        let max_state_count = max_arena_size / std::mem::size_of::<PpmState>();
        
        let mut model = Self {
            contexts: Vec::with_capacity(max_ctx_count),
            states: Vec::with_capacity(max_state_count),
            active_contexts: [0; 5],
            max_arena_size,
        };
        model.reset();
        model
    }

    /// Resets the trie structures and arena indices to the initial state.
    pub fn reset(&mut self) {
        self.contexts.clear();
        self.states.clear();

        let max_ctx_count = self.max_arena_size / std::mem::size_of::<PpmContext>();
        let max_state_count = self.max_arena_size / std::mem::size_of::<PpmState>();
        self.contexts.reserve(max_ctx_count);
        self.states.reserve(max_state_count);

        self.contexts.push(PpmContext {
            num_stats: 0,
            suffix: u32::MAX,
            states_offset: 0,
            inline_symbol: 0,
            inline_freq: 0,
        });
        self.active_contexts = [0; 5];
    }

    /// Verifies if the pre-allocated arena is full, resetting if limit is breached.
    pub fn check_and_reset(&mut self) -> bool {
        let current_size = self.contexts.len() * std::mem::size_of::<PpmContext>()
            + self.states.len() * std::mem::size_of::<PpmState>();
        if current_size >= self.max_arena_size {
            self.reset();
            true
        } else {
            false
        }
    }

    /// Performs context search to retrieve the transition target (0 if none).
    pub fn find_transition(&self, ctx_idx: u32, symbol: u8) -> u32 {
        let ctx = &self.contexts[ctx_idx as usize];
        if ctx.num_stats == 0 {
            return 0;
        }
        if ctx.num_stats == 1 {
            if ctx.inline_symbol == symbol {
                return ctx.states_offset;
            }
        } else {
            let start = ctx.states_offset as usize;
            for i in 0..(ctx.num_stats as usize) {
                let state = &self.states[start + i];
                if state.symbol == symbol {
                    return state.successor;
                }
            }
        }
        0
    }

    /// Updates statistical symbol counters within a context.
    pub fn update_context_symbol(&mut self, ctx_idx: u32, s: u8) {
        let num_stats = self.contexts[ctx_idx as usize].num_stats;
        if num_stats == 0 {
            let ctx = &mut self.contexts[ctx_idx as usize];
            ctx.num_stats = 1;
            ctx.inline_symbol = s;
            ctx.inline_freq = 1;
            ctx.states_offset = 0;
        } else if num_stats == 1 {
            let inline_symbol = self.contexts[ctx_idx as usize].inline_symbol;
            let inline_freq = self.contexts[ctx_idx as usize].inline_freq;
            let successor = self.contexts[ctx_idx as usize].states_offset;

            if inline_symbol == s {
                let ctx = &mut self.contexts[ctx_idx as usize];
                ctx.inline_freq = inline_freq.saturating_add(1);
            } else {
                let new_offset = self.states.len() as u32;
                self.states.push(PpmState {
                    symbol: inline_symbol,
                    freq: inline_freq,
                    successor,
                });
                self.states.push(PpmState {
                    symbol: s,
                    freq: 1,
                    successor: 0,
                });

                let ctx = &mut self.contexts[ctx_idx as usize];
                ctx.num_stats = 2;
                ctx.states_offset = new_offset;
            }
        } else {
            let offset = self.contexts[ctx_idx as usize].states_offset as usize;
            let mut found = false;
            for i in 0..(num_stats as usize) {
                if self.states[offset + i].symbol == s {
                    self.states[offset + i].freq = self.states[offset + i].freq.saturating_add(1);
                    found = true;
                    break;
                }
            }

            if !found {
                let new_offset = self.states.len() as u32;
                for i in 0..(num_stats as usize) {
                    self.states.push(self.states[offset + i]);
                }
                self.states.push(PpmState {
                    symbol: s,
                    freq: 1,
                    successor: 0,
                });

                let ctx = &mut self.contexts[ctx_idx as usize];
                ctx.num_stats += 1;
                ctx.states_offset = new_offset;
            }
        }
    }

    /// Finds or creates a successor transition node from the context.
    pub fn find_or_create_transition(&mut self, ctx_idx: u32, s: u8) -> u32 {
        let num_stats = self.contexts[ctx_idx as usize].num_stats;
        
        if num_stats == 1 {
            let successor = self.contexts[ctx_idx as usize].states_offset;
            if successor != 0 {
                return successor;
            }

            let new_ctx_idx = self.contexts.len() as u32;
            self.contexts.push(PpmContext {
                num_stats: 0,
                suffix: u32::MAX,
                states_offset: 0,
                inline_symbol: 0,
                inline_freq: 0,
            });

            let ctx = &mut self.contexts[ctx_idx as usize];
            ctx.states_offset = new_ctx_idx;
            new_ctx_idx
        } else {
            let offset = self.contexts[ctx_idx as usize].states_offset as usize;
            for i in 0..(num_stats as usize) {
                if self.states[offset + i].symbol == s {
                    let successor = self.states[offset + i].successor;
                    if successor != 0 {
                        return successor;
                    }

                    let new_ctx_idx = self.contexts.len() as u32;
                    self.contexts.push(PpmContext {
                        num_stats: 0,
                        suffix: u32::MAX,
                        states_offset: 0,
                        inline_symbol: 0,
                        inline_freq: 0,
                    });

                    self.states[offset + i].successor = new_ctx_idx;
                    return new_ctx_idx;
                }
            }
            unreachable!("Symbol {} not found in context states", s);
        }
    }

    /// Scales the context statistics to fit exactly a 4096-slot rANS distribution.
    pub fn get_scaled_context(&self, ctx_idx: u32) -> [RansSymbol; 257] {
        let ctx = &self.contexts[ctx_idx as usize];
        let mut scaled = [RansSymbol { freq: 0, cumfreq: 0 }; 257];
        if ctx.num_stats == 0 {
            scaled[256] = RansSymbol { freq: 4096, cumfreq: 0 };
            return scaled;
        }

        let mut raw_symbols = Vec::with_capacity(ctx.num_stats as usize + 1);
        let mut total_raw: u32 = 0;

        if ctx.num_stats == 1 {
            raw_symbols.push((ctx.inline_symbol as u16, ctx.inline_freq as u32));
            total_raw += ctx.inline_freq as u32;
        } else {
            let start = ctx.states_offset as usize;
            for i in 0..(ctx.num_stats as usize) {
                let state = &self.states[start + i];
                raw_symbols.push((state.symbol as u16, state.freq as u32));
                total_raw += state.freq as u32;
            }
        }

        let k = ctx.num_stats as u32;
        raw_symbols.push((256, k));
        total_raw += k;

        let mut sum_scaled = 0u32;
        let mut max_raw = 0u32;
        let mut max_idx = 0;

        let mut scaled_freqs = vec![0u32; raw_symbols.len()];
        for (i, &(_, f)) in raw_symbols.iter().enumerate() {
            let scaled_f = (f * 4096 + total_raw / 2) / total_raw;
            let scaled_f = scaled_f.max(1);
            scaled_freqs[i] = scaled_f;
            sum_scaled += scaled_f;
            if f > max_raw {
                max_raw = f;
                max_idx = i;
            }
        }

        let mut error = 4096_i32 - sum_scaled as i32;
        if error > 0 {
            scaled_freqs[max_idx] += error as u32;
        } else if error < 0 {
            let mut indices: Vec<usize> = (0..scaled_freqs.len()).collect();
            indices.sort_by_key(|&idx| std::cmp::Reverse(scaled_freqs[idx]));
            for idx in indices {
                let current = scaled_freqs[idx] as i32;
                let limit = current - 1;
                if limit > 0 {
                    let subtract = limit.min(-error);
                    scaled_freqs[idx] -= subtract as u32;
                    error += subtract;
                    if error == 0 {
                        break;
                    }
                }
            }
        }

        debug_assert_eq!(scaled_freqs.iter().sum::<u32>(), 4096);

        let mut sorted_indices: Vec<usize> = (0..raw_symbols.len()).collect();
        sorted_indices.sort_by_key(|&idx| raw_symbols[idx].0);

        let mut cum = 0;
        for idx in sorted_indices {
            let (sym, _) = raw_symbols[idx];
            let freq = scaled_freqs[idx];
            scaled[sym as usize] = RansSymbol { freq, cumfreq: cum };
            cum += freq;
        }

        scaled
    }

    /// Feeds the trie state machine with symbol `s` and shifts active history tracking.
    pub fn update(&mut self, s: u8) {
        if self.check_and_reset() {
            // Context resets to root
        }

        let mut next_active = [0u32; 5];
        next_active[0] = 0;

        self.update_context_symbol(0, s);
        let mut last_t = self.find_or_create_transition(0, s);
        next_active[1] = last_t;

        for j in 1..=3 {
            let parent = self.active_contexts[j];
            if parent != 0 {
                self.update_context_symbol(parent, s);
                let t = self.find_or_create_transition(parent, s);
                self.contexts[t as usize].suffix = last_t;
                next_active[j + 1] = t;
                last_t = t;
            } else {
                break;
            }
        }

        let parent4 = self.active_contexts[4];
        if parent4 != 0 {
            self.update_context_symbol(parent4, s);
        }

        self.active_contexts = next_active;
    }

    /// Evaluates the context order layers and writes matching probability outcomes to events.
    pub fn encode_literal(&mut self, s: u8, events: &mut Vec<PpmEncodeEvent>) {
        let mut highest_order = 0;
        for j in (0..=4).rev() {
            if self.active_contexts[j] != 0 {
                highest_order = j;
                break;
            }
        }

        let mut found_order = None;
        for order in (0..=highest_order).rev() {
            let ctx_idx = self.active_contexts[order];
            if self.contexts[ctx_idx as usize].num_stats > 0 {
                let scaled = self.get_scaled_context(ctx_idx);
                if scaled[s as usize].freq > 0 {
                    found_order = Some(order);
                    break;
                }
            }
        }

        if let Some(target_order) = found_order {
            for order in (target_order + 1..=highest_order).rev() {
                let ctx_idx = self.active_contexts[order];
                if self.contexts[ctx_idx as usize].num_stats > 0 {
                    let scaled = self.get_scaled_context(ctx_idx);
                    let esc_info = scaled[256];
                    events.push(PpmEncodeEvent {
                        freq: esc_info.freq,
                        cumfreq: esc_info.cumfreq,
                    });
                }
            }
            let ctx_idx = self.active_contexts[target_order];
            let scaled = self.get_scaled_context(ctx_idx);
            let sym_info = scaled[s as usize];
            events.push(PpmEncodeEvent {
                freq: sym_info.freq,
                cumfreq: sym_info.cumfreq,
            });
        } else {
            for order in (0..=highest_order).rev() {
                let ctx_idx = self.active_contexts[order];
                if self.contexts[ctx_idx as usize].num_stats > 0 {
                    let scaled = self.get_scaled_context(ctx_idx);
                    let esc_info = scaled[256];
                    events.push(PpmEncodeEvent {
                        freq: esc_info.freq,
                        cumfreq: esc_info.cumfreq,
                    });
                }
            }
            events.push(PpmEncodeEvent {
                freq: 16,
                cumfreq: s as u32 * 16,
            });
        }

        self.update(s);
    }

    /// Decodes a single symbol from rANS utilizing the PPM model contexts.
    pub fn decode_literal(&mut self, decoder: &mut crate::compress::rans::RansDecoder) -> Result<u8, crate::archive::index::ArchiveError> {
        let mut highest_order = 0;
        for j in (0..=4).rev() {
            if self.active_contexts[j] != 0 {
                highest_order = j;
                break;
            }
        }

        let mut current_order = highest_order;
        loop {
            let ctx_idx = self.active_contexts[current_order];
            let num_stats = self.contexts[ctx_idx as usize].num_stats;

            if num_stats > 0 {
                let scaled = self.get_scaled_context(ctx_idx);
                let mask = 4095;
                let slot = (decoder.state & mask) as u32;

                let mut decoded_sym = None;
                for (sym, s_info) in scaled.iter().enumerate() {
                    if s_info.freq > 0 && slot >= s_info.cumfreq && slot < s_info.cumfreq + s_info.freq {
                        decoded_sym = Some(sym as u16);
                        break;
                    }
                }

                let sym = decoded_sym.ok_or(crate::archive::index::ArchiveError::CorruptIndex)?;
                let sym_info = scaled[sym as usize];
                decoder.state = (sym_info.freq as u64) * (decoder.state >> 12) + (slot as u64) - (sym_info.cumfreq as u64);
                while decoder.state < (1 << 24) && decoder.pos > 0 {
                    decoder.pos -= 1;
                    let val = decoder.input[decoder.pos] as u64;
                    decoder.state = (decoder.state << 8) | val;
                }

                if sym == 256 {
                    if current_order > 0 {
                        current_order -= 1;
                    } else {
                        let slot = (decoder.state & mask) as u32;
                        let sym = (slot / 16) as u8;
                        let cumfreq = sym as u64 * 16;
                        decoder.state = 16 * (decoder.state >> 12) + (slot as u64) - cumfreq;
                        while decoder.state < (1 << 24) && decoder.pos > 0 {
                            decoder.pos -= 1;
                            let val = decoder.input[decoder.pos] as u64;
                            decoder.state = (decoder.state << 8) | val;
                        }

                        self.update(sym);
                        return Ok(sym);
                    }
                } else {
                    let sym = sym as u8;
                    self.update(sym);
                    return Ok(sym);
                }
            } else {
                if current_order > 0 {
                    current_order -= 1;
                } else {
                    let mask = 4095;
                    let slot = (decoder.state & mask) as u32;
                    let sym = (slot / 16) as u8;
                    let cumfreq = sym as u64 * 16;
                    decoder.state = 16 * (decoder.state >> 12) + (slot as u64) - cumfreq;
                    while decoder.state < (1 << 24) && decoder.pos > 0 {
                        decoder.pos -= 1;
                        let val = decoder.input[decoder.pos] as u64;
                        decoder.state = (decoder.state << 8) | val;
                    }

                    self.update(sym);
                    return Ok(sym);
                }
            }
        }
    }

    /// Predicts probabilities for all 256 symbols in a legacy-compatible format.
    pub fn predict(&self, _symbol: u8) -> Vec<(u8, f32)> {
        let mut res = Vec::new();
        let mut highest_order = 0;
        for j in (0..=4).rev() {
            if self.active_contexts[j] != 0 {
                highest_order = j;
                break;
            }
        }
        let ctx_idx = self.active_contexts[highest_order];
        let scaled = self.get_scaled_context(ctx_idx);
        for (sym, s_info) in scaled.iter().enumerate().take(256) {
            let freq = s_info.freq;
            if freq > 0 {
                res.push((sym as u8, freq as f32 / 4096.0));
            }
        }
        if res.is_empty() {
            for sym in 0..256 {
                res.push((sym as u8, 1.0 / 256.0));
            }
        }
        res
    }

    /// Compatibility interface returning PpmEvent lists.
    pub fn encode_symbol(&mut self, symbol: u8) -> Vec<PpmEvent> {
        let mut events = Vec::new();
        let mut highest_order = 0;
        for j in (0..=4).rev() {
            if self.active_contexts[j] != 0 {
                highest_order = j;
                break;
            }
        }

        let mut found_order = None;
        for order in (0..=highest_order).rev() {
            let ctx_idx = self.active_contexts[order];
            if self.contexts[ctx_idx as usize].num_stats > 0 {
                let scaled = self.get_scaled_context(ctx_idx);
                if scaled[symbol as usize].freq > 0 {
                    found_order = Some(order);
                    break;
                }
            }
        }

        if let Some(target_order) = found_order {
            for order in (target_order + 1..=highest_order).rev() {
                let ctx_idx = self.active_contexts[order];
                if self.contexts[ctx_idx as usize].num_stats > 0 {
                    let scaled = self.get_scaled_context(ctx_idx);
                    let esc_info = scaled[256];
                    events.push(PpmEvent::Escape {
                        order,
                        escape_prob: esc_info.freq as f32 / 4096.0,
                    });
                }
            }
            let ctx_idx = self.active_contexts[target_order];
            let scaled = self.get_scaled_context(ctx_idx);
            let sym_info = scaled[symbol as usize];
            events.push(PpmEvent::Symbol {
                symbol,
                probability: sym_info.freq as f32 / 4096.0,
                cumulative: sym_info.cumfreq as f32 / 4096.0,
            });
        } else {
            for order in (0..=highest_order).rev() {
                let ctx_idx = self.active_contexts[order];
                if self.contexts[ctx_idx as usize].num_stats > 0 {
                    let scaled = self.get_scaled_context(ctx_idx);
                    let esc_info = scaled[256];
                    events.push(PpmEvent::Escape {
                        order,
                        escape_prob: esc_info.freq as f32 / 4096.0,
                    });
                }
            }
            events.push(PpmEvent::Symbol {
                symbol,
                probability: 16.0 / 4096.0,
                cumulative: symbol as f32 * 16.0 / 4096.0,
            });
        }

        self.update(symbol);
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ppm_new_symbol_gets_probability() {
        let model = PpmModel::new(1024);
        let scaled = model.get_scaled_context(0);
        assert_eq!(scaled[100].freq, 0); 
        assert_eq!(scaled[256].freq, 4096); 
    }

    #[test]
    fn test_ppm_seen_symbol_higher_probability() {
        let mut model = PpmModel::new(1024);
        model.update(42);
        let scaled = model.get_scaled_context(0);
        assert!(scaled[42].freq > 0);
        assert!(scaled[256].freq > 0); 
    }

    #[test]
    fn test_ppm_context_building() {
        let mut model = PpmModel::new(4096);
        model.update(b'a');
        model.update(b'b');
        model.update(b'c');
        model.update(b'd');

        let t1 = model.find_transition(0, b'a');
        assert!(t1 != 0);
        let t2 = model.find_transition(t1, b'b');
        assert!(t2 != 0);
        let t3 = model.find_transition(t2, b'c');
        assert!(t3 != 0);
    }

    #[test]
    fn test_context_model_roundtrip() {
        let data = b"the quick brown fox jumps over the lazy dog the quick brown fox";
        let mut model_enc = PpmModel::new(8192);
        let mut events = Vec::new();
        for &b in data {
            model_enc.encode_literal(b, &mut events);
        }

        let mut encoder = crate::compress::rans::RansEncoder::new();
        for ev in events.iter().rev() {
            encoder.encode_symbol_with_freq(ev.freq, ev.cumfreq);
        }
        let compressed = encoder.flush();

        let mut decoder = crate::compress::rans::RansDecoder::new(compressed);
        let mut model_dec = PpmModel::new(8192);
        let mut decoded = Vec::new();
        for _ in 0..data.len() {
            let sym = model_dec.decode_literal(&mut decoder).unwrap();
            decoded.push(sym);
        }

        assert_eq!(decoded, data);
    }

    #[test]
    fn test_arena_exhaustion() {
        let mut model_enc = PpmModel::new(128);
        let data = vec![b'a'; 1000];
        let mut events = Vec::new();
        for &b in &data {
            model_enc.encode_literal(b, &mut events);
        }

        let mut encoder = crate::compress::rans::RansEncoder::new();
        for ev in events.iter().rev() {
            encoder.encode_symbol_with_freq(ev.freq, ev.cumfreq);
        }
        let compressed = encoder.flush();

        let mut decoder = crate::compress::rans::RansDecoder::new(compressed);
        let mut model_dec = PpmModel::new(128);
        let mut decoded = Vec::new();
        for _ in 0..data.len() {
            let sym = model_dec.decode_literal(&mut decoder).unwrap();
            decoded.push(sym);
        }

        assert_eq!(decoded, data);
    }
}
