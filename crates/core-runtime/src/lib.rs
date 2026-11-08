//! Concrete adapters over existing engines; no GUI or Tauri dependency.
pub mod async_workflow;
pub mod catalog;
pub mod export_io;
pub mod import;
pub mod jobs;
pub mod presets;
pub mod preview;
pub mod publication;
pub mod save_barrier;
pub mod settings_store;
pub mod sidecar;
pub mod workers;
pub mod workflow;
