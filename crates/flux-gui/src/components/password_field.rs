//! Reusable password input field widget.
//!
//! Provides text masking, a show/hide visibility toggle, and a dynamic
//! colored strength indicator bar using the core Argon2id key verification rules.

use egui::Ui;
use flux_core_v1::crypto::keys::{KeyDerivation, PasswordStrength};

/// Component managing password text entry state and rendering.
#[derive(Default)]
pub struct PasswordField {
    /// The current password text value.
    pub password: String,
    /// Whether the password characters are currently unmasked.
    pub visible: bool,
}

impl PasswordField {
    /// Renders the password field widget with its label, show/hide toggle, and strength bar.
    ///
    /// Returns `Some(password)` if the password field is not empty, otherwise `None`.
    pub fn show(&mut self, ui: &mut Ui, label: &str) -> Option<String> {
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.label(label);
                
                // Text input widget with dynamic masking
                let text_edit = egui::TextEdit::singleline(&mut self.password)
                    .password(!self.visible)
                    .desired_width(200.0);
                
                ui.add(text_edit);

                // Show/hide toggle button
                let toggle_label = if self.visible { "Hide 👁" } else { "Show 👁" };
                if ui.button(toggle_label).clicked() {
                    self.visible = !self.visible;
                }
            });

            // Password strength indicator bar
            if !self.password.is_empty() {
                let strength = KeyDerivation::verify_key_strength(&self.password);
                
                let (color, strength_label, fill_percent) = match strength {
                    PasswordStrength::Weak => (egui::Color32::from_rgb(220, 50, 50), "Weak 🔴", 0.25),
                    PasswordStrength::Moderate => (egui::Color32::from_rgb(220, 180, 50), "Moderate 🟡", 0.5),
                    PasswordStrength::Strong => (egui::Color32::from_rgb(50, 180, 50), "Strong 🟢", 0.75),
                    PasswordStrength::VeryStrong => (egui::Color32::from_rgb(0, 120, 0), "Very Strong 🔥", 1.0),
                };

                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.label("Strength:");
                    ui.colored_label(color, strength_label);
                });

                // Custom bar rendering
                let height = 4.0;
                let width = 250.0;
                let (rect, _response) = ui.allocate_exact_size(
                    egui::vec2(width, height),
                    egui::Sense::hover(),
                );
                
                // Draw background bar
                ui.painter().rect_filled(
                    rect,
                    2.0,
                    ui.style().visuals.extreme_bg_color,
                );

                // Draw filled strength portion
                let mut progress_rect = rect;
                progress_rect.set_width(width * fill_percent);
                ui.painter().rect_filled(progress_rect, 2.0, color);
            }
        });

        if self.password.is_empty() {
            None
        } else {
            Some(self.password.clone())
        }
    }
}
