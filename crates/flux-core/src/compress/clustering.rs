//! Greedy agglomerative clustering for context-mapped literals.
//! See docs/v1_5_context_literals_design.md Section 3 Pass 2.

/// Histogram type representing byte frequencies for literals.
pub type Histogram = [u32; 256];

/// Computes the binary logarithm of a 32-bit integer in 16.16 fixed-point format.
/// Returns log2(x) * 65536. If x is 0, returns 0.
fn log2_fixed(x: u32) -> u64 {
    if x == 0 {
        return 0;
    }
    let k = 31 - x.leading_zeros();
    let mut val = (x as u64) << (30 - k);
    let mut frac = 0u64;
    let mut bit = 1u64 << 31;
    for _ in 0..32 {
        val = (val * val) >> 30;
        if val >= (2u64 << 30) {
            frac |= bit;
            val >>= 1;
        }
        bit >>= 1;
    }
    ((k as u64) << 16) + (frac >> 16)
}

/// Returns the "weighted entropy" of a histogram:
/// Σ for nonzero counts c: c * log2(N / c)
/// computed deterministically using 16.16 fixed-point math.
fn cluster_entropy_times_n(hist: &Histogram) -> u64 {
    let mut sum_c = 0u64;
    let mut sum_c_log_c = 0u64;
    for &c in hist.iter() {
        if c > 0 {
            let c64 = c as u64;
            sum_c += c64;
            sum_c_log_c += c64 * log2_fixed(c);
        }
    }
    if sum_c == 0 {
        return 0;
    }
    let n = sum_c as u32;
    let n_log_n = sum_c * log2_fixed(n);
    n_log_n.saturating_sub(sum_c_log_c)
}

/// Returns the elementwise sum of two histograms (no normalization).
fn merge_histograms(a: &Histogram, b: &Histogram) -> Histogram {
    let mut merged = [0u32; 256];
    for i in 0..256 {
        merged[i] = a[i].saturating_add(b[i]);
    }
    merged
}

/// Greedy agglomerative clustering for context histograms.
///
/// Maps each context to a cluster index in `0..num_tables-1`.
/// Deterministic and platform-independent.
pub fn cluster_contexts(
    histograms: &[Histogram],
    num_tables: usize,
) -> Vec<u8> {
    let n = histograms.len();
    if n == 0 {
        return Vec::new();
    }
    if num_tables >= n {
        return (0..n as u8).collect();
    }

    let target_k = num_tables.max(1);

    // Initial cluster state: each context is its own cluster.
    // Zero-count contexts will start with all-zero histograms, having an initial
    // entropy of 0. When merged with other zero-count contexts, the merged histogram
    // remains all-zeros with 0 entropy, resulting in a merge loss of 0. Thus,
    // zero-count contexts naturally cluster together first due to minimum loss.
    struct Cluster {
        context_ids: Vec<u8>,
        hist: Histogram,
        entropy_times_n: u64,
    }

    let mut clusters: Vec<Cluster> = histograms
        .iter()
        .enumerate()
        .map(|(c_id, hist)| {
            let entropy = cluster_entropy_times_n(hist);
            Cluster {
                context_ids: vec![c_id as u8],
                hist: *hist,
                entropy_times_n: entropy,
            }
        })
        .collect();

    // Greedy agglomerative merge loop.
    while clusters.len() > target_k {
        let mut best_pair = None;
        let mut min_loss = i64::MAX;
        let mut best_c1 = u8::MAX;
        let mut best_c2 = u8::MAX;

        for i in 0..clusters.len() {
            for j in (i + 1)..clusters.len() {
                let merged_hist = merge_histograms(&clusters[i].hist, &clusters[j].hist);
                let merged_entropy = cluster_entropy_times_n(&merged_hist);
                let loss = (merged_entropy as i64)
                    - (clusters[i].entropy_times_n as i64)
                    - (clusters[j].entropy_times_n as i64);

                let c1 = clusters[i].context_ids[0];
                let c2 = clusters[j].context_ids[0];

                let is_better = if best_pair.is_none() || loss < min_loss {
                    true
                } else if loss == min_loss {
                    // Deterministic tie-breaking: lowest c1 first, then lowest c2.
                    if c1 < best_c1 {
                        true
                    } else if c1 == best_c1 {
                        c2 < best_c2
                    } else {
                        false
                    }
                } else {
                    false
                };

                if is_better {
                    min_loss = loss;
                    best_pair = Some((i, j));
                    best_c1 = c1;
                    best_c2 = c2;
                }
            }
        }

        if let Some((i, j)) = best_pair {
            // Merge cluster j into cluster i.
            // We remove j first to satisfy the borrow checker. Since i < j, removing j
            // does not shift the position of i.
            let bj = clusters.remove(j);
            let new_hist = merge_histograms(&clusters[i].hist, &bj.hist);
            let new_entropy = cluster_entropy_times_n(&new_hist);
            
            clusters[i].context_ids.extend(&bj.context_ids);
            clusters[i].context_ids.sort();
            clusters[i].hist = new_hist;
            clusters[i].entropy_times_n = new_entropy;
        } else {
            break; // Should not happen since target_k < clusters.len()
        }
    }

    // Assign final cluster index mapping.
    let mut context_map = vec![0; n];
    for (cluster_idx, cluster) in clusters.iter().enumerate() {
        for &c_id in &cluster.context_ids {
            context_map[c_id as usize] = cluster_idx as u8;
        }
    }

    context_map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cluster_trivial_identity() {
        let mut h0 = [0u32; 256];
        h0[0] = 10;
        let mut h1 = [0u32; 256];
        h1[1] = 10;
        let mut h2 = [0u32; 256];
        h2[2] = 10;
        let mut h3 = [0u32; 256];
        h3[3] = 10;

        let histograms = vec![h0, h1, h2, h3];
        let map = cluster_contexts(&histograms, 4);
        assert_eq!(map, vec![0, 1, 2, 3]);
    }

    #[test]
    fn test_cluster_obvious_two_groups() {
        // h0 and h1 are heavy on index 0
        let mut h0 = [0u32; 256];
        h0[0] = 100;
        let mut h1 = [0u32; 256];
        h1[0] = 90; h1[1] = 10;

        // h2 and h3 are heavy on index 2
        let mut h2 = [0u32; 256];
        h2[2] = 100;
        let mut h3 = [0u32; 256];
        h3[2] = 90; h3[3] = 10;

        let histograms = vec![h0, h1, h2, h3];
        let map = cluster_contexts(&histograms, 2);
        
        // 0 and 1 must share a cluster, 2 and 3 must share a different cluster.
        assert_eq!(map[0], map[1]);
        assert_eq!(map[2], map[3]);
        assert_ne!(map[0], map[2]);
    }

    #[test]
    fn test_cluster_all_to_one() {
        let mut histograms = Vec::new();
        for i in 0..8 {
            let mut h = [0u32; 256];
            h[i] = 10;
            histograms.push(h);
        }
        let map = cluster_contexts(&histograms, 1);
        assert_eq!(map, vec![0, 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn test_cluster_determinism() {
        let mut histograms = Vec::new();
        for i in 0..16 {
            let mut h = [0u32; 256];
            h[i % 5] = (i * 7 + 3) as u32;
            histograms.push(h);
        }
        let map1 = cluster_contexts(&histograms, 4);
        let map2 = cluster_contexts(&histograms, 4);
        let map3 = cluster_contexts(&histograms, 4);
        assert_eq!(map1, map2);
        assert_eq!(map2, map3);
    }

    #[test]
    fn test_cluster_handles_empty_contexts() {
        // Context 0 and 1 are empty. Context 2 and 3 have values.
        let h_empty = [0u32; 256];
        let mut h2 = [0u32; 256];
        h2[0] = 10;
        let mut h3 = [0u32; 256];
        h3[1] = 10;

        let histograms = vec![h_empty, h_empty, h2, h3];
        let map = cluster_contexts(&histograms, 2);
        
        // Verify no panic, and all contexts get assigned valid indices (0 or 1)
        for &val in &map {
            assert!(val < 2);
        }
        // Zero-count contexts 0 and 1 should cluster together
        assert_eq!(map[0], map[1]);
    }

    #[test]
    fn test_cluster_tie_breaking() {
        // All histograms are identical, so all merges have identical loss (0).
        let h = [10u32; 256];
        let histograms = vec![h, h, h, h];
        let map = cluster_contexts(&histograms, 2);

        // Under the lowest c1, then lowest c2 tie-breaking rule:
        // 1. Initial clusters: {0}, {1}, {2}, {3}
        // 2. First merge: c1=0, c2=1 is the lowest lexicographical pair. Merges 0 and 1 -> {0, 1}.
        //    Clusters are now: {0, 1}, {2}, {3}
        // 3. Second merge:
        //    Pairs: ({0, 1}, {2}) -> c1=0, c2=2
        //           ({0, 1}, {3}) -> c1=0, c2=3
        //           ({2}, {3})    -> c1=2, c2=3
        //    Lexicographically lowest pair is ({0, 1}, {2}) with c1=0, c2=2.
        //    Merges them into {0, 1, 2}.
        //    Clusters are now: {0, 1, 2}, {3} (count = 2, equals target_k).
        // 4. Output map assigns cluster index 0 to 0, 1, 2 and cluster index 1 to 3.
        //    Expected output: [0, 0, 0, 1]
        assert_eq!(map, vec![0, 0, 0, 1]);
    }

    #[test]
    fn test_cluster_k_equals_n() {
        let mut h0 = [0u32; 256];
        h0[0] = 5;
        let mut h1 = [0u32; 256];
        h1[1] = 12;
        let mut h2 = [0u32; 256];
        h2[2] = 20;

        let histograms = vec![h0, h1, h2];
        let map = cluster_contexts(&histograms, 3);
        assert_eq!(map, vec![0, 1, 2]);
    }

    #[test]
    fn test_cluster_k_greater_than_n() {
        let mut h0 = [0u32; 256];
        h0[0] = 5;
        let mut h1 = [0u32; 256];
        h1[1] = 12;
        let mut h2 = [0u32; 256];
        h2[2] = 20;

        let histograms = vec![h0, h1, h2];
        let map = cluster_contexts(&histograms, 5);
        assert_eq!(map, vec![0, 1, 2]);
    }
}
