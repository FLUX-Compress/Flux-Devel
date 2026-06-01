//! Extraction configuration view.
//!
//! Handles archive file input, reads archive headers to display info previews,
//! prompts for passwords if encrypted, and spawns decompression background threads.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use egui::{Button, RichText, Ui};

use flux::Archive;
use crate::app::FluxApp;
use crate::app::AppView;
use crate::components::file_picker::FilePicker;
use crate::components::password_field::PasswordField;
use crate::state::CompressionState;
use crate::views::compress::format_size;

/// Basic metadata extracted from the archive header/index.
#[derive(Debug, Clone)]
pub struct ArchiveInfo {
    pub original_size: u64,
    pub compressed_size: u64,
    pub file_count: u32,
    pub is_encrypted: bool,
}

/// State for the Extract View.
pub struct ExtractView {
    /// File picker for selecting the `.flx` archive.
    pub picker: FilePicker,
    /// Directory picker for extraction output destination.
    pub output_picker: FilePicker,
    /// Password entry widget.
    pub password_field: PasswordField,
    /// Extracted archive header info, if successfully read.
    pub archive_info: Option<ArchiveInfo>,
    /// Error message during metadata parsing.
    pub metadata_error: Option<String>,
    /// Tracks if the currently selected file is encrypted (read via header byte 6).
    pub is_encrypted: bool,
    /// Cache last input path to prevent redundant scans.
    last_archive_path: Option<PathBuf>,
    /// Cache last password to prevent redundant scans.
    last_password: Option<String>,
}

impl Default for ExtractView {
    fn default() -> Self {
        Self {
            picker: FilePicker::new(false, Some(vec!["flx".to_string()])),
            output_picker: FilePicker::new(true, None),
            password_field: PasswordField::default(),
            archive_info: None,
            metadata_error: None,
            is_encrypted: false,
            last_archive_path: None,
            last_password: None,
        }
    }
}

impl ExtractView {
    /// Checks the encryption byte and attempts to load the archive index metadata.
    pub fn read_archive_metadata(&mut self) {
        let current_path = self.picker.path.clone();
        let current_pass = self.password_field.password.clone();

        if current_path != self.last_archive_path || current_pass != self.last_password.as_deref().unwrap_or_default() {
            self.last_archive_path = current_path.clone();
            self.last_password = Some(current_pass.clone());

            if let Some(ref path) = current_path {
                // 1. Detect encryption quickly using header byte 6
                self.is_encrypted = is_file_encrypted(path);

                // 2. Read full metadata
                let builder = Archive::extract(path);
                let builder = if self.is_encrypted && !current_pass.is_empty() {
                    builder.password(current_pass.clone())
                } else {
                    builder
                };

                match builder.read_metadata() {
                    Ok((orig, comp, files, enc)) => {
                        self.archive_info = Some(ArchiveInfo {
                            original_size: orig,
                            compressed_size: comp,
                            file_count: files,
                            is_encrypted: enc,
                        });
                        self.metadata_error = None;
                    }
                    Err(e) => {
                        self.archive_info = None;
                        if self.is_encrypted && current_pass.is_empty() {
                            self.metadata_error = Some("Password required to view metadata".to_string());
                        } else {
                            match e {
                                flux::FluxError::WrongPassword => {
                                    self.metadata_error = Some("Incorrect password".to_string());
                                }
                                other => {
                                    self.metadata_error = Some(other.to_string());
                                }
                            }
                        }
                    }
                }
            } else {
                self.archive_info = None;
                self.metadata_error = None;
                self.is_encrypted = false;
            }
        }
    }
}

/// Parses the 7th byte of the FLUX archive file to check if it's encrypted.
fn is_file_encrypted(path: &Path) -> bool {
    if let Ok(mut f) = std::fs::File::open(path) {
        let mut buf = [0u8; 7];
        if f.read_exact(&mut buf).is_ok() {
            return buf[6] != 0;
        }
    }
    false
}

/// Renders the Extract view.
pub fn show(app: &mut FluxApp, ui: &mut Ui) {
    let view = &mut app.extract_view;

    ui.vertical(|ui| {
        ui.heading(RichText::new("🔓 Extract Solid Archive").strong());
        ui.add_space(10.0);

        // --- SOURCE SECTION ---
        ui.group(|ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new("Source Archive (.flx)").strong());
                view.picker.show(ui, "Select Archive:");
            });
        });

        // Trigger metadata parsing
        view.read_archive_metadata();

        ui.add_space(10.0);

        // --- METADATA PREVIEW PANEL ---
        if view.picker.path.is_some() {
            ui.group(|ui| {
                ui.vertical(|ui| {
                    ui.label(RichText::new("Archive Metadata").strong());
                    ui.add_space(5.0);

                    if let Some(ref info) = view.archive_info {
                        ui.horizontal(|ui| {
                            ui.label("Files Preserved:");
                            ui.label(RichText::new(info.file_count.to_string()).strong());
                        });
                        ui.horizontal(|ui| {
                            ui.label("Uncompressed Size:");
                            ui.label(RichText::new(format_size(info.original_size)).strong());
                        });
                        ui.horizontal(|ui| {
                            ui.label("Archive Size:");
                            ui.label(RichText::new(format_size(info.compressed_size)).strong());
                        });
                        ui.horizontal(|ui| {
                            ui.label("Encrypted:");
                            if info.is_encrypted {
                                ui.colored_label(egui::Color32::from_rgb(220, 180, 50), "Yes 🔒 (AES-256-GCM)");
                            } else {
                                ui.colored_label(egui::Color32::from_rgb(50, 180, 50), "No 🔓");
                            }
                        });
                    } else if let Some(ref err) = view.metadata_error {
                        ui.horizontal(|ui| {
                            ui.colored_label(egui::Color32::from_rgb(220, 50, 50), "⚠️ Status:");
                            ui.label(err);
                        });
                    } else {
                        ui.label("Reading archive header...");
                    }
                });
            });
            ui.add_space(10.0);
        }

        // --- OUTPUT SECTION ---
        ui.group(|ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new("Extraction Destination").strong());
                view.output_picker.show(ui, "Extract To:");
            });
        });

        ui.add_space(10.0);

        // --- PASSWORD INPUT (IF ENCRYPTED) ---
        if view.is_encrypted {
            ui.group(|ui| {
                ui.vertical(|ui| {
                    ui.label(RichText::new("Password Required").strong());
                    ui.add_space(5.0);
                    view.password_field.show(ui, "Enter Password:");
                });
            });
            ui.add_space(10.0);
        }

        // --- ACTION BUTTONS ---
        let has_input = view.picker.path.is_some();
        let has_output = view.output_picker.path.is_some();
        let password_ok = !view.is_encrypted || !view.password_field.password.is_empty();
        let can_start = has_input && has_output && password_ok && view.archive_info.is_some();

        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let extract_btn = Button::new(RichText::new("🔓 Start Extraction").size(15.0).strong());
                if ui.add_enabled(can_start, extract_btn).clicked() {
                    let in_path = view.picker.path.clone().unwrap();
                    let out_path = view.output_picker.path.clone().unwrap();
                    let pass = if view.is_encrypted {
                        Some(view.password_field.password.clone())
                    } else {
                        None
                    };

                    // Transition to Progress view
                    app.current_view = AppView::Progress;

                    let bytes_total = view.archive_info.as_ref().map(|i| i.original_size).unwrap_or(0);
                    // Start background thread extraction
                    start_extraction(
                        &mut app.compression_state,
                        in_path,
                        out_path,
                        pass,
                        bytes_total,
                        ui.ctx().clone(),
                    );
                }
            });
        });
    });
}

/// Spawns the background extraction task.
fn start_extraction(
    state: &mut CompressionState,
    input: PathBuf,
    output: PathBuf,
    password: Option<String>,
    bytes_total: u64,
    ctx: egui::Context,
) {
    let progress = Arc::new(std::sync::Mutex::new(flux::Progress::new(
        0.0,
        input.to_string_lossy().into_owned(),
        0,
        bytes_total,
        None,
    )));
    let progress_clone = Arc::clone(&progress);

    let handle = std::thread::spawn(move || {
        let update_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let update_count_clone = Arc::clone(&update_count);
        let mut builder = Archive::extract(input)
            .output(output)
            .verify(true)
            .on_progress(move |p| {
                let cnt = update_count_clone.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                flux_core_v1::flux_debug!("[DEBUG] Extraction progress callback update #{}: percent={:.1}%, bytes_processed={}", cnt, p.percent(), p.bytes_processed());
                if let Ok(mut guard) = progress_clone.lock() {
                    *guard = p;
                }
                ctx.request_repaint();
            });

        if let Some(pass) = password {
            builder = builder.password(pass);
        }

        builder.run()
    });

    *state = CompressionState::RunningExtract { progress, handle, start_time: std::time::Instant::now() };
}
