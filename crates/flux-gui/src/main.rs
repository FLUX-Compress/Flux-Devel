#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! Main entry point for the FLUX GUI application.
//!
//! Configures native window settings (title, size limits), registers
//! sub-modules, and starts the eframe GUI execution loop.

pub mod app;
pub mod components;
pub mod state;
pub mod views;

use app::FluxApp;

fn main() -> Result<(), eframe::Error> {
    // Configure native window settings
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("FLUX Archiver")
            .with_min_inner_size([700.0, 500.0]),
        ..Default::default()
    };

    // Run the native eframe application
    eframe::run_native(
        "FLUX Archiver",
        options,
        Box::new(|cc| Box::new(FluxApp::new(cc)) as Box<dyn eframe::App>),
    )
}
