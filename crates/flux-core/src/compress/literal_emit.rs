//! Context-coded literal stream emitter.
//! See docs/v1_5_context_literals_design.md Section 2 and Section 3 Pass 4.

use crate::archive::ArchiveError;
use crate::compress::clustering::{Histogram, merge_histograms};
use crate::compress::context::{ContextMode, MultiTableMetadata, get_context_id};
use crate::compress::context_stats::gather_context_stats;
use crate::compress::rans::{RansEncoder, ProbabilityTable};

/// Builds a `ProbabilityTable` from a `Histogram`.
fn probability_table_from_histogram(hist: &Histogram) -> ProbabilityTable {
    let total: u32 = hist.iter().sum();
    let mut probs_list = Vec::new();
    if total > 0 {
        let total_f = total as f32;
        for (i, &count) in hist.iter().enumerate() {
            if count > 0 {
                probs_list.push((i as u8, count as f32 / total_f));
            }
        }
    }
    ProbabilityTable::from_probabilities(&probs_list)
}

/// Emits the context-coded literal stream bytes to the output buffer.
///
/// Precondition: `metadata.mode != ContextMode::None`.
///
/// OPTION X (chosen): Encode literals in original order using per-symbol context-selected tables.
/// The rANS state stream has all literals interleaved. Since rANS is LIFO, the encoder runs
/// in reverse order, allowing the decoder to decode in forward order.
pub fn emit_context_coded_literals(
    literals_with_context: &[(u8, u8)],
    metadata: &MultiTableMetadata,
    output: &mut Vec<u8>,
) -> Result<(), ArchiveError> {
    if metadata.mode == ContextMode::None {
        return Err(ArchiveError::CorruptIndex);
    }

    let mode = metadata.mode;
    let num_tables = metadata.num_tables as usize;
    let map_size = mode.map_size();

    if metadata.context_map.len() != map_size {
        return Err(ArchiveError::CorruptIndex);
    }

    // 1. Gather histograms per context
    let context_hists = gather_context_stats(literals_with_context, mode);

    // 2. Build merged histograms for each table index
    let mut table_hists = vec![[0u32; 256]; num_tables];
    for (c, &table_idx) in metadata.context_map.iter().enumerate() {
        let t = table_idx as usize;
        if t < num_tables && c < context_hists.len() {
            table_hists[t] = merge_histograms(&table_hists[t], &context_hists[c]);
        }
    }

    // 3. Construct ProbabilityTable for each table
    let mut tables = Vec::with_capacity(num_tables);
    for hist in &table_hists {
        tables.push(probability_table_from_histogram(hist));
    }

    // 4. Serialize layout header
    // a. context_mode byte
    output.push(mode as u8);
    // b. num_tables byte
    output.push(metadata.num_tables);
    // c. context_map bytes
    output.extend_from_slice(&metadata.context_map);
    // d. freq_tables (num_tables * 512 bytes)
    for table in &tables {
        for sym in &table.symbols {
            output.extend_from_slice(&(sym.freq as u16).to_le_bytes());
        }
    }

    // 5. Encode literals using rANS in reverse order (Option X)
    let mut encoder = RansEncoder::new();
    for i in (0..literals_with_context.len()).rev() {
        let (lit, prev) = literals_with_context[i];
        let context_id = get_context_id(mode, prev);
        if context_id < map_size {
            let table_idx = metadata.context_map[context_id] as usize;
            if table_idx < num_tables {
                encoder.encode_symbol(lit, &tables[table_idx]);
            } else {
                return Err(ArchiveError::CorruptIndex);
            }
        } else {
            return Err(ArchiveError::CorruptIndex);
        }
    }

    // e. rANS payload
    let payload = encoder.flush();
    output.extend_from_slice(&payload);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_emit_with_none_mode_errors() {
        let literals = vec![(b'A', b'B'); 10];
        let metadata = MultiTableMetadata {
            mode: ContextMode::None,
            num_tables: 1,
            context_map: vec![],
        };
        let mut output = Vec::new();
        let result = emit_context_coded_literals(&literals, &metadata, &mut output);
        assert!(result.is_err());
    }

    #[test]
    fn test_emit_produces_expected_header() {
        let literals = vec![(b'A', 0x10), (b'B', 0x10), (b'C', 0x20)];
        let metadata = MultiTableMetadata {
            mode: ContextMode::Full,
            num_tables: 2,
            context_map: vec![0u8; 256], // Map all contexts to table 0
        };
        let mut output = Vec::new();
        let result = emit_context_coded_literals(&literals, &metadata, &mut output);
        assert!(result.is_ok());

        // Header size: 1 (mode) + 1 (num_tables) + 256 (context_map) + 2 * 512 (freq_tables) = 1282 bytes
        assert!(output.len() > 1282);
        assert_eq!(output[0], ContextMode::Full as u8);
        assert_eq!(output[1], 2);
        assert_eq!(&output[2..258], &vec![0u8; 256]);
    }

    #[test]
    fn test_emit_determinism() {
        let mut literals = Vec::new();
        for i in 0..100 {
            literals.push(( (i % 256) as u8, (i * 3 % 256) as u8 ));
        }
        let mut context_map = vec![0u8; 64];
        for (i, val) in context_map.iter_mut().enumerate() {
            *val = (i % 3) as u8;
        }
        let metadata = MultiTableMetadata {
            mode: ContextMode::Msb6,
            num_tables: 3,
            context_map,
        };

        let mut out1 = Vec::new();
        let mut out2 = Vec::new();
        let mut out3 = Vec::new();

        emit_context_coded_literals(&literals, &metadata, &mut out1).unwrap();
        emit_context_coded_literals(&literals, &metadata, &mut out2).unwrap();
        emit_context_coded_literals(&literals, &metadata, &mut out3).unwrap();

        assert_eq!(out1, out2);
        assert_eq!(out2, out3);
    }
}
