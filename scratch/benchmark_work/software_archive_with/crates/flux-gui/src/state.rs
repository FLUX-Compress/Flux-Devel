//! Application state management for the FLUX GUI.
//!
//! Handles background thread status monitoring and loading/saving
//! settings dynamically from the OS-specific application directory.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use flux::{Compression, CompressionStats, DecompressionStats, FluxError, Progress};

/// Background task execution state.
pub enum CompressionState {
    /// No operation is currently active.
    Idle,
    /// A solid compression task is actively running on a background thread.
    Running {
        /// Shared atomic reference to the progress updates.
        progress: Arc<Mutex<Progress>>,
        /// Thread join handle resolving to the compression statistics.
        handle: JoinHandle<Result<CompressionStats, FluxError>>,
        /// Instant when the compression started.
        start_time: std::time::Instant,
    },
    /// A solid extraction task is actively running on a background thread.
    RunningExtract {
        /// Shared atomic reference to the progress updates.
        progress: Arc<Mutex<Progress>>,
        /// Thread join handle resolving to the decompression statistics.
        handle: JoinHandle<Result<DecompressionStats, FluxError>>,
        /// Instant when the extraction started.
        start_time: std::time::Instant,
    },
    /// Compression completed successfully.
    Complete(CompressionStats),
    /// Decompression completed successfully.
    ExtractComplete(DecompressionStats),
    /// The operation failed with the given error message.
    Failed(String),
    /// The operation was cancelled by the user.
    Cancelled,
}

/// Available GUI application color themes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Theme {
    /// Follows the desktop environment system theme.
    #[default]
    System,
    /// Light mode theme layout.
    Light,
    /// Dark mode theme layout.
    Dark,
}

/// Persistent user configuration options.
#[derive(Debug, Clone)]
pub struct AppSettings {
    /// The default compression level for new archives.
    pub default_compression_level: Compression,
    /// Default output directory.
    pub default_output_dir: Option<PathBuf>,
    /// Thread count override. If None, auto-detected from hardware.
    pub thread_count_override: Option<usize>,
    /// Buffer size override in bytes. If None, defaults to 10% of system RAM.
    pub buffer_size_override: Option<usize>,
    /// Color theme.
    pub theme: Theme,
    /// History of the 5 most recently accessed archive files.
    pub recent_archives: VecDeque<PathBuf>,
}

// Implement custom serializer/deserializer mappings for Compression if not already derived.
// Note: In flux crate, Compression is Copy/Clone/Debug/PartialEq/Eq, let's verify if we need to implement serde manually.
// Wait, flux::Compression does NOT derive Serialize/Deserialize!
// Let's check flux::Compression:
// #[derive(Debug, Clone, Copy, PartialEq, Eq)]
// pub enum Compression { Fast, Balanced, Maximum }
// Because flux::Compression is defined in flux crate without Serde, we must serialize and deserialize it manually
// or write a helper wrapper.
// Since flux::Compression does not derive Serialize/Deserialize, we can wrap it or serialize it as a string.

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            default_compression_level: Compression::Balanced,
            default_output_dir: None,
            thread_count_override: None,
            buffer_size_override: None,
            theme: Theme::System,
            recent_archives: VecDeque::with_capacity(5),
        }
    }
}

impl AppSettings {
    /// Resolves the OS-specific directory for FLUX configuration files.
    fn config_file_path() -> Option<PathBuf> {
        let proj_dirs = directories::ProjectDirs::from("com", "FLUX", "FLUX")?;
        let config_dir = proj_dirs.config_dir();
        std::fs::create_dir_all(config_dir).ok()?;
        Some(config_dir.join("settings.json"))
    }

    /// Loads settings from the OS configuration directory, falling back to defaults on failure.
    pub fn load() -> Self {
        if let Some(path) = Self::config_file_path() {
            if path.exists() {
                if let Ok(file_content) = std::fs::read_to_string(path) {
                    if let Ok(settings_wrapper) =
                        serde_json::from_str::<AppSettingsRaw>(&file_content)
                    {
                        return settings_wrapper.into_settings();
                    }
                }
            }
        }
        Self::default()
    }

    /// Saves the current configuration to the OS configuration directory.
    pub fn save(&self) {
        if let Some(path) = Self::config_file_path() {
            let raw = AppSettingsRaw::from_settings(self);
            if let Ok(serialized) = serde_json::to_string_pretty(&raw) {
                let _ = std::fs::write(path, serialized);
            }
        }
    }

    /// Adds a path to the recent archives list, maintaining a maximum size of 5 entries.
    pub fn add_recent(&mut self, path: PathBuf) {
        // Remove duplicate entry if present
        self.recent_archives.retain(|p| p != &path);
        // Push to front
        self.recent_archives.push_front(path);
        // Cap size at 5
        if self.recent_archives.len() > 5 {
            self.recent_archives.truncate(5);
        }
        self.save();
    }
}

/// Helper struct for JSON serialization/deserialization to bridge non-serde types.
#[derive(Serialize, Deserialize)]
struct AppSettingsRaw {
    default_compression_level: String,
    default_output_dir: Option<PathBuf>,
    thread_count_override: Option<usize>,
    buffer_size_override: Option<usize>,
    theme: Theme,
    recent_archives: VecDeque<PathBuf>,
}

impl AppSettingsRaw {
    fn from_settings(settings: &AppSettings) -> Self {
        let level_str = match settings.default_compression_level {
            Compression::Tiny => "Tiny",
            Compression::Fast => "Fast",
            Compression::Balanced => "Balanced",
            Compression::Maximum => "Maximum",
            Compression::Extreme => "Extreme",
        };
        Self {
            default_compression_level: level_str.to_string(),
            default_output_dir: settings.default_output_dir.clone(),
            thread_count_override: settings.thread_count_override,
            buffer_size_override: settings.buffer_size_override,
            theme: settings.theme,
            recent_archives: settings.recent_archives.clone(),
        }
    }

    fn into_settings(self) -> AppSettings {
        let level = match self.default_compression_level.as_str() {
            "Tiny" => Compression::Tiny,
            "Fast" => Compression::Fast,
            "Maximum" => Compression::Maximum,
            "Extreme" => Compression::Extreme,
            _ => Compression::Balanced,
        };
        AppSettings {
            default_compression_level: level,
            default_output_dir: self.default_output_dir,
            thread_count_override: self.thread_count_override,
            buffer_size_override: self.buffer_size_override,
            theme: self.theme,
            recent_archives: self.recent_archives,
        }
    }
}
