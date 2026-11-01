//! Platform chrome of the native window: Lightroom-style macOS title bar and system fonts.

use std::sync::Arc;
use winit::dpi::LogicalSize;
use winit::window::{Window, WindowAttributes};

/// Space the traffic-light buttons occupy at the window's left edge (logical px) once the title
/// bar is transparent and the content extends under it; 0 where the OS draws a normal title bar.
pub const fn chrome_inset_left() -> f32 {
    if cfg!(target_os = "macos") {
        78.0
    } else {
        0.0
    }
}

pub fn window_attributes() -> WindowAttributes {
    let attributes = Window::default_attributes()
        .with_title("Darkroom")
        .with_inner_size(LogicalSize::new(1440.0, 900.0))
        .with_min_inner_size(LogicalSize::new(1000.0, 640.0));
    #[cfg(target_os = "macos")]
    let attributes = {
        use winit::platform::macos::WindowAttributesExtMacOS;
        attributes
            .with_titlebar_transparent(true)
            .with_fullsize_content_view(true)
            .with_title_hidden(true)
    };
    attributes
}

/// Try the macOS system UI fonts first, keeping egui's bundled fonts as fallbacks. Any missing
/// or unreadable file leaves the defaults in place (logged once).
pub fn install_fonts(ctx: &egui::Context) {
    if !cfg!(target_os = "macos") {
        return;
    }
    let mut fonts = egui::FontDefinitions::default();
    let faces = [
        (
            "system-ui",
            "/System/Library/Fonts/SFNS.ttf",
            egui::FontFamily::Proportional,
        ),
        (
            "system-mono",
            "/System/Library/Fonts/SFNSMono.ttf",
            egui::FontFamily::Monospace,
        ),
    ];
    for (name, path, family) in faces {
        match std::fs::read(path) {
            Ok(bytes) => {
                fonts
                    .font_data
                    .insert(name.to_owned(), Arc::new(egui::FontData::from_owned(bytes)));
                fonts
                    .families
                    .entry(family)
                    .or_default()
                    .insert(0, name.to_owned());
            }
            Err(error) => eprintln!("system font {path} unavailable, using defaults: {error}"),
        }
    }
    ctx.set_fonts(fonts);
}
