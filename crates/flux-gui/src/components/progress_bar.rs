//! Reusable smooth animated progress bar widget.
//!
//! Visual progress values are interpolated towards the true state on each frame,
//! calling for repaints to animate progress bar advancement.

use egui::Ui;

/// Component rendering an animated progress bar.
pub struct FluxProgressBar {
    /// The currently rendered animated progress value (0.0 to 1.0).
    pub current_value: f32,
}

impl Default for FluxProgressBar {
    fn default() -> Self {
        Self { current_value: 0.0 }
    }
}

impl FluxProgressBar {
    /// Renders the animated progress bar with a label and percentage display.
    ///
    /// The `progress` parameter should be in the range `0.0` to `100.0`.
    pub fn show(&mut self, ui: &mut Ui, progress: f32, label: &str) {
        // Normalize target progress to 0.0 - 1.0 range
        let target = (progress / 100.0).clamp(0.0, 1.0);
        
        // Smoothly interpolate the progress value
        let diff = target - self.current_value;
        if diff.abs() > 0.001 {
            // Move 10% of the remaining distance per frame
            self.current_value += diff * 0.1;
            ui.ctx().request_repaint();
        } else {
            self.current_value = target;
        }

        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.label(label);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(format!("{:.1}%", self.current_value * 100.0));
                });
            });
            ui.add_space(3.0);
            
            // Custom progress bar track
            let height = 14.0;
            let (rect, _response) = ui.allocate_exact_size(
                egui::vec2(ui.available_width(), height),
                egui::Sense::hover(),
            );
            
            // Track background (darker background)
            ui.painter().rect_filled(
                rect,
                7.0,
                ui.style().visuals.extreme_bg_color,
            );
            
            // Filled progress (using a nice cyan gradient/color)
            if self.current_value > 0.0 {
                let mut fill_rect = rect;
                fill_rect.set_width(rect.width() * self.current_value);
                
                ui.painter().rect_filled(
                    fill_rect,
                    7.0,
                    egui::Color32::from_rgb(0, 180, 220), // Cyan FLUX theme color
                );
            }
        });
    }
}
