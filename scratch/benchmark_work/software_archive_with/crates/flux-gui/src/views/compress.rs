//! Compression configuration view.
//!
//! Provides file and folder inputs, auto-suggests output paths,
//! renders level cards, handles password settings, and triggers background threads.

use egui::{Button, RichText, Ui};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::app::AppView;
use crate::app::FluxApp;
use crate::components::file_picker::FilePicker;
use crate::components::password_field::PasswordField;
use crate::state::CompressionState;
use flux::{Archive, Compression};

/// State for the Compress Configuration View.
pub struct CompressView {
    /// Picker for input folder or file.
    pub picker: FilePicker,
    /// Picker for output archive destination.
    pub output_picker: FilePicker,
    /// Selected compression level.
    pub level: Compression,
    /// Whether password encryption is checked.
    pub use_password: bool,
    /// Password entry input widget.
    pub password_field: PasswordField,
    /// Scanned file count under the input path.
    pub input_file_count: u32,
    /// Total size in bytes of scanned input.
    pub input_total_size: u64,
    /// Post-compress integrity verification.
    pub verify_after: bool,
    /// Track last input path to run rescans only when changed.
    last_input_path: Option<PathBuf>,
}

impl Default for CompressView {
    fn default() -> Self {
        Self {
            picker: FilePicker::new(false, None), // we'll override pick behavior using manual buttons
            output_picker: FilePicker::new_save(Some(vec!["flx".to_string()])),
            level: Compression::Balanced,
            use_password: false,
            password_field: PasswordField::default(),
            input_file_count: 0,
            input_total_size: 0,
            verify_after: true,
            last_input_path: None,
        }
    }
}

impl CompressView {
    /// Auto-suggests the output file path based on selected input.
    pub fn auto_suggest_output(&mut self) {
        if let Some(ref in_path) = self.picker.path {
            let mut out_path = in_path.clone();
            if in_path.is_file() {
                out_path.set_extension("flx");
            } else {
                let name = in_path.file_name().unwrap_or_default();
                out_path = in_path
                    .parent()
                    .unwrap_or(in_path)
                    .join(name)
                    .with_extension("flx");
            }
            self.output_picker.path = Some(out_path);
        }
    }

    /// Recursively counts files and sums sizes to update previews.
    pub fn scan_input(&mut self) {
        if self.picker.path != self.last_input_path {
            self.last_input_path = self.picker.path.clone();

            let mut count = 0;
            let mut size = 0;

            if let Some(ref path) = self.picker.path {
                if path.is_file() {
                    count = 1;
                    size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
                } else if path.is_dir() {
                    count_dir(path, &mut count, &mut size);
                }
            }

            self.input_file_count = count;
            self.input_total_size = size;
        }
    }
}

/// Helper function to recursively count files and size under a folder path.
fn count_dir(path: &Path, count: &mut u32, size: &mut u64) {
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            if let Ok(meta) = entry.metadata() {
                if meta.is_file() {
                    *count += 1;
                    *size += meta.len();
                } else if meta.is_dir() {
                    count_dir(&entry.path(), count, size);
                }
            }
        }
    }
}

/// Helper function to format sizes in bytes to human-readable strings.
pub fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} bytes", bytes)
    }
}

/// Renders the Compress configuration screen.
pub fn show(app: &mut FluxApp, ui: &mut Ui) {
    let view = &mut app.compress_view;

    ui.vertical(|ui| {
        ui.heading(RichText::new("📦 Create Solid Archive").strong());
        ui.add_space(10.0);

        // --- INPUT SECTION ---
        ui.group(|ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new("Input Path").strong());

                ui.horizontal(|ui| {
                    let mut path_str = match &view.picker.path {
                        Some(p) => p.to_string_lossy().to_string(),
                        None => "No input selected...".to_string(),
                    };
                    ui.add(
                        egui::TextEdit::singleline(&mut path_str)
                            .interactive(false)
                            .desired_width(320.0),
                    );

                    if ui.button("📁 Browse Folder").clicked() {
                        if let Some(p) = rfd::FileDialog::new().pick_folder() {
                            view.picker.path = Some(p);
                            view.auto_suggest_output();
                        }
                    }
                    if ui.button("📄 Browse File").clicked() {
                        if let Some(p) = rfd::FileDialog::new().pick_file() {
                            view.picker.path = Some(p);
                            view.auto_suggest_output();
                        }
                    }
                });

                // Input type and scans
                view.scan_input();
                if let Some(ref path) = view.picker.path {
                    let kind = if path.is_dir() {
                        "Directory"
                    } else {
                        "Regular File"
                    };
                    ui.label(format!(
                        "Type: {} | Scanned: {} files ({})",
                        kind,
                        view.input_file_count,
                        format_size(view.input_total_size)
                    ));
                } else {
                    ui.label(RichText::new("Please select a file or folder to compress.").weak());
                }
            });
        });

        ui.add_space(10.0);

        // --- OUTPUT SECTION ---
        ui.group(|ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new("Output Archive Location").strong());
                view.output_picker.show(ui, "Save As:");
            });
        });

        ui.add_space(10.0);

        // --- OPTIONS SECTION ---
        ui.group(|ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new("Compression Level").strong());
                ui.add_space(5.0);

                // Side-by-side cards with wrapping
                ui.horizontal_wrapped(|ui| {
                    let levels = [
                        (
                            Compression::Tiny,
                            "Tiny",
                            "Embedded/constrained",
                            "Window: 256KB | Ratio: Low",
                        ),
                        (
                            Compression::Fast,
                            "Fast",
                            "Prioritizes speed",
                            "Window: 4MB | Ratio: Med-Low",
                        ),
                        (
                            Compression::Balanced,
                            "Balanced",
                            "Optimal compromise",
                            "Window: 32MB | Ratio: Med",
                        ),
                        (
                            Compression::Maximum,
                            "Maximum",
                            "Prioritizes ratio",
                            "Window: 128MB | Ratio: High",
                        ),
                        (
                            Compression::Extreme,
                            "Extreme",
                            "Big data / big RAM",
                            "Window: 256MB | Ratio: Peak",
                        ),
                    ];

                    for (level, name, desc, tradeoff) in levels {
                        let is_selected = view.level == level;

                        let card_size = egui::vec2(140.0, 80.0);
                        let (rect, response) =
                            ui.allocate_exact_size(card_size, egui::Sense::click());
                        if response.clicked() {
                            view.level = level;
                        }

                        let border_color = if is_selected {
                            egui::Color32::from_rgb(0, 180, 220)
                        } else {
                            ui.visuals().widgets.noninteractive.bg_stroke.color
                        };
                        let fill_color = if is_selected {
                            ui.visuals().extreme_bg_color
                        } else {
                            ui.visuals().window_fill()
                        };

                        ui.painter().rect(
                            rect,
                            6.0,
                            fill_color,
                            egui::Stroke::new(if is_selected { 2.0 } else { 1.0 }, border_color),
                        );

                        let layout_rect = rect.shrink(8.0);
                        ui.child_ui(layout_rect, egui::Layout::top_down(egui::Align::Min))
                            .vertical(|ui| {
                                ui.label(RichText::new(name).strong());
                                ui.label(RichText::new(desc).size(9.0).weak());
                                ui.label(
                                    RichText::new(tradeoff)
                                        .size(8.0)
                                        .color(egui::Color32::from_rgb(0, 180, 220)),
                                );
                            });
                    }
                });

                ui.add_space(10.0);

                // Password toggle
                ui.checkbox(&mut view.use_password, "Enable Password Encryption");
                if view.use_password {
                    ui.add_space(5.0);
                    view.password_field.show(ui, "Password:");
                }

                ui.add_space(5.0);
                ui.checkbox(
                    &mut view.verify_after,
                    "Verify archive integrity after compression",
                );
            });
        });

        ui.add_space(10.0);

        // --- PREVIEW & ACTION ---
        let can_start = view.picker.path.is_some() && view.output_picker.path.is_some();

        ui.horizontal(|ui| {
            // Estimated size calculations
            if can_start {
                let ratio = match view.level {
                    Compression::Tiny => 0.60,
                    Compression::Fast => 0.50,
                    Compression::Balanced => 0.35,
                    Compression::Maximum => 0.25,
                    Compression::Extreme => 0.20,
                };
                let est_bytes = (view.input_total_size as f64 * ratio) as u64;

                ui.vertical(|ui| {
                    ui.label(
                        RichText::new(format!(
                            "Estimated Archive Size: ~{}",
                            format_size(est_bytes)
                        ))
                        .strong(),
                    );
                    ui.label(
                        RichText::new(format!("Total Files: {}", view.input_file_count)).weak(),
                    );
                });
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let start_btn =
                    Button::new(RichText::new("🚀 Start Compression").size(15.0).strong());
                if ui.add_enabled(can_start, start_btn).clicked() {
                    // Collect options
                    let in_path = view.picker.path.clone().unwrap();
                    let out_path = view.output_picker.path.clone().unwrap();
                    let level = view.level;
                    let pass = if view.use_password {
                        let p = view.password_field.password.clone();
                        if p.is_empty() {
                            None
                        } else {
                            Some(p)
                        }
                    } else {
                        None
                    };
                    let verify = view.verify_after;

                    // Transition to Progress view
                    app.current_view = AppView::Progress;

                    // Start background thread execution
                    start_compression(
                        &mut app.compression_state,
                        in_path,
                        out_path,
                        level,
                        pass,
                        verify,
                        view.input_total_size,
                        ui.ctx().clone(),
                    );
                }
            });
        });
    });
}

/// Spawns the background compression task.
#[allow(clippy::too_many_arguments)]
fn start_compression(
    state: &mut CompressionState,
    input: PathBuf,
    output: PathBuf,
    level: Compression,
    password: Option<String>,
    verify_after: bool,
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

    let input_dbg = input.clone();
    let output_dbg = output.clone();
    flux_core_v1::flux_debug!(
        "[DEBUG] Spawning compression thread. Input: {:?}, Output: {:?}",
        input_dbg,
        output_dbg
    );

    let handle = std::thread::spawn(move || {
        let update_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let update_count_clone = Arc::clone(&update_count);
        let mut builder = Archive::compress(input)
            .output(output)
            .level(level)
            .verify(verify_after)
            .on_progress(move |p| {
                let cnt = update_count_clone.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                flux_core_v1::flux_debug!("[DEBUG] Compression progress callback update #{}: percent={:.1}%, bytes_processed={}", cnt, p.percent(), p.bytes_processed());
                if let Ok(mut guard) = progress_clone.lock() {
                    *guard = p;
                }
                ctx.request_repaint();
            });

        if let Some(pass) = password {
            builder = builder.password(pass);
        }

        let res = builder.run();
        match &res {
            Ok(stats) => {
                flux_core_v1::flux_debug!(
                    "[DEBUG] .run() returned Ok. Bytes written: {}",
                    stats.compressed_size()
                );
            }
            Err(err) => {
                flux_core_v1::flux_debug!("[DEBUG] .run() returned Err: {:?}", err);
            }
        }
        res
    });

    *state = CompressionState::Running {
        progress,
        handle,
        start_time: std::time::Instant::now(),
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_gui_compression_logic() {
        let temp_dir = std::env::temp_dir();
        let input_path = temp_dir.join("flux_test_gui_input.txt");
        let output_path = temp_dir.join("flux_test_gui_output.flx");

        // Cleanup any leftovers
        let _ = std::fs::remove_file(&input_path);
        let _ = std::fs::remove_file(&output_path);

        // Write some text to input file
        {
            let mut file = std::fs::File::create(&input_path).unwrap();
            file.write_all(b"Hello World! This is a test file for compression testing via GUI logic. Hello World! This is a test file for compression testing via GUI logic.").unwrap();
        }

        let mut state = CompressionState::Idle;
        start_compression(
            &mut state,
            input_path.clone(),
            output_path.clone(),
            Compression::Balanced,
            None,
            true, // verify_after = true
            0,
            egui::Context::default(),
        );

        // Poll/wait for it to finish
        let start = std::time::Instant::now();
        loop {
            if start.elapsed().as_secs() > 10 {
                panic!("Compression timed out!");
            }

            let is_finished = match &state {
                CompressionState::Running { handle, .. } => handle.is_finished(),
                _ => false,
            };

            if is_finished {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        // Join/process completion just like app.rs does
        let active_state = std::mem::replace(&mut state, CompressionState::Idle);
        match active_state {
            CompressionState::Running { handle, .. } => match handle.join() {
                Ok(Ok(stats)) => {
                    println!("[TEST] Compression succeeded!");
                    println!("[TEST] Files processed: {}", stats.files_processed());
                    println!("[TEST] Original size: {}", stats.original_size());
                    println!("[TEST] Compressed size: {}", stats.compressed_size());
                    state = CompressionState::Complete(stats);
                }
                Ok(Err(e)) => {
                    println!("[TEST] Compression failed with error: {:?}", e);
                    state = CompressionState::Failed(e.to_string());
                }
                Err(_) => {
                    println!("[TEST] Thread panicked");
                    panic!("Thread panicked");
                }
            },
            _ => panic!("State was not Running!"),
        }

        // Check output file
        if output_path.exists() {
            let metadata = std::fs::metadata(&output_path).unwrap();
            println!("[TEST] Output file size: {} bytes", metadata.len());
        } else {
            println!("[TEST] Output file does not exist!");
        }

        // Cleanup
        let _ = std::fs::remove_file(&input_path);
        let _ = std::fs::remove_file(&output_path);

        match state {
            CompressionState::Complete(_) => {}
            CompressionState::Failed(e) => {
                panic!("Compression failed: {}", e);
            }
            _ => panic!("Expected Complete or Failed"),
        }
    }

    #[test]
    fn test_gui_compression_failure() {
        let temp_dir = std::env::temp_dir();
        let input_path = temp_dir.join("flux_non_existent_dir_xyz");
        let output_path = temp_dir.join("flux_test_gui_failed_output.flx");

        let _ = std::fs::remove_file(&output_path);

        let mut state = CompressionState::Idle;
        start_compression(
            &mut state,
            input_path.clone(),
            output_path.clone(),
            Compression::Balanced,
            None,
            true,
            0,
            egui::Context::default(),
        );

        // Poll/wait for it to finish
        let start = std::time::Instant::now();
        loop {
            if start.elapsed().as_secs() > 10 {
                panic!("Compression timed out!");
            }

            let is_finished = match &state {
                CompressionState::Running { handle, .. } => handle.is_finished(),
                _ => false,
            };

            if is_finished {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        // Join/process completion just like app.rs does
        let active_state = std::mem::replace(&mut state, CompressionState::Idle);
        match active_state {
            CompressionState::Running { handle, .. } => match handle.join() {
                Ok(Ok(_stats)) => {
                    panic!("Expected failure but compression succeeded!");
                }
                Ok(Err(e)) => {
                    println!("[TEST] Compression failed as expected with error: {:?}", e);
                }
                Err(_) => {
                    panic!("Thread panicked");
                }
            },
            _ => panic!("State was not Running!"),
        }

        // Check if output file exists
        if output_path.exists() {
            let metadata = std::fs::metadata(&output_path).unwrap();
            println!(
                "[TEST] Output file exists after failure! Size: {} bytes",
                metadata.len()
            );
            let _ = std::fs::remove_file(&output_path);
        } else {
            println!("[TEST] Output file does not exist after failure.");
        }
    }
}
