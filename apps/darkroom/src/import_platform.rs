//! Platform folder picker for the import dialog.

use core_runtime::import::ImportPlatform;
use std::path::{Path, PathBuf};

/// Blocking native folder dialog; call it from the main thread only (AppKit requirement).
pub struct NativePlatform;

impl ImportPlatform for NativePlatform {
    fn pick_folder(&self, title: &str, start: Option<&Path>) -> Option<PathBuf> {
        let dialog = rfd::FileDialog::new().set_title(title);
        let dialog = match start.filter(|dir| dir.is_dir()) {
            Some(dir) => dialog.set_directory(dir),
            None => dialog,
        };
        dialog.pick_folder()
    }
}
