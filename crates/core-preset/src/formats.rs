//! Format-specific preset importers. Each implements [`crate::registry::PresetImporter`] and parses
//! one external format into the unit-neutral [`crate::ir::PresetIr`]. Add a new format = add a module
//! here + register it in [`crate::registry::Registry::with_defaults`].

pub mod lr_template;
pub mod lr_xmp;
