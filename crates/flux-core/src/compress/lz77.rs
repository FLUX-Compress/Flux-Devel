//! LZ77 compression
//!
//! Implements sliding-window LZ77 match finding with hash chains and lazy matching.
//! LZ77 finds repeated byte sequences and replaces them with (length, distance)
//! back-references into a sliding window of historical data.

use std::collections::VecDeque;
use crate::transform::MediaFilterType;

/// Matches shorter than 4 bytes cost more to encode as a (length, distance) triplet
/// than they save, so 4 is the optimal minimum match length.
pub const MIN_MATCH_LEN: usize = 4;

/// 258 bytes is the standard maximum match length in many LZ77 implementations (like DEFLATE).
/// It fits within a single byte representation using a suitable offset mapping.
pub const MAX_MATCH_LEN: usize = 258;

/// A 32KB sliding window history balances compression ratio (capturing longer-range repetitions)
/// with search latency and memory footprint.
pub const WINDOW_SIZE: usize = 32768;

/// Hash table size = 1 << HASH_BITS (32768 entries). This balances hash collision rates
/// with memory footprint.
pub const HASH_BITS: usize = 15;

/// Only attempt lazy matching if the current match length is shorter than this threshold (6).
/// If we already have a long match, lazy lookahead search is unlikely to yield enough benefit
/// to justify the extra computation.
pub const LAZY_MATCH_THRESHOLD: usize = 6;

/// LZ77 Token representing either a literal byte or a back-reference.
///
/// A back-reference `Match { distance, length }` tells the decompressor to copy `length` bytes
/// from the history starting `distance` bytes back from the current write cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lz77Token {
    /// A single unmatched byte.
    Literal(u8),
    /// A back-reference to a previously seen sequence.
    Match {
        /// Distance back in the sliding window.
        distance: u32,
        /// Length of the matching sequence.
        length: u16,
    },
    /// A repeat-offset match.
    RepMatch {
        /// Cache index: 0, 1, or 2.
        index: u8,
        /// Length of the matching sequence.
        length: u16,
    },
}

/// LZ77 Match finder type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchFinder {
    /// Standard hash-chain match finder.
    HashChain,
    /// Binary search tree match finder.
    BinaryTree,
}

/// LZ77 Match candidate containing distance and length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lz77Match {
    /// Distance back in the sliding window.
    pub distance: u32,
    /// Length of the matching sequence.
    pub length: u16,
}

/// LZ77 Encoder using hash chains or binary tree and lazy matching.
pub struct Lz77Encoder {
    /// The sliding window history.
    pub window: VecDeque<u8>,
    /// Hash table mapping a hash to a position in the input data (or root of BST).
    pub hash_table: Vec<u32>,
    /// Hash chains mapping a position to a previous position with the same hash.
    pub hash_chains: Vec<u32>,
    /// Whether lazy matching is enabled.
    pub lazy_matching: bool,
    /// Maximum search depth in the hash chain / BST comparison limit.
    pub max_chain_depth: usize,
    /// Lazy match threshold (defer match if lookahead is better and current length is below this).
    pub lazy_match_threshold: usize,
    /// Early exit match length (stop searching if we find a match this long).
    pub good_match: usize,
    /// Cache of 3 most recent match distances (LRU).
    pub rep_offsets: [u32; 3],
    /// Map of minimum match lengths allowed per input position.
    pub min_match_map: Vec<u8>,
    /// Sliding window history size.
    pub window_size: usize,
    /// Hash bits used for the dynamic hash table.
    pub hash_bits: usize,
    /// Which match finder is selected.
    pub match_finder: MatchFinder,
    /// Left children in the suffix BST.
    pub bt_left: Vec<u32>,
    /// Right children in the suffix BST.
    pub bt_right: Vec<u32>,
}

/// Returns the number of hash table address bits based on the window size.
pub fn hash_bits_for_window(window_size: usize) -> usize {
    if window_size <= 256 * 1024 {
        15
    } else if window_size <= 4 * 1024 * 1024 {
        17
    } else if window_size <= 32 * 1024 * 1024 {
        19
    } else if window_size <= 128 * 1024 * 1024 {
        21
    } else {
        22
    }
}

impl Lz77Encoder {
    /// Creates a new LZ77 encoder with default Balanced parameters.
    pub fn new(lazy_matching: bool) -> Self {
        Self::new_with_params(
            32 * 1024 * 1024, // default 32MB window for Balanced
            lazy_matching,
            1024, // max_chain_depth
            128,  // lazy_match_threshold
            32,   // good_match
            MatchFinder::HashChain,
        )
    }

    /// Creates a new LZ77 encoder with specific performance parameters.
    pub fn new_with_params(
        window_size: usize,
        lazy_matching: bool,
        max_chain_depth: usize,
        lazy_match_threshold: usize,
        good_match: usize,
        match_finder: MatchFinder,
    ) -> Self {
        let hash_bits = hash_bits_for_window(window_size);
        Self {
            window: VecDeque::with_capacity(window_size),
            hash_table: vec![u32::MAX; 1 << hash_bits],
            hash_chains: vec![u32::MAX; window_size],
            lazy_matching,
            max_chain_depth,
            lazy_match_threshold,
            good_match,
            rep_offsets: [1, 4, 8],
            min_match_map: Vec::new(),
            window_size,
            hash_bits,
            match_finder,
            bt_left: Vec::new(),
            bt_right: Vec::new(),
        }
    }

    /// Encodes a block of data into a sequence of LZ77 tokens.
    ///
    /// If lazy matching is enabled, we perform a deferred evaluation:
    /// 1. Find the best match at the current position.
    /// 2. If the match length is less than `LAZY_MATCH_THRESHOLD`, look ahead one byte.
    /// 3. If the lookahead search yields a longer match, we emit the current byte as a literal
    ///    and use the lookahead match.
    ///
    /// Lazy matching consistently finds better matches because greedy matching can make a locally
    /// optimal choice that prevents a much larger match immediately following it (e.g. matching
    /// 4 bytes starting at `i` might break a 10-byte match starting at `i+1`).
    /// Returns the match length at a given history distance.
    pub fn get_match_len_at_dist(&self, data: &[u8], pos: usize, dist: usize) -> usize {
        if dist == 0 || dist > pos {
            return 0;
        }
        let match_pos = pos - dist;
        let max_len = (data.len() - pos).min(MAX_MATCH_LEN);
        let mut len = 0;
        while len < max_len && data[match_pos + len] == data[pos + len] {
            len += 1;
        }
        len
    }

    pub fn encode_with_map(&mut self, data: &[u8], map: &[u8]) -> Vec<Lz77Token> {
        self.min_match_map = map.to_vec();
        self.encode(data)
    }

    pub fn encode_with_media_filter(&mut self, data: &[u8], media_filter: &MediaFilterType) -> Vec<Lz77Token> {
        let map = build_min_match_map(media_filter, data.len());
        self.encode_with_map(data, &map)
    }

    pub fn min_match_at(&self, pos: usize) -> usize {
        if pos < self.min_match_map.len() {
            self.min_match_map[pos] as usize
        } else {
            4
        }
    }

    /// Finds the best match (either normal match or repcode match) at the given position.
    pub fn find_best_match_with_repcodes(&self, data: &[u8], pos: usize) -> Option<Lz77Token> {
        let chain_match = self.find_best_match(data, pos);
        
        let min_match = self.min_match_at(pos);
        let mut best_rep_idx = None;
        let mut best_rep_len = 0;
        for i in 0..3 {
            let dist = self.rep_offsets[i] as usize;
            let len = self.get_match_len_at_dist(data, pos, dist);
            if len >= min_match && len > best_rep_len {
                best_rep_len = len;
                best_rep_idx = Some(i as u8);
            }
        }
        
        match (chain_match, best_rep_idx) {
            (Some((c_dist, c_len)), Some(r_idx)) => {
                if best_rep_len >= c_len {
                    Some(Lz77Token::RepMatch { index: r_idx, length: best_rep_len as u16 })
                } else {
                    // Check if c_dist is actually one of the rep offsets
                    if c_dist == self.rep_offsets[0] as usize {
                        Some(Lz77Token::RepMatch { index: 0, length: c_len as u16 })
                    } else if c_dist == self.rep_offsets[1] as usize {
                        Some(Lz77Token::RepMatch { index: 1, length: c_len as u16 })
                    } else if c_dist == self.rep_offsets[2] as usize {
                        Some(Lz77Token::RepMatch { index: 2, length: c_len as u16 })
                    } else {
                        Some(Lz77Token::Match { distance: c_dist as u32, length: c_len as u16 })
                    }
                }
            }
            (Some((c_dist, c_len)), None) => {
                if c_dist == self.rep_offsets[0] as usize {
                    Some(Lz77Token::RepMatch { index: 0, length: c_len as u16 })
                } else if c_dist == self.rep_offsets[1] as usize {
                    Some(Lz77Token::RepMatch { index: 1, length: c_len as u16 })
                } else if c_dist == self.rep_offsets[2] as usize {
                    Some(Lz77Token::RepMatch { index: 2, length: c_len as u16 })
                } else {
                    Some(Lz77Token::Match { distance: c_dist as u32, length: c_len as u16 })
                }
            }
            (None, Some(r_idx)) => {
                Some(Lz77Token::RepMatch { index: r_idx, length: best_rep_len as u16 })
            }
            (None, None) => None,
        }
    }

    pub fn encode(&mut self, data: &[u8]) -> Vec<Lz77Token> {
        if self.min_match_map.len() != data.len() {
            self.min_match_map.clear();
        }
        self.window.clear();
        self.hash_table.fill(u32::MAX);
        self.rep_offsets = [1, 4, 8];
        
        if self.hash_chains.len() < data.len() {
            self.hash_chains.resize(data.len(), u32::MAX);
        }
        self.hash_chains[..data.len()].fill(u32::MAX);

        if self.match_finder == MatchFinder::BinaryTree {
            if self.bt_left.len() < data.len() {
                self.bt_left.resize(data.len(), u32::MAX);
                self.bt_right.resize(data.len(), u32::MAX);
            }
            self.bt_left[..data.len()].fill(u32::MAX);
            self.bt_right[..data.len()].fill(u32::MAX);
        }

        let mut tokens = Vec::new();
        let mut pos = 0;

        while pos < data.len() {
            let current_match = self.find_best_match_with_repcodes(data, pos);

            if self.lazy_matching {
                if let Some(token) = current_match {
                    let len = match token {
                        Lz77Token::Match { length, .. } => length as usize,
                        Lz77Token::RepMatch { length, .. } => length as usize,
                        _ => 0,
                    };
                    if len < self.lazy_match_threshold && pos + 1 < data.len() {
                        // Update hash for current byte to include it in the lookup chain
                        self.update_hash(data, pos);

                        // Look ahead one byte
                        let lookahead_match = self.find_best_match_with_repcodes(data, pos + 1);
                        if let Some(la_token) = lookahead_match {
                            let la_len = match la_token {
                                Lz77Token::Match { length, .. } => length as usize,
                                Lz77Token::RepMatch { length, .. } => length as usize,
                                _ => 0,
                            };
                            if la_len > len {
                                // The lookahead match is better! Emit current byte as literal.
                                tokens.push(Lz77Token::Literal(data[pos]));
                                self.window.push_back(data[pos]);
                                if self.window.len() > self.window_size {
                                    self.window.pop_front();
                                }
                                pos += 1;
                                continue;
                            }
                        }
                    }
                }
            }

            // Emit either a match or a literal
            if let Some(token) = current_match {
                // Update repcode cache
                match token {
                    Lz77Token::Match { distance, .. } => {
                        let d = distance;
                        self.rep_offsets[2] = self.rep_offsets[1];
                        self.rep_offsets[1] = self.rep_offsets[0];
                        self.rep_offsets[0] = d;
                    }
                    Lz77Token::RepMatch { index, .. } => {
                        let idx = index as usize;
                        let d = self.rep_offsets[idx];
                        if idx == 1 {
                            let tmp = self.rep_offsets[0];
                            self.rep_offsets[0] = d;
                            self.rep_offsets[1] = tmp;
                        } else if idx == 2 {
                            let tmp0 = self.rep_offsets[0];
                            let tmp1 = self.rep_offsets[1];
                            self.rep_offsets[0] = d;
                            self.rep_offsets[1] = tmp0;
                            self.rep_offsets[2] = tmp1;
                        }
                    }
                    _ => {}
                }

                tokens.push(token);

                let len = match token {
                    Lz77Token::Match { length, .. } => length as usize,
                    Lz77Token::RepMatch { length, .. } => length as usize,
                    _ => unreachable!(),
                };

                // Update hash table, chains, and window history for all bytes in the match
                for i in 0..len {
                    let p = pos + i;
                    self.update_hash(data, p);
                    self.window.push_back(data[p]);
                    if self.window.len() > self.window_size {
                        self.window.pop_front();
                    }
                }
                pos += len;
            } else {
                tokens.push(Lz77Token::Literal(data[pos]));
                self.update_hash(data, pos);
                self.window.push_back(data[pos]);
                if self.window.len() > self.window_size {
                    self.window.pop_front();
                }
                pos += 1;
            }
        }

        tokens
    }

    /// Finds the best match at the given position in the data block.
    ///
    /// Traverses the hash chain starting from the head in `hash_table`.
    /// Limits search depth to `self.max_chain_depth` to control compression ratio and speed.
    pub fn find_best_match(&self, data: &[u8], pos: usize) -> Option<(usize, usize)> {
        if self.match_finder == MatchFinder::BinaryTree {
            self.find_best_match_bt(data, pos)
        } else {
            let min_match = self.min_match_at(pos);
            if pos + min_match > data.len() {
                return None;
            }

            let h = if min_match == 3 {
                self.hash3(data, pos)
            } else {
                self.hash4(data, pos)
            };
            let mut chain_head = self.hash_table[h];
            let mut best_len = 0;
            let mut best_dist = 0;
            let mut depth = 0;

            while chain_head != u32::MAX && depth < self.max_chain_depth {
                let match_pos = chain_head as usize;

                if match_pos >= pos || pos - match_pos > self.window_size {
                    break;
                }

                // Measure match length
                let max_len = (data.len() - pos).min(MAX_MATCH_LEN);
                let mut len = 0;
                while len < max_len && data[match_pos + len] == data[pos + len] {
                    len += 1;
                }

                if len >= min_match && len > best_len {
                    best_len = len;
                    best_dist = pos - match_pos;
                    if len >= self.good_match || len >= MAX_MATCH_LEN {
                        break;
                    }
                }

                chain_head = self.hash_chains[match_pos];
                depth += 1;
            }

            if best_len >= min_match {
                Some((best_dist, best_len))
            } else {
                None
            }
        }
    }

    /// Finds the best match in the suffix BST.
    pub fn find_best_match_bt(&self, data: &[u8], pos: usize) -> Option<(usize, usize)> {
        let min_match = self.min_match_at(pos);
        if pos + min_match > data.len() {
            return None;
        }

        let h = if min_match == 3 {
            self.hash3(data, pos)
        } else {
            self.hash4(data, pos)
        };
        let mut curr = self.hash_table[h];
        let mut best_len = 0;
        let mut best_dist = 0;
        let mut depth = 0;

        let max_compare = self.max_chain_depth;
        let target_len = self.good_match;
        let max_len = (data.len() - pos).min(MAX_MATCH_LEN);

        while curr != u32::MAX && depth < max_compare {
            let match_pos = curr as usize;

            if match_pos >= pos || pos - match_pos > self.window_size {
                break;
            }

            // Measure match length
            let mut len = 0;
            while len < max_len && data[match_pos + len] == data[pos + len] {
                len += 1;
            }

            if len >= min_match && len > best_len {
                best_len = len;
                best_dist = pos - match_pos;
                if len >= target_len || len >= MAX_MATCH_LEN {
                    break;
                }
            }

            if pos + len == data.len() && match_pos + len == data.len() {
                break;
            } else if pos + len == data.len() {
                curr = self.bt_left[match_pos];
            } else if match_pos + len == data.len() {
                curr = self.bt_right[match_pos];
            } else if data[pos + len] < data[match_pos + len] {
                curr = self.bt_left[match_pos];
            } else {
                curr = self.bt_right[match_pos];
            }
            depth += 1;
        }

        if best_len >= min_match {
            Some((best_dist, best_len))
        } else {
            None
        }
    }

    /// Finds all matches in the suffix BST (ordered by increasing lengths).
    pub fn find_all_matches(&self, data: &[u8], pos: usize) -> Vec<Lz77Match> {
        let min_match = self.min_match_at(pos);
        if pos + min_match > data.len() {
            return Vec::new();
        }

        let h = if min_match == 3 {
            self.hash3(data, pos)
        } else {
            self.hash4(data, pos)
        };
        let mut curr = self.hash_table[h];
        let mut matches = Vec::new();
        let mut depth = 0;
        let mut best_len = 0;

        let max_compare = self.max_chain_depth;
        let target_len = self.good_match;
        let max_len = (data.len() - pos).min(MAX_MATCH_LEN);

        while curr != u32::MAX && depth < max_compare {
            let match_pos = curr as usize;

            if match_pos >= pos || pos - match_pos > self.window_size {
                break;
            }

            let mut len = 0;
            while len < max_len && data[match_pos + len] == data[pos + len] {
                len += 1;
            }

            if len >= min_match && len > best_len {
                best_len = len;
                matches.push(Lz77Match {
                    distance: (pos - match_pos) as u32,
                    length: len as u16,
                });
                if len >= target_len || len >= MAX_MATCH_LEN {
                    break;
                }
            }

            if pos + len == data.len() && match_pos + len == data.len() {
                break;
            } else if pos + len == data.len() {
                curr = self.bt_left[match_pos];
            } else if match_pos + len == data.len() {
                curr = self.bt_right[match_pos];
            } else if data[pos + len] < data[match_pos + len] {
                curr = self.bt_left[match_pos];
            } else {
                curr = self.bt_right[match_pos];
            }
            depth += 1;
        }

        matches
    }

    /// Inserts the node `pos` into the suffix BST and splits the tree.
    pub fn insert_node_bt(&mut self, data: &[u8], pos: usize) {
        let min_match = self.min_match_at(pos);
        if pos + min_match > data.len() {
            return;
        }

        let h = if min_match == 3 {
            self.hash3(data, pos)
        } else {
            self.hash4(data, pos)
        };

        let mut curr = self.hash_table[h];
        self.hash_table[h] = pos as u32;

        self.bt_left[pos] = u32::MAX;
        self.bt_right[pos] = u32::MAX;

        let mut left_parent = pos;
        let mut left_is_left = true;
        
        let mut right_parent = pos;
        let mut right_is_left = false;

        let mut depth = 0;
        let max_compare = self.max_chain_depth;
        let target_len = self.good_match;
        let max_len = (data.len() - pos).min(MAX_MATCH_LEN);

        while curr != u32::MAX && depth < max_compare {
            let match_pos = curr as usize;

            if match_pos >= pos || pos - match_pos > self.window_size {
                break;
            }

            let mut len = 0;
            while len < max_len && data[match_pos + len] == data[pos + len] {
                len += 1;
            }

            if len >= target_len || len >= MAX_MATCH_LEN {
                if left_is_left {
                    self.bt_left[left_parent] = self.bt_left[match_pos];
                } else {
                    self.bt_right[left_parent] = self.bt_left[match_pos];
                }
                if right_is_left {
                    self.bt_left[right_parent] = self.bt_right[match_pos];
                } else {
                    self.bt_right[right_parent] = self.bt_right[match_pos];
                }
                return;
            }

            if pos + len == data.len() && match_pos + len == data.len() {
                if left_is_left {
                    self.bt_left[left_parent] = self.bt_left[match_pos];
                } else {
                    self.bt_right[left_parent] = self.bt_left[match_pos];
                }
                if right_is_left {
                    self.bt_left[right_parent] = self.bt_right[match_pos];
                } else {
                    self.bt_right[right_parent] = self.bt_right[match_pos];
                }
                return;
            } else if pos + len == data.len() {
                if right_is_left {
                    self.bt_left[right_parent] = curr;
                } else {
                    self.bt_right[right_parent] = curr;
                }
                right_parent = match_pos;
                right_is_left = true;
                curr = self.bt_left[match_pos];
            } else if match_pos + len == data.len() {
                if left_is_left {
                    self.bt_left[left_parent] = curr;
                } else {
                    self.bt_right[left_parent] = curr;
                }
                left_parent = match_pos;
                left_is_left = false;
                curr = self.bt_right[match_pos];
            } else if data[pos + len] < data[match_pos + len] {
                if right_is_left {
                    self.bt_left[right_parent] = curr;
                } else {
                    self.bt_right[right_parent] = curr;
                }
                right_parent = match_pos;
                right_is_left = true;
                curr = self.bt_left[match_pos];
            } else {
                if left_is_left {
                    self.bt_left[left_parent] = curr;
                } else {
                    self.bt_right[left_parent] = curr;
                }
                left_parent = match_pos;
                left_is_left = false;
                curr = self.bt_right[match_pos];
            }
            depth += 1;
        }

        if left_is_left {
            self.bt_left[left_parent] = u32::MAX;
        } else {
            self.bt_right[left_parent] = u32::MAX;
        }
        if right_is_left {
            self.bt_left[right_parent] = u32::MAX;
        } else {
            self.bt_right[right_parent] = u32::MAX;
        }
    }

    /// Updates the hash table and chain or suffix BST for the sequence starting at `pos`.
    pub fn update_hash(&mut self, data: &[u8], pos: usize) {
        if self.match_finder == MatchFinder::BinaryTree {
            self.insert_node_bt(data, pos);
        } else {
            let min_match = self.min_match_at(pos);
            if pos + min_match > data.len() {
                return;
            }
            let h = if min_match == 3 {
                self.hash3(data, pos)
            } else {
                self.hash4(data, pos)
            };
            let prev = self.hash_table[h];
            self.hash_chains[pos] = prev;
            self.hash_table[h] = pos as u32;
        }
    }

    /// Computes a multiplicative hash value for 3 bytes starting at `pos`.
    pub fn hash3(&self, data: &[u8], pos: usize) -> usize {
        if pos + 2 >= data.len() {
            return 0;
        }
        let val = ((data[pos] as u32) << 16)
            | ((data[pos + 1] as u32) << 8)
            | (data[pos + 2] as u32);
        let h = val.wrapping_mul(50683);
        (h >> (32 - self.hash_bits)) as usize
    }

    /// Computes a multiplicative hash value for 4 bytes starting at `pos`.
    ///
    /// Multiplies the combined 32-bit value by a large prime constant (50683)
    /// and shifts the result to fit in self.hash_bits.
    pub fn hash4(&self, data: &[u8], pos: usize) -> usize {
        if pos + 3 >= data.len() {
            return 0;
        }
        let val = ((data[pos] as u32) << 24)
            | ((data[pos + 1] as u32) << 16)
            | ((data[pos + 2] as u32) << 8)
            | (data[pos + 3] as u32);
        let h = val.wrapping_mul(50683);
        (h >> (32 - self.hash_bits)) as usize
    }
}

/// LZ77 Decoder.
pub struct Lz77Decoder;

impl Lz77Decoder {
    /// Creates a new LZ77 decoder.
    pub fn new() -> Self {
        Self
    }

    /// Reconstructs the original data from a stream of LZ77 tokens.
    ///
    /// Handles overlapping matches (where match distance < match length) by copying byte-by-byte.
    /// Overlapping matches are intentional and valid (e.g. copying "a" with distance 1 and length 5
    /// yields "aaaaaa"), allowing efficient run-length encoding.
    pub fn decode(&mut self, tokens: &[Lz77Token]) -> Vec<u8> {
        let mut decoded = Vec::new();
        let mut rep_offsets = [1, 4, 8];

        for token in tokens {
            match token {
                Lz77Token::Literal(b) => {
                    decoded.push(*b);
                }
                Lz77Token::Match { distance, length } => {
                    let dist = *distance as usize;
                    let len = *length as usize;

                    // Ensure distance is valid (not referring to before the start of the decoded buffer)
                    if dist > 0 && dist <= decoded.len() {
                        for _ in 0..len {
                            let src_idx = decoded.len() - dist;
                            let b = decoded[src_idx];
                            decoded.push(b);
                        }
                    }

                    // Update rep offsets
                    let d = *distance;
                    rep_offsets[2] = rep_offsets[1];
                    rep_offsets[1] = rep_offsets[0];
                    rep_offsets[0] = d;
                }
                Lz77Token::RepMatch { index, length } => {
                    let idx = *index as usize;
                    let dist = rep_offsets[idx] as usize;
                    let len = *length as usize;

                    if dist > 0 && dist <= decoded.len() {
                        for _ in 0..len {
                            let src_idx = decoded.len() - dist;
                            let b = decoded[src_idx];
                            decoded.push(b);
                        }
                    }

                    // Update rep offsets
                    let d = rep_offsets[idx];
                    if idx == 1 {
                        let tmp = rep_offsets[0];
                        rep_offsets[0] = d;
                        rep_offsets[1] = tmp;
                    } else if idx == 2 {
                        let tmp0 = rep_offsets[0];
                        let tmp1 = rep_offsets[1];
                        rep_offsets[0] = d;
                        rep_offsets[1] = tmp0;
                        rep_offsets[2] = tmp1;
                    }
                }
            }
        }

        decoded
    }
}

impl Default for Lz77Decoder {
    fn default() -> Self {
        Self::new()
    }
}

/// Calculates the base distance of a given slot.
pub fn slot_base(slot: u8) -> u32 {
    if slot <= 3 {
        return (slot + 1) as u32;
    }
    let s = slot as u32;
    let exponent = s / 2;
    if s.is_multiple_of(2) {
        (1 << exponent) + 1
    } else {
        let prev_base = (1 << exponent) + 1;
        prev_base + (1 << (exponent - 1))
    }
}

/// Maps a distance to a distance slot (0..=55) and returns (slot, extra_val, extra_bits_count)
#[allow(clippy::manual_is_multiple_of)]
pub fn distance_to_slot(dist: u32) -> (u8, u32, u8) {
    if dist <= 4 {
        let d = dist.max(1);
        return (d as u8 - 1, 0, 0);
    }
    let mut slot = 4;
    loop {
        let base = slot_base(slot);
        let extra_bits = (slot - 2) / 2;
        let next_base = base + (1 << extra_bits);
        
        if dist >= base && dist < next_base {
            let extra_val = dist - base;
            return (slot, extra_val, extra_bits);
        }
        slot += 1;
        if slot >= 56 {
            let base_55 = slot_base(55);
            return (55, dist.saturating_sub(base_55), 26);
        }
    }
}

/// Reconstructs distance from a slot and extra value
#[allow(clippy::manual_is_multiple_of)]
pub fn slot_to_distance(slot: u8, extra_val: u32) -> u32 {
    if slot <= 3 {
        return (slot + 1) as u32;
    }
    let base = slot_base(slot);
    base + extra_val
}

pub fn build_min_match_map(media_filter: &MediaFilterType, data_len: usize) -> Vec<u8> {
    let mut map = vec![4u8; data_len];
    if let MediaFilterType::FloatChannelSplit { channels, mask } = media_filter {
        let channels = *channels as usize;
        let n = data_len / (4 * channels);
        if n > 0 {
            let plane_size = n * channels;
            for plane_idx in 0..4 {
                // If min_match_per_plane bit is 1, min_match is 3
                let min_match = if (mask & (1 << (plane_idx + 4))) != 0 { 3 } else { 4 };
                let start = (3 - plane_idx) * plane_size;
                let end = start + plane_size;
                map[start..end].fill(min_match);
            }
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lz77_literal_only() {
        // Incompressible random-like unique sequence
        let data = vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
        let mut encoder = Lz77Encoder::new(false);
        let tokens = encoder.encode(&data);

        for token in &tokens {
            assert!(matches!(token, Lz77Token::Literal(_)));
        }

        let mut decoder = Lz77Decoder::new();
        let decoded = decoder.decode(&tokens);
        assert_eq!(data, decoded);
    }

    #[test]
    fn test_lz77_repeated_sequence() {
        // Detects simple repetition
        let part = b"FLUX-compression-";
        let mut data = Vec::new();
        for _ in 0..10 {
            data.extend_from_slice(part);
        }

        let mut encoder = Lz77Encoder::new(false);
        let tokens = encoder.encode(&data);

        // Verify matches are found
        let match_count = tokens.iter().filter(|t| matches!(t, Lz77Token::Match { .. } | Lz77Token::RepMatch { .. })).count();
        assert!(match_count > 0);

        let mut decoder = Lz77Decoder::new();
        let decoded = decoder.decode(&tokens);
        assert_eq!(data, decoded);
    }

    #[test]
    fn test_lz77_roundtrip_short() {
        let data = b"hello hello hello";
        let mut encoder = Lz77Encoder::new(true);
        let tokens = encoder.encode(data);

        let mut decoder = Lz77Decoder::new();
        let decoded = decoder.decode(&tokens);
        assert_eq!(data.to_vec(), decoded);
    }

    #[test]
    fn test_lz77_roundtrip_longer() {
        // Use a paragraph of repeated text
        let data = b"Prediction by Partial Matching (PPM) variant I (PPM-I) \
                     with adaptive order up to MAX_ORDER. PPM builds a statistical model \
                     of the data and predicts the probability of each next symbol. \
                     PPM builds a statistical model and predicts the probability.";
                     
        let mut encoder = Lz77Encoder::new(true);
        let tokens = encoder.encode(data);

        let mut decoder = Lz77Decoder::new();
        let decoded = decoder.decode(&tokens);
        assert_eq!(data.to_vec(), decoded);
    }

    #[test]
    fn test_lz77_lazy_matching_finds_better_match() {
        // At 0: "bcdefgh" (length 7)
        // At 7: "_" (literal)
        // At 8: "abcd" (length 4)
        // At 12: "_" (literal)
        // At 13: "abcdbcdefgh"
        // At index 13, "abcd" matches at index 8 (len 4).
        // Since len 4 < LAZY_MATCH_THRESHOLD (6), we look ahead to index 14.
        // At index 14, "bcdefgh" matches at index 0 (len 7).
        // Since 7 > 4, lazy matching should emit 'a' as a literal, and match "bcdefgh".
        let data = b"bcdefgh_abcd_abcdbcdefgh";
        
        let mut encoder_lazy = Lz77Encoder::new(true);
        let tokens_lazy = encoder_lazy.encode(data);

        // Find the match for "bcdefgh" (len 7) in the lazy output
        let has_long_match = tokens_lazy.iter().any(|t| match t {
            Lz77Token::Match { length, .. } => *length >= 7,
            Lz77Token::RepMatch { length, .. } => *length >= 7,
            _ => false,
        });
        assert!(has_long_match);

        let mut decoder = Lz77Decoder::new();
        let decoded = decoder.decode(&tokens_lazy);
        assert_eq!(data.to_vec(), decoded);
    }

    #[test]
    fn test_lz77_overlapping_match() {
        // "aaaaaa" -> literal 'a' followed by match with dist 1, len 5
        let data = b"aaaaaa";
        let mut encoder = Lz77Encoder::new(false);
        let tokens = encoder.encode(data);

        assert_eq!(tokens[0], Lz77Token::Literal(b'a'));
        assert_eq!(tokens[1], Lz77Token::RepMatch { index: 0, length: 5 });

        let mut decoder = Lz77Decoder::new();
        let decoded = decoder.decode(&tokens);
        assert_eq!(data.to_vec(), decoded);
    }

    #[test]
    fn test_lz77_min_match_len() {
        // Matches below MIN_MATCH_LEN (4) become literals
        let data = b"abcxyzabc"; // "abc" matches at 6 but length is 3 (less than MIN_MATCH_LEN)
        let mut encoder = Lz77Encoder::new(false);
        let tokens = encoder.encode(data);

        for token in &tokens {
            assert!(matches!(token, Lz77Token::Literal(_)));
        }

        let mut decoder = Lz77Decoder::new();
        let decoded = decoder.decode(&tokens);
        assert_eq!(data.to_vec(), decoded);
    }

    #[test]
    fn test_repcode_roundtrip() {
        // Simple repeating pattern that triggers repcode matches at distance 14
        let data = b"abcd_1234_XYZ_abcd_1234_123_abcd_1234";
        let mut encoder = Lz77Encoder::new(true);
        let tokens = encoder.encode(data);

        // Verify there is at least one RepMatch token
        let rep_match_count = tokens.iter().filter(|t| matches!(t, Lz77Token::RepMatch { .. })).count();
        assert!(rep_match_count > 0, "No repcode matches found, check cache updates");

        let mut decoder = Lz77Decoder::new();
        let decoded = decoder.decode(&tokens);
        assert_eq!(data.to_vec(), decoded);
    }

    #[test]
    fn test_distance_slot_roundtrip_all() {
        let test_distances = [
            1, 2, 3, 4, 5, 8, 16, 32, 64, 128, 1024, 65536, 1048576, 16777216,
            134217728, 268435456
        ];
        for &dist in &test_distances {
            let (slot, extra, _extra_bits) = distance_to_slot(dist);
            let reconstructed = slot_to_distance(slot, extra);
            assert_eq!(reconstructed, dist, "Mismatch for distance {}", dist);
            assert!(slot <= 55, "Slot {} exceeded 55 for distance {}", slot, dist);
        }
    }

    fn brute_force_longest_match(data: &[u8], pos: usize, window_size: usize, min_match: usize) -> Option<(usize, usize)> {
        if pos + min_match > data.len() {
            return None;
        }
        let mut best_len = 0;
        let mut best_dist = 0;
        let max_len = (data.len() - pos).min(MAX_MATCH_LEN);
        let start = pos.saturating_sub(window_size);
        for match_pos in start..pos {
            let mut len = 0;
            while len < max_len && data[match_pos + len] == data[pos + len] {
                len += 1;
            }
            if len >= min_match && len > best_len {
                best_len = len;
                best_dist = pos - match_pos;
            }
        }
        if best_len >= min_match {
            Some((best_dist, best_len))
        } else {
            None
        }
    }

    #[test]
    fn test_bst_correctness_against_brute_force() {
        let mut data = Vec::new();
        let pattern1 = b"abcdefg";
        let pattern2 = b"xyz123";
        for i in 0..100 {
            if i % 3 == 0 {
                data.extend_from_slice(pattern1);
            } else if i % 3 == 1 {
                data.extend_from_slice(pattern2);
            } else {
                data.extend_from_slice(b"something_else");
            }
        }

        let mut encoder = Lz77Encoder::new_with_params(
            32 * 1024,
            false,
            4096,
            258,
            258,
            MatchFinder::BinaryTree,
        );

        encoder.bt_left = vec![u32::MAX; data.len()];
        encoder.bt_right = vec![u32::MAX; data.len()];

        let midpoint = data.len() / 2;
        for i in 0..midpoint {
            encoder.update_hash(&data, i);
        }

        for pos in midpoint..(data.len() - 10) {
            let bst_match = encoder.find_best_match(&data, pos);
            let bf_match = brute_force_longest_match(&data, pos, encoder.window_size, encoder.min_match_at(pos));
            
            if let (Some((_bst_dist, bst_len)), Some((_bf_dist, bf_len))) = (bst_match, bf_match) {
                assert_eq!(bst_len, bf_len, "Length mismatch at pos {}", pos);
            } else {
                assert_eq!(bst_match, bf_match, "Mismatch at pos {}", pos);
            }

            let all_matches = encoder.find_all_matches(&data, pos);
            if let Some((_best_dist, best_len)) = bst_match {
                assert!(!all_matches.is_empty(), "find_all_matches returned empty but best match exists");
                let has_len = all_matches.iter().any(|m| m.length == best_len as u16);
                assert!(has_len, "find_all_matches didn't contain the best match length at pos {}", pos);
            }

            encoder.update_hash(&data, pos);
        }
    }

    #[test]
    fn test_bst_edge_cases() {
        let data = b"a".repeat(1000);
        let mut encoder = Lz77Encoder::new_with_params(
            32 * 1024,
            false,
            4096,
            258,
            258,
            MatchFinder::BinaryTree,
        );
        encoder.bt_left = vec![u32::MAX; data.len()];
        encoder.bt_right = vec![u32::MAX; data.len()];

        for i in 0..data.len() {
            let bst_match = encoder.find_best_match(&data, i);
            if i >= 4 && i + 4 <= data.len() {
                assert!(bst_match.is_some(), "Match should exist at i = {}", i);
                let (dist, len) = bst_match.unwrap();
                assert_eq!(dist, 1);
                assert_eq!(len, (data.len() - i).min(MAX_MATCH_LEN));
            }
            encoder.update_hash(&data, i);
        }

        let data = b"abcde_abcde";
        let mut encoder = Lz77Encoder::new_with_params(
            32 * 1024,
            false,
            4096,
            258,
            258,
            MatchFinder::BinaryTree,
        );
        encoder.bt_left = vec![u32::MAX; data.len()];
        encoder.bt_right = vec![u32::MAX; data.len()];

        assert_eq!(encoder.find_best_match(data, 0), None);
        encoder.update_hash(data, 0);

        for i in 1..6 {
            encoder.update_hash(data, i);
        }
        assert_eq!(encoder.find_best_match(data, 6), Some((6, 5)));
    }
}

