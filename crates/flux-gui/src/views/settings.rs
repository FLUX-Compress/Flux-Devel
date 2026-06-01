//! Settings configuration and About section view.
//!
//! Provides controls for adjusting default settings (threads, buffer size,
//! compression level, output folders) and configuring color themes.

use egui::{RichText, Ui};
use sysinfo::System;

use flux::Compression;
use crate::app::FluxApp;
use crate::state::Theme;

/// Renders the Settings page and saves modifications.
pub fn show(app: &mut FluxApp, ui: &mut Ui) {
    let settings = &mut app.settings;
    let mut changed = false;

    ui.vertical(|ui| {
        ui.heading(RichText::new("⚙️ Settings").strong());
        ui.add_space(10.0);

        // --- HARDWARE & THREADS ---
        ui.group(|ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new("Hardware & Threading").strong());
                ui.add_space(5.0);

                let mut auto_threads = settings.thread_count_override.is_none();
                if ui.checkbox(&mut auto_threads, "Auto-detect threads (recommended)").changed() {
                    if auto_threads {
                        settings.thread_count_override = None;
                    } else {
                        settings.thread_count_override = Some(4);
                    }
                    changed = true;
                }

                if let Some(ref mut threads) = settings.thread_count_override {
                    ui.horizontal(|ui| {
                        ui.label("Thread Count:");
                        let slider = egui::Slider::new(threads, 1..=32).text("threads");
                        if ui.add(slider).changed() {
                            changed = true;
                        }
                    });
                }
            });
        });

        ui.add_space(10.0);

        // --- LOOKAHEAD BUFFER ---
        ui.group(|ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new("Lookahead Buffer Size").strong());
                ui.add_space(5.0);

                // Fetch total RAM to display 10% value
                let mut sys = System::new();
                sys.refresh_memory();
                let total_ram_mb = sys.total_memory() / (1024 * 1024);
                let default_buffer_mb = total_ram_mb / 10; // 10% of RAM

                let mut default_buffer = settings.buffer_size_override.is_none();
                if ui.checkbox(&mut default_buffer, format!("Default (10% of RAM: ~{} MB)", default_buffer_mb)).changed() {
                    if default_buffer {
                        settings.buffer_size_override = None;
                    } else {
                        settings.buffer_size_override = Some((default_buffer_mb * 1024 * 1024) as usize);
                    }
                    changed = true;
                }

                if let Some(ref mut size_bytes) = settings.buffer_size_override {
                    ui.horizontal(|ui| {
                        ui.label("Buffer Size (MB):");
                        let mut size_mb = *size_bytes / (1024 * 1024);
                        let slider = egui::Slider::new(&mut size_mb, 16..=4096).text("MB");
                        if ui.add(slider).changed() {
                            *size_bytes = size_mb * 1024 * 1024;
                            changed = true;
                        }
                    });
                }
            });
        });

        ui.add_space(10.0);

        // --- COMPRESSION DEFAULTS ---
        ui.group(|ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new("Compression Defaults").strong());
                ui.add_space(5.0);

                ui.horizontal(|ui| {
                    ui.label("Default Level:");
                    if ui.radio_value(&mut settings.default_compression_level, Compression::Tiny, "Tiny").changed() {
                        changed = true;
                    }
                    if ui.radio_value(&mut settings.default_compression_level, Compression::Fast, "Fast").changed() {
                        changed = true;
                    }
                    if ui.radio_value(&mut settings.default_compression_level, Compression::Balanced, "Balanced").changed() {
                        changed = true;
                    }
                    if ui.radio_value(&mut settings.default_compression_level, Compression::Maximum, "Maximum").changed() {
                        changed = true;
                    }
                    if ui.radio_value(&mut settings.default_compression_level, Compression::Extreme, "Extreme").changed() {
                        changed = true;
                    }
                });

                ui.add_space(5.0);

                // Default Output Directory
                ui.horizontal(|ui| {
                    ui.label("Default Output Folder:");
                    let mut path_str = match &settings.default_output_dir {
                        Some(p) => p.to_string_lossy().to_string(),
                        None => "Same as source...".to_string(),
                    };
                    ui.add(
                        egui::TextEdit::singleline(&mut path_str)
                            .interactive(false)
                            .desired_width(240.0),
                    );
                    if ui.button("Browse...").clicked() {
                        if let Some(p) = rfd::FileDialog::new().pick_folder() {
                            settings.default_output_dir = Some(p);
                            changed = true;
                        }
                    }
                    if settings.default_output_dir.is_some() && ui.button("Clear").clicked() {
                        settings.default_output_dir = None;
                        changed = true;
                    }
                });
            });
        });

        ui.add_space(10.0);

        // --- APPEARANCE & THEME ---
        ui.group(|ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new("Appearance").strong());
                ui.add_space(5.0);

                ui.horizontal(|ui| {
                    ui.label("Color Theme:");
                    let mut theme = settings.theme;
                    if ui.radio_value(&mut theme, Theme::System, "System").changed() {
                        settings.theme = theme;
                        apply_theme(ui.ctx(), theme);
                        changed = true;
                    }
                    if ui.radio_value(&mut theme, Theme::Light, "Light").changed() {
                        settings.theme = theme;
                        apply_theme(ui.ctx(), theme);
                        changed = true;
                    }
                    if ui.radio_value(&mut theme, Theme::Dark, "Dark").changed() {
                        settings.theme = theme;
                        apply_theme(ui.ctx(), theme);
                        changed = true;
                    }
                });
            });
        });

        ui.add_space(10.0);

        // --- ABOUT SECTION ---
        ui.group(|ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new("About FLUX").strong());
                ui.add_space(5.0);
                ui.label("FLUX Archiver — Version 1.0.0");
                ui.label(
                    "FLUX is an advanced solid archive utility that combines content-aware \
                     routing, adaptive plane transposition, context mixed PPM-I modeling, \
                     and secure AES-256-GCM encryption.",
                );
                
                ui.add_space(5.0);
                ui.hyperlink_to("GitHub Project Repository", "https://github.com/acydd/flux");
                ui.add_space(5.0);
                ui.label(
                    RichText::new(
                        "FLUX is open source software. Distributed under the GPL v3 license \
                         for open source usage. Commercial licenses are available for proprietary \
                         embedding.",
                    )
                    .weak()
                    .size(10.0),
                );
            });
        });

        // Trigger persistence if a field changed
        if changed {
            settings.save();
        }
    });
}

/// Applies theme changes to the current egui context.
pub fn apply_theme(ctx: &egui::Context, theme: Theme) {
    match theme {
        Theme::System => {
            // Egui detects system theme on launch, clear any visual overrides
            ctx.set_visuals(egui::Visuals::default());
        }
        Theme::Light => {
            ctx.set_visuals(egui::Visuals::light());
        }
        Theme::Dark => {
            ctx.set_visuals(egui::Visuals::dark());
        }
    }
}
