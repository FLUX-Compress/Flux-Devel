//! Reusable file and folder picker widget.
//!
//! Integrates rfd (Rust File Dialogs) for cross-platform native selection
//! of input/output files and directories.

use std::path::PathBuf;
use egui::Ui;

/// Component managing native file or folder selection dialogs and path display.
pub struct FilePicker {
    /// The currently selected path, if any.
    pub path: Option<PathBuf>,
    /// Whether this picker is configured to select directories/folders.
    pub pick_folders: bool,
    /// List of file extensions allowed for picking (e.g. `["flx"]`).
    pub allowed_extensions: Option<Vec<String>>,
    /// Whether this picker is a save dialog instead of an open dialog.
    pub is_save: bool,
}

impl FilePicker {
    /// Initializes a new FilePicker with custom settings.
    pub fn new(pick_folders: bool, allowed_extensions: Option<Vec<String>>) -> Self {
        Self {
            path: None,
            pick_folders,
            allowed_extensions,
            is_save: false,
        }
    }

    /// Initializes a new FilePicker specifically for saving files.
    pub fn new_save(allowed_extensions: Option<Vec<String>>) -> Self {
        Self {
            path: None,
            pick_folders: false,
            allowed_extensions,
            is_save: true,
        }
    }

    /// Renders the path text display and the "Browse..." button.
    ///
    /// Returns the current selected path.
    pub fn show(&mut self, ui: &mut Ui, label: &str) -> Option<PathBuf> {
        ui.horizontal(|ui| {
            ui.label(label);
            
            let mut path_str = match &self.path {
                Some(p) => p.to_string_lossy().to_string(),
                None => "No path selected...".to_string(),
            };
            
            // Read-only text area showing path
            ui.add(
                egui::TextEdit::singleline(&mut path_str)
                    .interactive(false)
                    .desired_width(320.0),
            );
            
            if ui.button("Browse...").clicked() {
                let mut dialog = rfd::FileDialog::new();
                
                if let Some(ref exts) = self.allowed_extensions {
                    let ref_exts: Vec<&str> = exts.iter().map(|s| s.as_str()).collect();
                    dialog = dialog.add_filter("FLUX Archive Files", &ref_exts);
                }
                
                let picked_path = if self.pick_folders {
                    dialog.pick_folder()
                } else if self.is_save {
                    dialog.save_file()
                } else {
                    dialog.pick_file()
                };

                if let Some(p) = picked_path {
                    self.path = Some(p);
                }
            }
        });

        self.path.clone()
    }
}
