//! Solid block creation and grouping logic
//!
//! Solid blocks group similar files together to preserve PPM models and dictionary contexts.
//! The files within each block are sorted smallest-first to warm up PPM and prediction states.

use crate::analysis::ClassificationResult;
use crate::archive::format::{BlockType, FileEntry, SolidBlock};
use crate::threads::signals::{CompressionPipeline, ContentType};
use std::collections::HashMap;

/// Builder to accumulate files into a single solid compression block.
#[derive(Debug, Clone)]
pub struct SolidBlockBuilder {
    /// The classification type of the block.
    pub block_type: BlockType,
    /// List of file entries inside this block.
    pub files: Vec<FileEntry>,
    /// Concatenated uncompressed raw data.
    pub raw_data: Vec<u8>,
    /// Limit on raw data size in bytes.
    pub max_block_size: usize,
    /// Internal accumulator of file entries and their byte content.
    pub entries_with_data: Vec<(FileEntry, Vec<u8>)>,
}

impl SolidBlockBuilder {
    /// Creates a new `SolidBlockBuilder` for the specified block type.
    pub fn new(block_type: BlockType) -> Self {
        Self {
            block_type,
            files: Vec::new(),
            raw_data: Vec::new(),
            max_block_size: 256 * 1024 * 1024, // 256MB default
            entries_with_data: Vec::new(),
        }
    }

    /// Attempts to add a file entry and its data to this solid block.
    ///
    /// Returns `false` if adding the file exceeds `max_block_size`.
    pub fn add_file(&mut self, entry: FileEntry, data: &[u8]) -> bool {
        let current_total: usize = self.entries_with_data.iter().map(|(_, d)| d.len()).sum();
        if current_total + data.len() > self.max_block_size {
            return false;
        }
        self.entries_with_data.push((entry, data.to_vec()));
        true
    }

    /// Checks whether the block size limit has been reached.
    pub fn is_full(&self) -> bool {
        let current_total: usize = self.entries_with_data.iter().map(|(_, d)| d.len()).sum();
        current_total >= self.max_block_size || current_total * 10 >= self.max_block_size * 9
    }

    /// Finalizes the block, sorting files smallest-first, and returns the block metadata.
    pub fn build(self) -> SolidBlock {
        let (block, _, _) = self.build_with_data();
        block
    }

    /// Finalizes the block and returns both metadata and concatenated raw uncompressed data.
    pub fn build_with_data(mut self) -> (SolidBlock, Vec<u8>, Vec<FileEntry>) {
        // Sort files smallest first to warm up compression models gradually.
        self.entries_with_data
            .sort_by_key(|(entry, _)| entry.original_size);

        let uncompressed_size = self
            .entries_with_data
            .iter()
            .map(|(_, d)| d.len())
            .sum::<usize>();
        let mut raw_data = Vec::with_capacity(uncompressed_size);
        let mut files = Vec::new();
        for (mut entry, data) in self.entries_with_data {
            entry.block_offset = raw_data.len() as u64;
            raw_data.extend(data);
            files.push(entry);
        }
        self.files = files.clone();
        self.raw_data = raw_data;
        let block = SolidBlock {
            block_id: 0,
            block_type: self.block_type,
            compressed_size: 0,
            uncompressed_size: uncompressed_size as u64,
            compressed_hash: [0u8; 32],
        };

        (block, self.raw_data, files)
    }
}

/// Helper to sort and group files from directory scans into solid block builders.
#[derive(Debug, Default)]
pub struct SolidBlockGrouper;

impl SolidBlockGrouper {
    /// Creates a new `SolidBlockGrouper`.
    pub fn new() -> Self {
        Self
    }

    /// Groups a scanned list of files and data into optimal solid blocks based on content classifications.
    pub fn group_files(
        &self,
        files: Vec<(FileEntry, Vec<u8>)>,
        classifications: &HashMap<u64, ClassificationResult>,
    ) -> Vec<SolidBlockBuilder> {
        let mut text_files = Vec::new();
        let mut multimedia_files = Vec::new();
        let mut binary_files = Vec::new();
        let mut raw_files = Vec::new();
        let mut mixed_files = Vec::new();

        for (file_idx, (entry, data)) in files.into_iter().enumerate() {
            let target_type = if let Some(cls) = classifications.get(&(file_idx as u64)) {
                if cls.pipeline == CompressionPipeline::StoreRaw
                    || cls.content_type == ContentType::Incompressible
                {
                    BlockType::Raw
                } else {
                    match cls.content_type {
                        ContentType::Text => BlockType::Text,
                        ContentType::Multimedia => BlockType::Multimedia,
                        ContentType::Binary => BlockType::Binary,
                        ContentType::Executable => BlockType::Binary,
                        ContentType::Incompressible => BlockType::Raw,
                    }
                }
            } else {
                BlockType::Binary
            };

            match target_type {
                BlockType::Text => text_files.push((entry, data)),
                BlockType::Multimedia => multimedia_files.push((entry, data)),
                BlockType::Binary => binary_files.push((entry, data)),
                BlockType::Raw => raw_files.push((entry, data)),
                BlockType::Mixed => mixed_files.push((entry, data)),
            }
        }

        fn pack_into_builders(
            block_type: BlockType,
            mut items: Vec<(FileEntry, Vec<u8>)>,
        ) -> Vec<SolidBlockBuilder> {
            // Sort by file extension first (for similarity grouping), then by path
            items.sort_by(|(a, _), (b, _)| {
                let ext_a = std::path::Path::new(&a.path).extension();
                let ext_b = std::path::Path::new(&b.path).extension();
                ext_a.cmp(&ext_b).then_with(|| a.path.cmp(&b.path))
            });

            let mut builders: Vec<SolidBlockBuilder> = Vec::new();
            for (entry, data) in items {
                let mut added = false;
                for builder in &mut builders {
                    if builder.add_file(entry.clone(), &data) {
                        added = true;
                        break;
                    }
                }
                if !added {
                    let mut new_builder = SolidBlockBuilder::new(block_type);
                    new_builder.add_file(entry, &data);
                    builders.push(new_builder);
                }
            }
            builders
        }

        let mut result = Vec::new();
        result.extend(pack_into_builders(BlockType::Text, text_files));
        result.extend(pack_into_builders(BlockType::Multimedia, multimedia_files));
        result.extend(pack_into_builders(BlockType::Binary, binary_files));
        result.extend(pack_into_builders(BlockType::Raw, raw_files));
        result.extend(pack_into_builders(BlockType::Mixed, mixed_files));

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mock_file(path: &str, size: u64) -> FileEntry {
        FileEntry {
            path: path.to_string(),
            original_size: size,
            permissions: 0o644,
            modified_time: 0,
            created_time: 0,
            accessed_time: 0,
            solid_block_id: 0,
            block_offset: 0,
            original_sha256: [0; 32],
            original_crc32: 0,
        }
    }

    #[test]
    fn test_solid_block_groups_by_type() {
        let grouper = SolidBlockGrouper::new();
        let files = vec![
            (mock_file("src/main.rs", 100), vec![0; 100]),
            (mock_file("image.png", 500), vec![0; 500]),
            (mock_file("audio.wav", 1000), vec![0; 1000]),
        ];

        let mut classifications = HashMap::new();
        classifications.insert(
            0,
            ClassificationResult {
                content_type: ContentType::Text,
                pipeline: CompressionPipeline::TextPipeline,
                stride_hint: None,
                window_size_recommendation: 4096,
                confidence: 1.0,
                estimated_ratio: 0.5,
            },
        );
        classifications.insert(
            1,
            ClassificationResult {
                content_type: ContentType::Incompressible,
                pipeline: CompressionPipeline::StoreRaw,
                stride_hint: None,
                window_size_recommendation: 4096,
                confidence: 1.0,
                estimated_ratio: 1.0,
            },
        );
        classifications.insert(
            2,
            ClassificationResult {
                content_type: ContentType::Multimedia,
                pipeline: CompressionPipeline::MultimediaPipeline,
                stride_hint: Some(2),
                window_size_recommendation: 4096,
                confidence: 1.0,
                estimated_ratio: 0.5,
            },
        );

        let builders = grouper.group_files(files, &classifications);
        // We expect one Text block, one Raw block, and one Multimedia block.
        assert_eq!(builders.len(), 3);
        assert!(builders.iter().any(|b| b.block_type == BlockType::Text));
        assert!(builders.iter().any(|b| b.block_type == BlockType::Raw));
        assert!(builders
            .iter()
            .any(|b| b.block_type == BlockType::Multimedia));
    }

    #[test]
    fn test_solid_block_sorts_small_files_first() {
        let mut builder = SolidBlockBuilder::new(BlockType::Text);
        builder.add_file(mock_file("large.rs", 1000), &[0; 1000]);
        builder.add_file(mock_file("small.rs", 50), &[0; 50]);
        builder.add_file(mock_file("medium.rs", 200), &[0; 200]);

        let (_, data, files) = builder.build_with_data();
        assert_eq!(data.len(), 1250);

        // Check order of files in built state.
        assert_eq!(files[0].path, "small.rs");
        assert_eq!(files[1].path, "medium.rs");
        assert_eq!(files[2].path, "large.rs");

        // Verify offsets: small (offset 0), medium (offset 50), large (offset 250)
        assert_eq!(files[0].block_offset, 0);
        assert_eq!(files[1].block_offset, 50);
        assert_eq!(files[2].block_offset, 250);
    }

    #[test]
    fn test_solid_block_incompressible_goes_to_raw() {
        let grouper = SolidBlockGrouper::new();
        let files = vec![(mock_file("data.zip", 2000), vec![0; 2000])];

        let mut classifications = HashMap::new();
        classifications.insert(
            0,
            ClassificationResult {
                content_type: ContentType::Incompressible,
                pipeline: CompressionPipeline::StoreRaw,
                stride_hint: None,
                window_size_recommendation: 0,
                confidence: 1.0,
                estimated_ratio: 1.0,
            },
        );

        let builders = grouper.group_files(files, &classifications);
        assert_eq!(builders.len(), 1);
        assert_eq!(builders[0].block_type, BlockType::Raw);
    }

    #[test]
    fn test_solid_block_size_limit_respected() {
        let mut builder = SolidBlockBuilder::new(BlockType::Text);
        builder.max_block_size = 100;

        assert!(builder.add_file(mock_file("a.txt", 40), &[0; 40]));
        assert!(builder.add_file(mock_file("b.txt", 50), &[0; 50]));
        // Next file (size 20) exceeds limit (40 + 50 + 20 = 110 > 100)
        assert!(!builder.add_file(mock_file("c.txt", 20), &[0; 20]));
        assert!(builder.is_full());
    }
}
