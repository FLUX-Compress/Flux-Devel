//! Archive Format Specification & Structure Definitions
//!
//! Describes the binary representation of FLUX archive structures, including plaintext headers,
//! encrypted metadata sections, solid block configurations, and directory entries.

use crate::threads::signals::ContentType;

/// The 4-byte magic signature at the start of every FLUX archive.
pub const FLUX_MAGIC: [u8; 4] = *b"FLUX";

/// Major version number of the FLUX format.
pub const FLUX_VERSION_MAJOR: u16 = 1;

/// Minor version number of the FLUX format.
pub const FLUX_VERSION_MINOR: u16 = 0;

/// Plaintext bootstrap section of the FLUX Archive Header.
///
/// This structure is stored unencrypted at the very beginning of the archive
/// to allow extractor tools to read formats, versions, and cryptography parameters.
#[derive(Debug, Clone)]
pub struct ArchiveHeader {
    /// Magic signature, must match [`FLUX_MAGIC`].
    pub magic: [u8; 4],
    /// Major format version.
    pub version_major: u16,
    /// Minor format version.
    pub version_minor: u16,
    /// Argon2id memory size parameter in kilobytes.
    pub argon2_memory_kb: u32,
    /// Argon2id iteration parameter.
    pub argon2_iterations: u32,
    /// Argon2id parallelism parameter (threads).
    pub argon2_parallelism: u32,
    /// 128-bit random salt used for key derivation.
    pub salt: [u8; 16],
    /// Initial vector (nonce) used for header decryption.
    pub header_iv: [u8; 12],
    /// Wrong-password validation verification sentinel.
    pub password_sentinel: [u8; 16],
}

/// Encrypted metadata section of the FLUX Archive Header.
///
/// Contains directories, trees, file counts, and layout indices. Encrypted using AES-256-GCM.
#[derive(Debug, Clone)]
pub struct EncryptedHeader {
    /// Total number of files preserved inside this archive.
    pub total_files: u64,
    /// Total size in bytes of the uncompressed directory tree representation.
    pub directory_tree_size: u32,
    /// Absolute byte offset in the archive where the Solid Block payload begins.
    pub solid_payload_offset: u64,
}

/// Category of data stored inside a solid compression block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockType {
    /// Text files (sources, logs, markdown).
    Text,
    /// Binaries, executables, or compressed artifacts.
    Binary,
    /// Structured or high-stride values (audio, spreadsheets, bitmaps).
    Multimedia,
    /// Arbitrary fallback grouping.
    Mixed,
    /// Stored raw with zero compression applied.
    Raw,
}

/// Represents a single solid compression block within the archive payload.
///
/// Similar files are grouped together into solid blocks to maximize dictionary sharing.
#[derive(Debug, Clone)]
pub struct SolidBlock {
    /// Block sequence ID.
    pub block_id: u32,
    /// Data classification category.
    pub block_type: BlockType,
    /// Size of this block when compressed on disk in bytes.
    pub compressed_size: u64,
    /// Original uncompressed size of the block data in bytes.
    pub uncompressed_size: u64,
    /// SHA-256 integrity hash of this compressed block on disk.
    pub compressed_hash: [u8; 32],
}

/// Preservation metadata for an archived directory entry or file.
#[derive(Debug, Clone)]
pub struct FileEntry {
    /// Relative file path within the archive.
    pub path: String,
    /// Uncompressed size of this file in bytes.
    pub original_size: u64,
    /// Unix/Windows file system permissions representation.
    pub permissions: u32,
    /// Modification timestamp (seconds since Unix Epoch).
    pub modified_time: u64,
    /// Creation timestamp (seconds since Unix Epoch).
    pub created_time: u64,
    /// Access timestamp (seconds since Unix Epoch).
    pub accessed_time: u64,
    /// The ID of the solid block containing this file's data.
    pub solid_block_id: u32,
    /// Offset (in bytes) within the uncompressed solid block stream.
    pub block_offset: u64,
    /// SHA-256 hash of the uncompressed original file.
    pub original_sha256: [u8; 32],
    /// CRC32 checksum of the uncompressed original file.
    pub original_crc32: u32,
}

/// A hint produced by the BackwardsAnalyzer at EOF describing an optimal
/// block boundary location detected through full-file stride history analysis.
///
/// The archive layer consumes these hints to place solid block boundaries
/// at structurally meaningful transition points rather than arbitrary byte
/// offsets. This is one of the ways FLUX uses EOF detection as active
/// compression intelligence — only possible because we have seen the
/// complete file before finalizing the archive structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockBoundaryHint {
    /// Byte offset in the original uncompressed stream where the
    /// transition was detected.
    pub byte_offset: u64,
    /// Content type of the data before this boundary.
    pub content_type_before: ContentType,
    /// Content type of the data after this boundary.
    pub content_type_after: ContentType,
    /// Confirmed stride before the boundary, if any.
    pub stride_before: Option<u8>,
    /// Confirmed stride after the boundary, if any.
    pub stride_after: Option<u8>,
}
