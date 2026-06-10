//! Home landing view for the FLUX GUI application.
//!
//! Renders branding, main buttons, drag-and-drop file inputs,
//! and the list of recently created/extracted archives.

use crate::app::AppView;
use crate::app::FluxApp;
use egui::{Align, Button, Layout, RichText, Ui};

/// Renders the Home page view of the application.
pub fn show(app: &mut FluxApp, ui: &mut Ui) {
    ui.vertical_centered(|ui| {
        ui.add_space(20.0);

        // Logo/Title area (vibrant cyan typography)
        ui.heading(
            RichText::new("⚡ FLUX Archiver")
                .size(36.0)
                .strong()
                .color(egui::Color32::from_rgb(0, 180, 220)),
        );
        ui.label(
            RichText::new("Adaptive Content-Aware Solid Compression")
                .size(14.0)
                .italics(),
        );

        ui.add_space(30.0);

        // Main operations: Compress & Extract
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 20.0;

            // Align buttons nicely in the center
            let total_width = 380.0; // two 180px buttons + 20px spacing
            let offset = (ui.available_width() - total_width) / 2.0;
            ui.add_space(offset.max(0.0));

            let compress_btn = Button::new(RichText::new("📦 Compress").size(18.0));
            if ui.add_sized([180.0, 50.0], compress_btn).clicked() {
                app.current_view = AppView::Compress;
            }

            let extract_btn = Button::new(RichText::new("🔓 Extract").size(18.0));
            if ui.add_sized([180.0, 50.0], extract_btn).clicked() {
                app.current_view = AppView::Extract;
            }
        });

        ui.add_space(30.0);

        // Drag and drop target area
        let drop_rect_size = egui::vec2(ui.available_width().min(500.0), 120.0);
        let (rect, response) = ui.allocate_exact_size(drop_rect_size, egui::Sense::hover());

        let visual = ui.style().interact(&response);
        let border_color = if ui.ctx().input(|i| !i.raw.hovered_files.is_empty()) {
            egui::Color32::from_rgb(0, 180, 220) // Highlight border when dragging over
        } else {
            visual.bg_stroke.color
        };

        ui.painter()
            .rect_stroke(rect, 8.0, egui::Stroke::new(2.0, border_color));

        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "📥 Drop files or folders here to compress",
            egui::FontId::proportional(15.0),
            ui.visuals().text_color(),
        );

        // Check for dropped files
        if !ui.ctx().input(|i| i.raw.dropped_files.is_empty()) {
            let dropped = ui.ctx().input(|i| i.raw.dropped_files.clone());
            if let Some(file) = dropped.first() {
                if let Some(ref path) = file.path {
                    app.compress_view.picker.path = Some(path.clone());
                    app.compress_view.auto_suggest_output();
                    app.current_view = AppView::Compress;
                }
            }
        }

        ui.add_space(30.0);

        // Recent archives list
        ui.vertical(|ui| {
            ui.label(RichText::new("Recent Archives").strong().size(14.0));
            ui.add_space(5.0);

            if app.settings.recent_archives.is_empty() {
                ui.label("No recent archives. Select Compress or Extract above to begin.");
            } else {
                for recent in app.settings.recent_archives.clone() {
                    let filename = recent.file_name().unwrap_or_default().to_string_lossy();
                    ui.horizontal(|ui| {
                        if ui.button(format!("Open: {}", filename)).clicked() {
                            app.extract_view.picker.path = Some(recent.clone());
                            app.extract_view.read_archive_metadata();
                            app.current_view = AppView::Extract;
                        }
                        ui.label(RichText::new(recent.to_string_lossy()).size(10.0).weak());
                    });
                }
            }
        });

        // Version in corner (lower right/left layout)
        ui.add_space(20.0);
        ui.with_layout(Layout::bottom_up(Align::Max), |ui| {
            ui.label(RichText::new("FLUX Version 1.0.0").size(10.0).weak());
        });
    });
}
