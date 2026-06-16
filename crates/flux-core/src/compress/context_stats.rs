//! Statistics gathering and cost evaluation for context-mapped
//! literal coding. See docs/v1_5_context_literals_design.md
//! Section 3 Pass 1 and Pass 2 (cost evaluation step).

use crate::compress::clustering::{Histogram, cluster_entropy_times_n, merge_histograms};
use crate::compress::context::{ContextMode, get_context_id};

/// Gathers context literal histograms for the given literal sequence and mode.
///
/// Returns a Vec of histograms where the index is the context ID.
/// - For `ContextMode::None`, returns a single-element Vec containing a combined histogram.
/// - For others, returns a Vec of length equal to `mode.map_size()`.
pub fn gather_context_stats(
    literals_with_context: &[(u8, u8)],
    mode: ContextMode,
) -> Vec<Histogram> {
    match mode {
        ContextMode::None => {
            let mut hist = [0u32; 256];
            for &(lit, _) in literals_with_context {
                hist[lit as usize] = hist[lit as usize].saturating_add(1);
            }
            vec![hist]
        }
        _ => {
            let size = mode.map_size();
            let mut histograms = vec![[0u32; 256]; size];
            for &(lit, prev) in literals_with_context {
                let context_id = get_context_id(mode, prev);
                if context_id < size {
                    histograms[context_id][lit as usize] =
                        histograms[context_id][lit as usize].saturating_add(1);
                }
            }
            histograms
        }
    }
}

/// Evaluates the coding cost (Shannon entropy) of a given clustering/mapping.
pub fn evaluate_clustering_cost(
    histograms: &[Histogram],
    context_map: &[u8],
    num_tables: usize,
) -> u64 {
    if num_tables == 0 {
        return 0;
    }

    let mut merged_tables = vec![[0u32; 256]; num_tables];

    // Sum histograms by their mapped table index.
    for (context_id, &table_idx) in context_map.iter().enumerate() {
        let t = table_idx as usize;
        if t < num_tables && context_id < histograms.len() {
            merged_tables[t] = merge_histograms(&merged_tables[t], &histograms[context_id]);
        }
    }

    let mut total_cost = 0u64;
    for merged in &merged_tables {
        total_cost = total_cost.saturating_add(cluster_entropy_times_n(merged));
    }

    total_cost
}

/// Computes the bit cost of encoding all literals using a single legacy frequency table.
///
/// Note: To remain unit-consistent with the relative overhead model in `decide_context_coding`
/// (which uses `(K - 1) * 512` bytes of table overhead to measure extra overhead above the baseline),
/// this function returns JUST the absolute Shannon entropy coding cost in fixed-point bits,
/// with no table overhead added.
#[allow(dead_code)]
pub(crate) fn legacy_single_table_cost(
    literals_with_context: &[(u8, u8)],
) -> u64 {
    let histograms = gather_context_stats(literals_with_context, ContextMode::None);
    cluster_entropy_times_n(&histograms[0])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gather_none_mode() {
        let input = vec![(b'A', 0), (b'B', 0), (b'A', 99)];
        let histograms = gather_context_stats(&input, ContextMode::None);
        assert_eq!(histograms.len(), 1);
        assert_eq!(histograms[0][b'A' as usize], 2);
        assert_eq!(histograms[0][b'B' as usize], 1);
        // Assert everything else is 0
        let mut expected = [0u32; 256];
        expected[b'A' as usize] = 2;
        expected[b'B' as usize] = 1;
        assert_eq!(histograms[0], expected);
    }

    #[test]
    fn test_gather_full_mode() {
        let input = vec![(b'X', 0x10), (b'Y', 0x10), (b'X', 0x20)];
        let histograms = gather_context_stats(&input, ContextMode::Full);
        assert_eq!(histograms.len(), 256);
        assert_eq!(histograms[0x10][b'X' as usize], 1);
        assert_eq!(histograms[0x10][b'Y' as usize], 1);
        assert_eq!(histograms[0x20][b'X' as usize], 1);
        
        // Check a random other one is zero
        assert_eq!(histograms[0x00][b'X' as usize], 0);
    }

    #[test]
    fn test_gather_msb6_mode() {
        let input = vec![(b'A', 0x80), (b'A', 0x83)];
        let histograms = gather_context_stats(&input, ContextMode::Msb6);
        assert_eq!(histograms.len(), 64);
        // (0x80 >> 2) == 0x20, (0x83 >> 2) == 0x20
        assert_eq!(histograms[0x20][b'A' as usize], 2);
    }

    #[test]
    fn test_gather_lsb6_mode() {
        // 0x40 and 0x80 both end in bottom 6 bits 0x00:
        // 0x40 & 0x3F = 0x00
        // 0x80 & 0x3F = 0x00
        let input = vec![(b'Z', 0x40), (b'Z', 0x80)];
        let histograms = gather_context_stats(&input, ContextMode::Lsb6);
        assert_eq!(histograms.len(), 64);
        assert_eq!(histograms[0x00][b'Z' as usize], 2);
    }

    #[test]
    fn test_gather_empty_input() {
        let histograms = gather_context_stats(&[], ContextMode::Full);
        assert_eq!(histograms.len(), 256);
        for h in &histograms {
            assert_eq!(*h, [0u32; 256]);
        }
    }

    #[test]
    fn test_evaluate_single_table_identity() {
        let mut histograms = vec![[0u32; 256]; 64];
        histograms[5][b'A' as usize] = 100;
        
        let context_map = vec![0u8; 64];
        let cost = evaluate_clustering_cost(&histograms, &context_map, 1);
        
        let mut expected_hist = [0u32; 256];
        expected_hist[b'A' as usize] = 100;
        let expected_cost = cluster_entropy_times_n(&expected_hist);
        
        assert_eq!(cost, expected_cost);
    }

    #[test]
    fn test_evaluate_perfect_separation() {
        // 4 histograms:
        // h0 and h1 only have byte 'A' (count 10 each)
        // h2 and h3 only have byte 'B' (count 10 each)
        let mut h_a = [0u32; 256];
        h_a[b'A' as usize] = 10;
        let mut h_b = [0u32; 256];
        h_b[b'B' as usize] = 10;

        let histograms = vec![h_a, h_a, h_b, h_b];

        // 1. Separated context map (K = 2)
        let context_map_sep = vec![0, 0, 1, 1];
        let cost_sep = evaluate_clustering_cost(&histograms, &context_map_sep, 2);

        // 2. Merged context map (K = 1)
        let context_map_merge = vec![0, 0, 0, 0];
        let cost_merge = evaluate_clustering_cost(&histograms, &context_map_merge, 1);

        // Cost of perfect separation must be less than merged cost
        // Because separated tables have 0 entropy (all identical symbols in each table),
        // whereas the merged table has a mix of 'A' and 'B' symbols (high entropy).
        assert!(cost_sep < cost_merge);
        assert_eq!(cost_sep, 0); // 0 entropy since each table gets only one kind of symbol
    }

    #[test]
    fn test_evaluate_determinism() {
        let mut histograms = vec![[0u32; 256]; 64];
        for (i, hist) in histograms.iter_mut().enumerate() {
            hist[i % 4] = (i + 1) as u32;
        }
        let mut context_map = vec![0u8; 64];
        for (i, val) in context_map.iter_mut().enumerate() {
            *val = (i % 3) as u8;
        }

        let cost1 = evaluate_clustering_cost(&histograms, &context_map, 3);
        let cost2 = evaluate_clustering_cost(&histograms, &context_map, 3);
        let cost3 = evaluate_clustering_cost(&histograms, &context_map, 3);

        assert_eq!(cost1, cost2);
        assert_eq!(cost2, cost3);
    }

    #[test]
    fn test_legacy_cost_determinism() {
        let mut input = Vec::new();
        for i in 0..100 {
            input.push(((i % 256) as u8, 0x10));
        }
        let cost1 = legacy_single_table_cost(&input);
        let cost2 = legacy_single_table_cost(&input);
        let cost3 = legacy_single_table_cost(&input);
        assert_eq!(cost1, cost2);
        assert_eq!(cost2, cost3);
    }

    #[test]
    fn test_legacy_cost_empty_input() {
        let cost = legacy_single_table_cost(&[]);
        assert_eq!(cost, 0);
    }

    #[test]
    fn test_legacy_cost_uniform() {
        // Uniform distribution of all 256 symbols appearing exactly once.
        let mut input = Vec::new();
        for i in 0..256 {
            input.push((i as u8, 0));
        }
        let cost = legacy_single_table_cost(&input);
        // Shannon entropy of uniform distribution over 256 symbols is exactly 8 bits per symbol.
        // In 16.16 fixed point: 256 symbols * 8 bits/symbol * 65536 = 134,217,728.
        assert_eq!(cost, 134_217_728);
    }
}
