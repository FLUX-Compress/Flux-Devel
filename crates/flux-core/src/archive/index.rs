//! File index reader/writer
//!
//! Handles serialization, deserialization, and indexing of files at the front and back
//! of FLUX archives.

use std::fmt;
use crate::archive::format::FileEntry;
use crate::integrity::ChecksumSet;

/// Represents error states encountered during archive read, write, or verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArchiveError {
    /// The archive magic bytes did not match "FLUX".
    InvalidMagic,
    /// The archive version is not supported by this tool.
    UnsupportedVersion,
    /// The file index is corrupt or failed checksum validation.
    CorruptIndex,
    /// The archive header is corrupted.
    HeaderCorrupt,
    /// A solid block is corrupt.
    CorruptBlock(u32),
    /// A specific file failed integrity verification.
    CorruptFile(String),
    /// AES decryption failed (possibly due to data tampering).
    DecryptionFailed,
    /// The user provided an incorrect password.
    WrongPassword,
    /// A general filesystem or network I/O error.
    Io(String),
}

impl fmt::Display for ArchiveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ArchiveError::InvalidMagic => write!(f, "Invalid FLUX archive magic bytes"),
            ArchiveError::UnsupportedVersion => write!(f, "Unsupported FLUX archive version"),
            ArchiveError::CorruptIndex => write!(f, "Corrupt archive file index"),
            ArchiveError::HeaderCorrupt => write!(f, "Archive header is corrupted, cannot read this file"),
            ArchiveError::CorruptBlock(id) => write!(f, "Corrupt solid block (id: {})", id),
            ArchiveError::CorruptFile(path) => write!(f, "Corrupt file data: {}", path),
            ArchiveError::DecryptionFailed => write!(f, "Decryption failed"),
            ArchiveError::WrongPassword => write!(f, "Incorrect password"),
            ArchiveError::Io(err) => write!(f, "I/O error: {}", err),
        }
    }
}

impl std::error::Error for ArchiveError {}

/// File index storing all file preservation metadata.
#[derive(Debug, Clone, Default)]
pub struct FileIndex {
    /// All files preserved in the archive.
    pub entries: Vec<FileEntry>,
    /// Checksum verification for the file index.
    pub index_checksum: ChecksumSet,
    /// Total size of all compressed solid blocks.
    pub total_compressed_size: u64,
}

impl FileIndex {
    /// Creates a new empty `FileIndex`.
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            index_checksum: ChecksumSet::default(),
            total_compressed_size: 0,
        }
    }

    /// Adds a file entry to the index.
    pub fn add_entry(&mut self, entry: FileEntry) {
        self.entries.push(entry);
    }

    /// Finds a file entry matching the given relative path.
    pub fn find_by_path(&self, path: &str) -> Option<&FileEntry> {
        self.entries.iter().find(|e| e.path == path)
    }

    /// Returns the total count of files in the index.
    pub fn total_files(&self) -> usize {
        self.entries.len()
    }

    /// Returns the sum of uncompressed sizes of all files.
    pub fn total_original_size(&self) -> u64 {
        self.entries.iter().map(|e| e.original_size).sum()
    }

    /// Returns the sum of compressed sizes of the archive blocks.
    pub fn total_compressed_size(&self) -> u64 {
        self.total_compressed_size
    }

    /// Serializes the index into a binary byte representation.
    pub fn serialize(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&self.total_compressed_size.to_le_bytes());
        buf.extend_from_slice(&(self.entries.len() as u32).to_le_bytes());

        for entry in &self.entries {
            let path_bytes = entry.path.as_bytes();
            buf.extend_from_slice(&(path_bytes.len() as u32).to_le_bytes());
            buf.extend_from_slice(path_bytes);
            buf.extend_from_slice(&entry.original_size.to_le_bytes());
            buf.extend_from_slice(&entry.permissions.to_le_bytes());
            buf.extend_from_slice(&entry.modified_time.to_le_bytes());
            buf.extend_from_slice(&entry.created_time.to_le_bytes());
            buf.extend_from_slice(&entry.accessed_time.to_le_bytes());
            buf.extend_from_slice(&entry.solid_block_id.to_le_bytes());
            buf.extend_from_slice(&entry.block_offset.to_le_bytes());
            buf.extend_from_slice(&entry.original_sha256);
            buf.extend_from_slice(&entry.original_crc32.to_le_bytes());
        }

        // Compute checksum of the serialized buffer payload
        let checksum = ChecksumSet::compute(&buf);
        buf.extend_from_slice(&checksum.crc32.to_le_bytes());
        buf.extend_from_slice(&checksum.sha256);

        buf
    }

    /// Deserializes the index from a binary byte representation.
    pub fn deserialize(data: &[u8]) -> Result<Self, ArchiveError> {
        let (index, _) = Self::deserialize_prefix(data)?;
        Ok(index)
    }

    /// Deserializes the index from a prefix of the binary representation, returning the remaining unparsed bytes.
    pub fn deserialize_prefix(data: &[u8]) -> Result<(Self, &[u8]), ArchiveError> {
        if data.len() < 8 + 4 {
            return Err(ArchiveError::CorruptIndex);
        }

        let mut pos = 0;
        let total_compressed_size = u64::from_le_bytes(
            data[pos..pos + 8].try_into().map_err(|_| ArchiveError::CorruptIndex)?
        );
        pos += 8;

        let entries_count = u32::from_le_bytes(
            data[pos..pos + 4].try_into().map_err(|_| ArchiveError::CorruptIndex)?
        ) as usize;
        pos += 4;

        let mut entries = Vec::with_capacity(entries_count);
        for _ in 0..entries_count {
            if pos + 4 > data.len() {
                return Err(ArchiveError::CorruptIndex);
            }
            let path_len = u32::from_le_bytes(
                data[pos..pos + 4].try_into().map_err(|_| ArchiveError::CorruptIndex)?
            ) as usize;
            pos += 4;

            if pos + path_len > data.len() {
                return Err(ArchiveError::CorruptIndex);
            }
            let path = String::from_utf8(data[pos..pos + path_len].to_vec())
                .map_err(|_| ArchiveError::CorruptIndex)?;
            pos += path_len;

            if pos + 84 > data.len() {
                return Err(ArchiveError::CorruptIndex);
            }

            let original_size = u64::from_le_bytes(data[pos..pos + 8].try_into().unwrap());
            pos += 8;
            let permissions = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap());
            pos += 4;
            let modified_time = u64::from_le_bytes(data[pos..pos + 8].try_into().unwrap());
            pos += 8;
            let created_time = u64::from_le_bytes(data[pos..pos + 8].try_into().unwrap());
            pos += 8;
            let accessed_time = u64::from_le_bytes(data[pos..pos + 8].try_into().unwrap());
            pos += 8;
            let solid_block_id = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap());
            pos += 4;
            let block_offset = u64::from_le_bytes(data[pos..pos + 8].try_into().unwrap());
            pos += 8;
            let mut original_sha256 = [0u8; 32];
            original_sha256.copy_from_slice(&data[pos..pos + 32]);
            pos += 32;
            let original_crc32 = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap());
            pos += 4;

            entries.push(FileEntry {
                path,
                original_size,
                permissions,
                modified_time,
                created_time,
                accessed_time,
                solid_block_id,
                block_offset,
                original_sha256,
                original_crc32,
            });
        }

        if pos + 36 > data.len() {
            return Err(ArchiveError::CorruptIndex);
        }

        let payload = &data[..pos];
        let mut crc_bytes = [0u8; 4];
        crc_bytes.copy_from_slice(&data[pos..pos + 4]);
        let crc32 = u32::from_le_bytes(crc_bytes);

        let mut sha256 = [0u8; 32];
        sha256.copy_from_slice(&data[pos + 4..pos + 36]);

        let index_checksum = ChecksumSet { crc32, sha256 };
        if !index_checksum.verify(payload) {
            return Err(ArchiveError::CorruptIndex);
        }

        Ok((
            FileIndex {
                entries,
                index_checksum,
                total_compressed_size,
            },
            &data[pos + 36..],
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mock_entry(path: &str) -> FileEntry {
        FileEntry {
            path: path.to_string(),
            original_size: 1234,
            permissions: 0o644,
            modified_time: 1622248800,
            created_time: 1622248800,
            accessed_time: 1622248800,
            solid_block_id: 1,
            block_offset: 0,
            original_sha256: [0xa5; 32],
            original_crc32: 0x12345678,
        }
    }

    #[test]
    fn test_index_serialize_deserialize_roundtrip() {
        let mut index = FileIndex::new();
        index.total_compressed_size = 98765;
        index.add_entry(mock_entry("src/lib.rs"));
        index.add_entry(mock_entry("Cargo.toml"));

        let serialized = index.serialize();
        let deserialized = FileIndex::deserialize(&serialized).unwrap();

        assert_eq!(deserialized.total_compressed_size(), 98765);
        assert_eq!(deserialized.total_files(), 2);
        assert_eq!(deserialized.entries[0].path, "src/lib.rs");
        assert_eq!(deserialized.entries[1].path, "Cargo.toml");
        assert_eq!(deserialized.total_original_size(), 2468);
    }

    #[test]
    fn test_index_find_by_path() {
        let mut index = FileIndex::new();
        index.add_entry(mock_entry("src/lib.rs"));

        let entry = index.find_by_path("src/lib.rs");
        assert!(entry.is_some());
        assert_eq!(entry.unwrap().path, "src/lib.rs");

        let missing = index.find_by_path("missing.rs");
        assert!(missing.is_none());
    }

    #[test]
    fn test_index_corrupt_data_detected() {
        let mut index = FileIndex::new();
        index.add_entry(mock_entry("src/lib.rs"));

        let mut serialized = index.serialize();
        // Corrupt one byte of data (e.g. within the path string)
        serialized[15] ^= 1;

        let res = FileIndex::deserialize(&serialized);
        assert_eq!(res.err().unwrap(), ArchiveError::CorruptIndex);
    }
}
