//! UI-independent application state and typed workflow boundaries.
pub mod autosave;
pub mod editor;
pub mod error;
pub mod import;
pub mod jobs;
pub mod ports;
pub mod registry;
pub mod session;
pub mod state;
pub mod subscription;
pub mod workflow;
pub use error::AppError;
pub use session::ApplicationSession;
