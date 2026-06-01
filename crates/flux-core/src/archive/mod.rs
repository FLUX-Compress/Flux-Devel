//! # Archive Format Group
//!
//! ## Purpose
//! Manages solid archive files, formatting, directories, symlinks, and file indices.
//!
//! ## Architecture Diagram
//! ```text
//! +-------------------------------------------------------------+
//! |                       Archive Group                         |
//! |                                                             |
//! |   Directory Tree ──► solid grouping ──► Solid Blocks        |
//! |   Index Tracker ◄─── metadata ────────► [format.rs]         |
//! |                                                             |
//! +-------------------------------------------------------------+
//! ```
//!
//! ## Thread Safety
//! Thread safety in archive parsing and writing is maintained by isolating block
//! building from file system operations. Metadata is accumulated sequentially
//! and written using thread-safe channels.
//!
//! ## Why This Matters for Compression
//! Correct grouping of files (solid archives by similarity and type) allows the
//! dictionary and models to retain compression state across boundaries, dramatically
//! increasing compression ratios on groups of small, related files.
//!
//! ## See Also
//! * [`mod@format`]: Serialization format definitions.
//! * [`index`]: Solid archive file index reader/writer.
//! * [`solid`]: Grouping logic for files inside solid blocks.
//! * [`directory`]: OS directory and metadata storage.

pub mod format;
pub mod index;
pub mod solid;
pub mod directory;

pub use index::{ArchiveError, FileIndex};
pub use solid::{SolidBlockBuilder, SolidBlockGrouper};
pub use directory::{DirectoryTree, DirectoryEntry, EntryType, EntryMetadata};
