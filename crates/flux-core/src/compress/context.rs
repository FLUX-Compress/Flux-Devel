//! Context-mapped literal coding structures and helper functions.
//!
//! Provides the core context extraction functions (FULL, MSB6, LSB6) and
//! the `MultiTableMetadata` structure that represents the context mode, the number
//! of tables K, and the context map, along with serialization routines.

use crate::archive::ArchiveError;

/// Context mapping mode used for literals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContextMode {
    /// No context mapping (legacy single-table format).
    None = 0,
    /// Full previous byte as context (256 contexts).
    Full = 1,
    /// Previous byte's top 6 bits as context (64 contexts).
    Msb6 = 2,
    /// Previous byte's bottom 6 bits as context (64 contexts).
    Lsb6 = 3,
}

impl ContextMode {
    /// Converts a byte value to a `ContextMode`.
    pub fn from_u8(val: u8) -> Result<Self, ArchiveError> {
        match val {
            0 => Ok(ContextMode::None),
            1 => Ok(ContextMode::Full),
            2 => Ok(ContextMode::Msb6),
            3 => Ok(ContextMode::Lsb6),
            _ => Err(ArchiveError::CorruptIndex),
        }
    }

    /// Returns the size of the context map in bytes for this mode.
    pub fn map_size(&self) -> usize {
        match self {
            ContextMode::None => 0,
            ContextMode::Full => 256,
            ContextMode::Msb6 | ContextMode::Lsb6 => 64,
        }
    }
}

/// Extracts context ID from the preceding reconstructed bytes.
#[inline]
pub fn get_context_id(mode: ContextMode, prev1: u8) -> usize {
    match mode {
        ContextMode::None => 0,
        ContextMode::Full => prev1 as usize,
        ContextMode::Msb6 => (prev1 >> 2) as usize,
        ContextMode::Lsb6 => (prev1 & 0x3F) as usize,
    }
}

/// Metadata describing the context-mapped literals layout for a block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MultiTableMetadata {
    /// Context extraction mode.
    pub mode: ContextMode,
    /// Number of distinct frequency tables (1..=16).
    pub num_tables: u8,
    /// Maps context ID to table index (0..K-1).
    pub context_map: Vec<u8>,
}

impl MultiTableMetadata {
    /// Serializes the metadata to a binary representation.
    pub fn serialize(&self) -> Vec<u8> {
        debug_assert!(
            self.context_map.len() <= self.mode.map_size(),
            "context_map too long for this mode"
        );
        let map_len = self.mode.map_size();
        let mut buf = Vec::with_capacity(2 + map_len);
        buf.push(self.mode as u8);
        buf.push(self.num_tables);
        if map_len > 0 {
            let limit = map_len.min(self.context_map.len());
            buf.extend_from_slice(&self.context_map[..limit]);
            if self.context_map.len() < map_len {
                buf.resize(2 + map_len, 0);
            }
        }
        buf
    }

    /// Deserializes the metadata from a byte slice.
    pub fn deserialize(data: &[u8]) -> Result<(Self, &[u8]), ArchiveError> {
        if data.len() < 2 {
            return Err(ArchiveError::CorruptIndex);
        }
        let mode = ContextMode::from_u8(data[0])?;
        let num_tables = data[1];

        // Validate num_tables is in [1, 16] range.
        if num_tables == 0 || num_tables > 16 {
            return Err(ArchiveError::CorruptIndex);
        }
        // When mode is None, num_tables must be exactly 1 (legacy single table).
        if mode == ContextMode::None && num_tables != 1 {
            return Err(ArchiveError::CorruptIndex);
        }

        let map_len = mode.map_size();
        if data.len() < 2 + map_len {
            return Err(ArchiveError::CorruptIndex);
        }
        let context_map = data[2..2 + map_len].to_vec();
        Ok((
            MultiTableMetadata {
                mode,
                num_tables,
                context_map,
            },
            &data[2 + map_len..],
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_context_modes() {
        assert_eq!(get_context_id(ContextMode::None, 0xAA), 0);
        assert_eq!(get_context_id(ContextMode::Full, 0xAA), 0xAA);
        assert_eq!(get_context_id(ContextMode::Msb6, 0xFC), 0x3F);
        assert_eq!(get_context_id(ContextMode::Lsb6, 0xFF), 0x3F);
        
        assert_eq!(ContextMode::None.map_size(), 0);
        assert_eq!(ContextMode::Full.map_size(), 256);
        assert_eq!(ContextMode::Msb6.map_size(), 64);
        assert_eq!(ContextMode::Lsb6.map_size(), 64);
    }

    #[test]
    fn test_metadata_serialization_none() {
        let meta = MultiTableMetadata {
            mode: ContextMode::None,
            num_tables: 1,
            context_map: Vec::new(),
        };
        let serialized = meta.serialize();
        assert_eq!(serialized, vec![0, 1]);

        let (deserialized, remaining) = MultiTableMetadata::deserialize(&serialized).unwrap();
        assert_eq!(deserialized, meta);
        assert_eq!(remaining.len(), 0);
    }

    #[test]
    fn test_metadata_serialization_full() {
        let mut map = vec![0u8; 256];
        for (i, item) in map.iter_mut().enumerate() {
            *item = (i % 4) as u8;
        }
        let meta = MultiTableMetadata {
            mode: ContextMode::Full,
            num_tables: 4,
            context_map: map,
        };
        let serialized = meta.serialize();
        assert_eq!(serialized.len(), 258);
        assert_eq!(serialized[0], 1);
        assert_eq!(serialized[1], 4);
        assert_eq!(serialized[2], 0);
        assert_eq!(serialized[3], 1);

        let (deserialized, remaining) = MultiTableMetadata::deserialize(&serialized).unwrap();
        assert_eq!(deserialized, meta);
        assert_eq!(remaining.len(), 0);
    }

    #[test]
    fn test_metadata_serialization_short() {
        let map = vec![3u8; 10]; // intentionally too short
        let meta = MultiTableMetadata {
            mode: ContextMode::Lsb6,
            num_tables: 5,
            context_map: map,
        };
        let serialized = meta.serialize();
        assert_eq!(serialized.len(), 66);
        assert_eq!(serialized[0], 3);
        assert_eq!(serialized[1], 5);
        assert_eq!(serialized[2], 3);
        assert_eq!(serialized[11], 3);
        assert_eq!(serialized[12], 0); // padded with 0s

        let (deserialized, remaining) = MultiTableMetadata::deserialize(&serialized).unwrap();
        assert_eq!(deserialized.mode, ContextMode::Lsb6);
        assert_eq!(deserialized.num_tables, 5);
        assert_eq!(deserialized.context_map.len(), 64);
        assert_eq!(deserialized.context_map[0], 3);
        assert_eq!(deserialized.context_map[13], 0);
        assert_eq!(remaining.len(), 0);
    }

    #[test]
    fn test_deserialize_corrupt() {
        let short_data = vec![1];
        assert!(MultiTableMetadata::deserialize(&short_data).is_err());

        let invalid_mode = vec![9, 4];
        assert!(MultiTableMetadata::deserialize(&invalid_mode).is_err());

        let missing_map = vec![1, 4, 1, 2, 3]; // map too short for FULL
        assert!(MultiTableMetadata::deserialize(&missing_map).is_err());

        // num_tables = 0 is invalid
        let zero_tables = vec![1, 0, 1, 2, 3];
        assert!(MultiTableMetadata::deserialize(&zero_tables).is_err());

        // num_tables = 17 is invalid (> 16)
        let too_many_tables = vec![1, 17, 1, 2, 3];
        assert!(MultiTableMetadata::deserialize(&too_many_tables).is_err());

        // mode = None but num_tables != 1 is invalid
        let none_mode_multi_tables = vec![0, 2];
        assert!(MultiTableMetadata::deserialize(&none_mode_multi_tables).is_err());
    }
}
