//! Build script for the FLUX GUI application.
//!
//! On Windows platforms, this script embeds high-DPI awareness manifests
//! and application icons into the compiled executable target.
//!
//! ## Windows Installer (.msi) Packaging
//! To package the FLUX GUI as a Windows Installer (.msi), use `cargo-wix`:
//! 1. Install `cargo-wix`: `cargo install cargo-wix`
//! 2. Initialize configuration: `cargo wix init`
//! 3. Build package: `cargo wix`

fn main() {
    // Only compile resource files when targeting Windows systems.
    #[cfg(target_os = "windows")]
    {
        if std::path::Path::new("resources.rc").exists() {
            embed_resource::compile("resources.rc", embed_resource::NONE);
        }
    }
}
