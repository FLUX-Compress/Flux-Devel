//! Main eframe application coordinator for the FLUX GUI.
//!
//! Provides the root layout, coordinates top navigation menus,
//! applies themes, and polls background thread progress.

use egui::RichText;

use crate::state::{AppSettings, CompressionState};
use crate::views::compress::CompressView;
use crate::views::extract::ExtractView;
use crate::views::progress::ProgressView;

/// Available pages/views in the application.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppView {
    Home,
    Compress,
    Extract,
    Progress,
    Settings,
    About,
}

/// The root application struct for FLUX GUI.
pub struct FluxApp {
    /// Currently active view.
    pub current_view: AppView,
    /// Thread monitoring execution state.
    pub compression_state: CompressionState,
    /// User settings.
    pub settings: AppSettings,
    /// Child view state for Compression.
    pub compress_view: CompressView,
    /// Child view state for Extraction.
    pub extract_view: ExtractView,
    /// Child view state for Progress.
    pub progress_view: ProgressView,
}

impl FluxApp {
    /// Initializes a new `FluxApp`, loading user settings and applying color themes.
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let settings = AppSettings::load();

        crate::views::settings::apply_theme(&cc.egui_ctx, settings.theme);

        Self {
            current_view: AppView::Home,
            compression_state: CompressionState::Idle,
            settings,
            compress_view: CompressView::default(),
            extract_view: ExtractView::default(),
            progress_view: ProgressView::default(),
        }
    }
}

impl eframe::App for FluxApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // --- 1. POLL BACKGROUND THREADS ---
        let mut thread_finished = None;

        match &self.compression_state {
            CompressionState::Running { handle, .. } if handle.is_finished() => {
                thread_finished = Some(true);
            }
            CompressionState::RunningExtract { handle, .. } if handle.is_finished() => {
                thread_finished = Some(false);
            }
            _ => {}
        }

        if let Some(is_compress) = thread_finished {
            let active_state =
                std::mem::replace(&mut self.compression_state, CompressionState::Idle);
            match active_state {
                CompressionState::Running { handle, .. } if is_compress => match handle.join() {
                    Ok(Ok(stats)) => {
                        if let Some(ref path) = self.compress_view.output_picker.path {
                            self.settings.add_recent(path.clone());
                        }
                        self.compression_state = CompressionState::Complete(stats);
                    }
                    Ok(Err(e)) => {
                        self.compression_state = CompressionState::Failed(e.to_string());
                    }
                    Err(_) => {
                        self.compression_state =
                            CompressionState::Failed("Compression thread panicked".to_string());
                    }
                },
                CompressionState::RunningExtract { handle, .. } if !is_compress => {
                    match handle.join() {
                        Ok(Ok(stats)) => {
                            if let Some(ref path) = self.extract_view.picker.path {
                                self.settings.add_recent(path.clone());
                            }
                            self.compression_state = CompressionState::ExtractComplete(stats);
                        }
                        Ok(Err(e)) => {
                            self.compression_state = CompressionState::Failed(e.to_string());
                        }
                        Err(_) => {
                            self.compression_state =
                                CompressionState::Failed("Extraction thread panicked".to_string());
                        }
                    }
                }
                _ => {}
            }
        }

        // --- 2. RENDER LAYOUT ---
        egui::TopBottomPanel::top("top_navbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 12.0;

                // Navigation Items
                if ui
                    .selectable_label(self.current_view == AppView::Home, "🏠 Home")
                    .clicked()
                {
                    self.current_view = AppView::Home;
                }
                if ui
                    .selectable_label(self.current_view == AppView::Compress, "📦 Compress")
                    .clicked()
                {
                    self.current_view = AppView::Compress;
                }
                if ui
                    .selectable_label(self.current_view == AppView::Extract, "🔓 Extract")
                    .clicked()
                {
                    self.current_view = AppView::Extract;
                }
                if ui
                    .selectable_label(self.current_view == AppView::Settings, "⚙ Settings")
                    .clicked()
                {
                    self.current_view = AppView::Settings;
                }

                // Dynamic Task Running Banner
                let is_active = matches!(
                    self.compression_state,
                    CompressionState::Running { .. } | CompressionState::RunningExtract { .. }
                );
                if is_active {
                    ui.separator();
                    let banner = ui.selectable_label(
                        self.current_view == AppView::Progress,
                        RichText::new("⚡ Active Task Running...")
                            .color(egui::Color32::from_rgb(0, 180, 220))
                            .strong(),
                    );
                    if banner.clicked() {
                        self.current_view = AppView::Progress;
                    }
                }
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            // Render specific page
            egui::ScrollArea::vertical().show(ui, |ui| match self.current_view {
                AppView::Home => crate::views::home::show(self, ui),
                AppView::Compress => crate::views::compress::show(self, ui),
                AppView::Extract => crate::views::extract::show(self, ui),
                AppView::Progress => crate::views::progress::show(self, ui),
                AppView::Settings => crate::views::settings::show(self, ui),
                _ => {}
            });
        });
    }
}
