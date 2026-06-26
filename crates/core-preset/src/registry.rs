//! The importer registry: a list of format plug-ins, each implementing [`PresetImporter`]. `import`
//! sniffs the bytes/path, parses with the first matching importer into a [`PresetIr`], then maps to
//! the sparse params + [`ImportReport`]. New formats (Lightroom `.xmp`/`.lrtemplate`, RawTherapee
//! `.pp3`, `.cube`, …) are added by registering another importer — nothing else changes.

use std::path::Path;

use serde_json::{Map, Value};

use crate::error::PresetError;
use crate::ir::PresetIr;
use crate::map::ir_to_sparse;
use crate::report::ImportReport;

/// Hard cap on a preset import file's size (4 MiB). Real `.xmp`/`.lrtemplate` presets are a few KB;
/// this bounds the DOM/parse cost of a hostile or accidentally-huge input. (roxmltree already ignores
/// DTDs/external entities, so this is the remaining size lever.)
pub const MAX_IMPORT_BYTES: usize = 4 * 1024 * 1024;

/// A pluggable preset-format importer. `detect` must be cheap and must not panic on arbitrary bytes.
pub trait PresetImporter: Send + Sync {
    fn format_name(&self) -> &'static str;
    fn detect(&self, bytes: &[u8], path: Option<&Path>) -> bool;
    fn parse(&self, bytes: &[u8], path: Option<&Path>) -> Result<PresetIr, PresetError>;
}

/// The result of importing one preset file: the sparse params + which top-level fields it sets + the
/// honest conversion report.
pub struct ParsedPreset {
    pub params: Map<String, Value>,
    pub field_keys: Vec<String>,
    pub report: ImportReport,
}

/// A set of importers tried in order.
#[derive(Default)]
pub struct Registry {
    importers: Vec<Box<dyn PresetImporter>>,
}

impl Registry {
    /// Registry with the built-in importers (tried in registration order).
    pub fn with_defaults() -> Self {
        Self {
            importers: vec![
                Box::new(crate::formats::lr_xmp::LightroomXmp),
                Box::new(crate::formats::lr_template::LightroomTemplate),
            ],
        }
    }

    /// Register an additional importer (highest-priority last-registered wins ties is *not* implied;
    /// first match in registration order wins).
    pub fn register(&mut self, importer: Box<dyn PresetImporter>) {
        self.importers.push(importer);
    }

    /// Detect → parse → map. Returns the sparse preset + report, or an error if no importer matches.
    pub fn import(&self, bytes: &[u8], path: Option<&Path>) -> Result<ParsedPreset, PresetError> {
        if bytes.len() > MAX_IMPORT_BYTES {
            return Err(PresetError::Malformed(format!(
                "preset file too large ({} bytes; max {})",
                bytes.len(),
                MAX_IMPORT_BYTES
            )));
        }
        let importer = self
            .importers
            .iter()
            .find(|i| i.detect(bytes, path))
            .ok_or(PresetError::UnknownFormat)?;
        let ir = importer.parse(bytes, path)?;
        let (params, field_keys, report) = ir_to_sparse(&ir, importer.format_name());
        Ok(ParsedPreset {
            params,
            field_keys,
            report,
        })
    }
}
