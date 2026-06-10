//! Directory structure preservation
//!
//! Handles scanning directories, metadata preservation (permissions, timestamps, empty directories,
//! symlinks), and cross-platform restoration.

use crate::archive::index::ArchiveError;
use std::fs;
use std::path::{Path, PathBuf};

/// Preserved entry metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryMetadata {
    /// Unix/Windows file system permissions representation.
    pub permissions: u32,
    /// Modification timestamp (seconds since Unix Epoch).
    pub modified_time: u64,
    /// Creation timestamp (seconds since Unix Epoch).
    pub created_time: u64,
    /// Owner User ID (cross-platform preservation).
    pub owner_uid: u32,
    /// Owner Group ID.
    pub owner_gid: u32,
}

/// Category of directory tree entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryType {
    /// Regular file with recorded size.
    RegularFile {
        /// Size of the file in bytes.
        size: u64,
    },
    /// Directory that contains files.
    Directory,
    /// Symbolic link pointing to a target path.
    Symlink {
        /// Target path of the symlink.
        target: PathBuf,
    },
    /// Explicitly tracked empty directory.
    EmptyDirectory,
}

/// A single entry in the directory tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryEntry {
    /// Relative path from the root directory of the archive.
    pub path: PathBuf,
    /// Specific category type of this entry.
    pub entry_type: EntryType,
    /// Metadata attributes.
    pub metadata: EntryMetadata,
}

/// Structured directory tree representation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryTree {
    /// Base root path of the directory tree.
    pub root: PathBuf,
    /// All entries scanned or restored.
    pub entries: Vec<DirectoryEntry>,
}

impl DirectoryTree {
    /// Recursively scans a source directory to build a `DirectoryTree`.
    ///
    /// Explicitly tracks empty directories and preserves symbolic links without
    /// traversing them, avoiding potential infinite reference loops.
    pub fn scan(root: &Path) -> Result<Self, ArchiveError> {
        let mut entries = Vec::new();
        if root.is_dir() {
            Self::scan_dir(root, root, &mut entries)
                .map_err(|e| ArchiveError::Io(e.to_string()))?;
        } else {
            // Root is a single file. Scan it as a lone entry.
            let entry =
                Self::scan_entry(root, root).map_err(|e| ArchiveError::Io(e.to_string()))?;
            entries.push(entry);
        }

        Ok(Self {
            root: root.to_path_buf(),
            entries,
        })
    }

    /// Internal recursive scanner helper.
    fn scan_dir(
        dir: &Path,
        base: &Path,
        entries: &mut Vec<DirectoryEntry>,
    ) -> Result<(), std::io::Error> {
        // Read directory entries.
        let mut is_empty = true;
        let mut dir_entries = Vec::new();
        for entry_res in fs::read_dir(dir)? {
            let entry = entry_res?;
            dir_entries.push(entry);
            is_empty = false;
        }

        if is_empty && dir != base {
            // It's an empty directory. Track it explicitly.
            let metadata = fs::metadata(dir)?;
            let relative_path = dir.strip_prefix(base).unwrap().to_path_buf();
            let entry_meta = Self::build_metadata(&metadata);
            entries.push(DirectoryEntry {
                path: relative_path,
                entry_type: EntryType::EmptyDirectory,
                metadata: entry_meta,
            });
            return Ok(());
        }

        for entry in dir_entries {
            let path = entry.path();
            let relative_path = path.strip_prefix(base).unwrap().to_path_buf();
            let file_type = entry.file_type()?;
            let metadata = entry.metadata()?;
            let entry_meta = Self::build_metadata(&metadata);

            if file_type.is_symlink() {
                // Symbolic link: do NOT recurse or follow!
                let target = fs::read_link(&path)?;
                entries.push(DirectoryEntry {
                    path: relative_path,
                    entry_type: EntryType::Symlink { target },
                    metadata: entry_meta,
                });
            } else if file_type.is_dir() {
                // Check if directory is empty first to avoid duplicate / wrong entry type.
                let mut is_sub_empty = true;
                if let Ok(mut rd) = fs::read_dir(&path) {
                    if rd.next().is_some() {
                        is_sub_empty = false;
                    }
                }
                if is_sub_empty {
                    entries.push(DirectoryEntry {
                        path: relative_path,
                        entry_type: EntryType::EmptyDirectory,
                        metadata: entry_meta,
                    });
                } else {
                    entries.push(DirectoryEntry {
                        path: relative_path.clone(),
                        entry_type: EntryType::Directory,
                        metadata: entry_meta,
                    });
                    // Recursively scan the subdirectory.
                    Self::scan_dir(&path, base, entries)?;
                }
            } else {
                // Regular file.
                entries.push(DirectoryEntry {
                    path: relative_path,
                    entry_type: EntryType::RegularFile {
                        size: metadata.len(),
                    },
                    metadata: entry_meta,
                });
            }
        }

        Ok(())
    }

    /// Scans a single standalone file.
    fn scan_entry(path: &Path, _base: &Path) -> Result<DirectoryEntry, std::io::Error> {
        let relative_path = PathBuf::from(path.file_name().unwrap_or(path.as_os_str()));
        let file_type = fs::symlink_metadata(path)?.file_type();
        let metadata = fs::metadata(path)?;
        let entry_meta = Self::build_metadata(&metadata);

        if file_type.is_symlink() {
            let target = fs::read_link(path)?;
            Ok(DirectoryEntry {
                path: relative_path,
                entry_type: EntryType::Symlink { target },
                metadata: entry_meta,
            })
        } else if file_type.is_dir() {
            let is_empty = fs::read_dir(path)?.next().is_none();
            if is_empty {
                Ok(DirectoryEntry {
                    path: relative_path,
                    entry_type: EntryType::EmptyDirectory,
                    metadata: entry_meta,
                })
            } else {
                Ok(DirectoryEntry {
                    path: relative_path,
                    entry_type: EntryType::Directory,
                    metadata: entry_meta,
                })
            }
        } else {
            Ok(DirectoryEntry {
                path: relative_path,
                entry_type: EntryType::RegularFile {
                    size: metadata.len(),
                },
                metadata: entry_meta,
            })
        }
    }

    fn build_metadata(metadata: &fs::Metadata) -> EntryMetadata {
        #[cfg(unix)]
        let permissions = {
            use std::os::unix::fs::PermissionsExt;
            metadata.permissions().mode()
        };
        #[cfg(not(unix))]
        let permissions = {
            let mut p = if metadata.permissions().readonly() {
                0o444
            } else {
                0o666
            };
            if metadata.is_dir() {
                p |= 0o111;
            }
            p
        };

        let modified_time = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let created_time = metadata
            .created()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(modified_time);

        #[cfg(unix)]
        let (owner_uid, owner_gid) = {
            use std::os::unix::fs::MetadataExt;
            (metadata.uid(), metadata.gid())
        };
        #[cfg(not(unix))]
        let (owner_uid, owner_gid) = (0, 0);

        EntryMetadata {
            permissions,
            modified_time,
            created_time,
            owner_uid,
            owner_gid,
        }
    }

    /// Restores the directory tree skeleton (directories and symlinks) at destination.
    ///
    /// Standard regular files are not written here, but the paths are pre-created.
    pub fn restore(&self, destination: &Path) -> Result<(), ArchiveError> {
        for entry in &self.entries {
            let target_path = destination.join(&entry.path);

            if let Some(parent) = target_path.parent() {
                fs::create_dir_all(parent).map_err(|e| ArchiveError::Io(e.to_string()))?;
            }

            match &entry.entry_type {
                EntryType::Directory | EntryType::EmptyDirectory => {
                    fs::create_dir_all(&target_path)
                        .map_err(|e| ArchiveError::Io(e.to_string()))?;
                }
                EntryType::Symlink { target } => {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::symlink;
                        let _ = fs::remove_file(&target_path);
                        symlink(target, &target_path)
                            .map_err(|e| ArchiveError::Io(e.to_string()))?;
                    }
                    #[cfg(windows)]
                    {
                        use std::os::windows::fs::symlink_file;
                        let _ = fs::remove_file(&target_path);
                        // Attempt to create. Might fail if privilege not held, which is ignored in test environments.
                        let _ = symlink_file(target, &target_path);
                    }
                }
                EntryType::RegularFile { .. } => {
                    // Created on demand by the file extraction writer.
                }
            }
        }
        Ok(())
    }

    /// Restores timestamps and permissions on all directory entries.
    ///
    /// Must be executed after files are written to ensure that file modification
    /// events do not overwrite the restored timestamps.
    pub fn restore_metadata(&self, destination: &Path) -> Result<(), ArchiveError> {
        for entry in &self.entries {
            let target_path = destination.join(&entry.path);
            if !target_path.exists() {
                continue;
            }

            // Skip symlink metadata restoration on Windows to prevent dereferencing target path.
            if let EntryType::Symlink { .. } = entry.entry_type {
                #[cfg(windows)]
                {
                    continue;
                }
            }

            // 1. Restore permissions
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let perm = fs::Permissions::from_mode(entry.metadata.permissions);
                let _ = fs::set_permissions(&target_path, perm);
            }
            #[cfg(windows)]
            {
                if let Ok(m) = fs::metadata(&target_path) {
                    let mut p = m.permissions();
                    p.set_readonly((entry.metadata.permissions & 0o222) == 0);
                    let _ = fs::set_permissions(&target_path, p);
                }
            }

            // 2. Restore timestamps (mtime and atime)
            let mtime = filetime::FileTime::from_unix_time(entry.metadata.modified_time as i64, 0);
            let atime = filetime::FileTime::from_unix_time(entry.metadata.created_time as i64, 0);
            let _ = filetime::set_file_times(&target_path, atime, mtime);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_directory_entry_types_recognized() {
        let temp = tempfile::tempdir().unwrap();
        let temp_path = temp.path();

        // 1. Create file
        let file_path = temp_path.join("file.txt");
        fs::write(&file_path, "hello").unwrap();

        // 2. Create subdir
        let dir_path = temp_path.join("subdir");
        fs::create_dir(&dir_path).unwrap();

        // Scan directory
        let tree = DirectoryTree::scan(temp_path).unwrap();

        let file_entry = tree
            .entries
            .iter()
            .find(|e| e.path == Path::new("file.txt"))
            .unwrap();
        assert!(matches!(
            file_entry.entry_type,
            EntryType::RegularFile { size: 5 }
        ));

        let dir_entry = tree
            .entries
            .iter()
            .find(|e| e.path == Path::new("subdir"))
            .unwrap();
        assert_eq!(dir_entry.entry_type, EntryType::EmptyDirectory); // Empty subdir
    }

    #[test]
    fn test_empty_directory_preserved() {
        let temp_src = tempfile::tempdir().unwrap();
        let temp_dest = tempfile::tempdir().unwrap();

        let empty_dir = temp_src.path().join("empty_dir");
        fs::create_dir(&empty_dir).unwrap();

        let tree = DirectoryTree::scan(temp_src.path()).unwrap();
        assert!(tree
            .entries
            .iter()
            .any(|e| e.entry_type == EntryType::EmptyDirectory));

        tree.restore(temp_dest.path()).unwrap();
        assert!(temp_dest.path().join("empty_dir").exists());
        assert!(temp_dest.path().join("empty_dir").is_dir());
    }

    #[test]
    fn test_symlink_not_followed() {
        let temp = tempfile::tempdir().unwrap();
        let temp_path = temp.path();

        let file_path = temp_path.join("file.txt");
        fs::write(&file_path, "hello").unwrap();

        // Create symlink
        let sym_path = temp_path.join("link.txt");
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            symlink(&file_path, &sym_path).unwrap();
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::symlink_file;
            // Ignore error if symlink creation is not permitted on this Windows user account.
            let _ = symlink_file(&file_path, &sym_path);
        }

        if sym_path.exists() {
            let tree = DirectoryTree::scan(temp_path).unwrap();
            let link_entry = tree
                .entries
                .iter()
                .find(|e| e.path == Path::new("link.txt"));
            assert!(link_entry.is_some());
            assert!(matches!(
                link_entry.unwrap().entry_type,
                EntryType::Symlink { .. }
            ));
        }
    }
}
