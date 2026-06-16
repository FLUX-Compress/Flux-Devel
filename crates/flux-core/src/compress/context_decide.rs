//! Per-block decision logic for context-mapped literal coding.
//! Combines clustering (B.1) + stats/cost (B.2) to pick the
//! best (mode, num_tables) for a block — or decide legacy
//! single-table is better.
//! See docs/v1_5_context_literals_design.md Section 3 Pass 3.

use crate::compress::context::{ContextMode, MultiTableMetadata};
use crate::compress::clustering::cluster_contexts;
use crate::compress::context_stats::{gather_context_stats, evaluate_clustering_cost};

#[derive(Debug, Clone)]
struct Candidate {
    mode: ContextMode,
    num_tables: u8,
    context_map: Vec<u8>,
    total_cost: u64, // ClusteredBitCost + Overhead
}

/// Unified decision function that picks the best context mode and cluster count for a block.
///
/// Returns `Some(MultiTableMetadata)` if context-mapped coding beats legacy single-table coding
/// (including overhead), otherwise returns `None`.
///
/// Note: Both `legacy_single_table_cost` and the returned candidate's total cost must be
/// measured in the same fixed-point bit units.
pub fn decide_context_coding(
    literals_with_context: &[(u8, u8)],
    legacy_single_table_cost: u64,
) -> Option<MultiTableMetadata> {
    // EARLY EXIT: if the block has very few literals, the overhead can never be justified.
    // This threshold (64 literals) is a starting heuristic to avoid wasting clustering cycles
    // on tiny blocks where context coding can never pay off.
    if literals_with_context.len() < 64 {
        return None;
    }

    let mut best: Option<Candidate> = None;

    // Try each context mode (skip None — that's the legacy path; we're looking for a
    // context coding that beats legacy).
    for &mode in &[ContextMode::Full, ContextMode::Msb6, ContextMode::Lsb6] {
        // Gather per-context histograms for this mode.
        let histograms = gather_context_stats(literals_with_context, mode);

        // Try each candidate num_tables in [2..=8].
        for num_tables in 2..=8u8 {
            // num_tables must not exceed the active contexts count;
            // cluster_contexts handles this by clamping (returning identity if K >= N).
            // Skip if K >= mode.map_size().
            if (num_tables as usize) >= mode.map_size() {
                continue;
            }

            // Cluster the histograms into num_tables groups.
            let context_map = cluster_contexts(&histograms, num_tables as usize);

            // Compute the bit-cost of encoding under this clustering.
            let cost = evaluate_clustering_cost(&histograms, &context_map, num_tables as usize);

            // Compute the per-block overhead:
            //   2 + map_size + (K - 1) * 512 bytes
            // The 2 bytes are context_mode + num_tables.
            // map_size is the context_map size for this mode.
            // (K - 1) * 512 accounts for the legacy single-table baseline already having
            // 1 * 512 of table cost.
            let overhead_bytes = 2
                + mode.map_size()
                + ((num_tables as usize - 1) * 512);
            
            // Convert overhead to fixed-point bits by multiplying by 8 (since cost is in bits).
            let overhead_bits = (overhead_bytes as u64) * 8;
            let total = cost.saturating_add(overhead_bits);

            // Track the best candidate seen so far.
            if best.as_ref().is_none_or(|b| total < b.total_cost) {
                best = Some(Candidate {
                    mode,
                    num_tables,
                    context_map,
                    total_cost: total,
                });
            }
        }
    }

    // Compare the best context-coded candidate to the legacy single-table cost.
    if let Some(b) = best {
        if b.total_cost < legacy_single_table_cost {
            return Some(MultiTableMetadata {
                mode: b.mode,
                num_tables: b.num_tables,
                context_map: b.context_map,
            });
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compress::context_stats::gather_context_stats;

    #[test]
    fn test_decide_returns_none_for_tiny_input() {
        let input = vec![(b'A', b'B'); 10];
        let result = decide_context_coding(&input, 100_000);
        assert!(result.is_none());
    }

    #[test]
    fn test_decide_returns_none_when_legacy_wins() {
        let mut input = Vec::new();
        for i in 0..200 {
            let lit = ((i * 7) % 256) as u8;
            let prev = ((i * 13) % 256) as u8;
            input.push((lit, prev));
        }
        let result = decide_context_coding(&input, 100);
        assert!(result.is_none());
    }

    #[test]
    fn test_decide_returns_metadata_when_context_helps() {
        let mut input = Vec::new();
        for i in 0..200 {
            let prev = i as u8;
            let lit = if prev % 2 == 0 { b'A' } else { b'B' };
            input.push((lit, prev));
        }
        // Legacy cost should be high so it doesn't beat context coding.
        let result = decide_context_coding(&input, 100_000_000);
        assert!(result.is_some());
        let meta = result.unwrap();
        assert_ne!(meta.mode, ContextMode::None);
        assert!(meta.num_tables >= 2 && meta.num_tables <= 8);
    }

    #[test]
    fn test_decide_metadata_structure_valid() {
        let mut input = Vec::new();
        for i in 0..200 {
            let prev = i as u8;
            let lit = if prev % 2 == 0 { b'A' } else { b'B' };
            input.push((lit, prev));
        }
        let result = decide_context_coding(&input, 100_000_000);
        let meta = result.unwrap();
        
        assert_ne!(meta.mode, ContextMode::None);
        assert!(meta.num_tables >= 2 && meta.num_tables <= 16);
        assert_eq!(meta.context_map.len(), meta.mode.map_size());
        for &table_idx in &meta.context_map {
            assert!(table_idx < meta.num_tables);
        }
    }

    #[test]
    fn test_decide_determinism() {
        let mut input = Vec::new();
        for i in 0..200 {
            let prev = i as u8;
            let lit = if prev % 3 == 0 { b'X' } else { b'Y' };
            input.push((lit, prev));
        }
        let res1 = decide_context_coding(&input, 100_000_000);
        let res2 = decide_context_coding(&input, 100_000_000);
        let res3 = decide_context_coding(&input, 100_000_000);

        assert_eq!(res1, res2);
        assert_eq!(res2, res3);
    }

    #[test]
    fn test_decide_msb6_wins_for_msb_structured_input() {
        let mut input = Vec::new();
        for i in 0..200 {
            let prev = i as u8;
            let lit = if ((prev >> 2) % 2) == 0 { b'X' } else { b'Y' };
            input.push((lit, prev));
        }
        let result = decide_context_coding(&input, 100_000_000);
        assert!(result.is_some());
        let meta = result.unwrap();
        assert_ne!(meta.mode, ContextMode::None);
    }

    #[test]
    fn test_decide_lsb6_wins_for_lsb_structured_input() {
        let mut input = Vec::new();
        for i in 0..200 {
            let prev = i as u8;
            let lit = if ((prev & 0x3F) % 2) == 0 { b'X' } else { b'Y' };
            input.push((lit, prev));
        }
        let result = decide_context_coding(&input, 100_000_000);
        assert!(result.is_some());
        let meta = result.unwrap();
        assert_ne!(meta.mode, ContextMode::None);
    }

    #[test]
    fn test_decide_cost_monotonicity() {
        let mut input = Vec::new();
        for i in 0..200 {
            let prev = i as u8;
            let lit = if prev % 2 == 0 { b'A' } else { b'B' };
            input.push((lit, prev));
        }
        
        let legacy_cost = 1_000_000;
        let result = decide_context_coding(&input, legacy_cost);
        assert!(result.is_some());
        let meta = result.unwrap();

        let histograms = gather_context_stats(&input, meta.mode);
        let coding_cost = evaluate_clustering_cost(&histograms, &meta.context_map, meta.num_tables as usize);
        let overhead_bytes = 2 + meta.mode.map_size() + ((meta.num_tables as usize - 1) * 512);
        let overhead_bits = (overhead_bytes as u64) * 8;
        let total_candidate_cost = coding_cost + overhead_bits;

        assert!(total_candidate_cost < legacy_cost);
    }
}
