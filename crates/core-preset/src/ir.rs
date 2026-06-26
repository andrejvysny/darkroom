//! The format-neutral intermediate representation. Each importer (`formats::*`) parses its file into
//! a `PresetIr` whose fields are already expressed in **Darkroom's** units/scales (e.g. `exposure` in
//! EV, the −100..100 sliders, tone-curve points in [0,1]). `None` means "the source did not set this
//! module" → it stays absent from the produced sparse preset. Conversion to the camelCase sparse JSON
//! + the `ImportReport` is done once in [`crate::map::ir_to_sparse`].
//!
//! Dropped/approximated keys the parser encountered are recorded in `seen_dropped`/`seen_approx` so
//! the report can explain them (the mapping tier of the *emitted* fields is decided in `map.rs`).

/// A unit-neutral, sparse parsed preset.
#[derive(Debug, Clone, Default)]
pub struct PresetIr {
    /// Lightroom `crs:ProcessVersion` (or equivalent), for the report. Not our `PROCESS_VERSION`.
    pub lr_process_version: Option<String>,

    // Basic tone — already in Darkroom slider units.
    pub exposure: Option<f32>, // EV
    pub contrast: Option<f32>, // -100..100
    pub highlights: Option<f32>,
    pub shadows: Option<f32>,
    pub whites: Option<f32>,
    pub blacks: Option<f32>,
    pub saturation: Option<f32>,

    // Detail / lens.
    pub sharpen: Option<f32>, // 0..150
    pub nr_luma: Option<f32>, // 0..100
    pub nr_color: Option<f32>,
    pub vignette: Option<f32>, // -100..100

    /// Relative tint nudge only — absolute WB temperature has no anchor in our as-shot-relative model
    /// and is dropped by the importer (recorded in `seen_dropped`).
    pub tint: Option<f32>,

    pub tone_curve: Option<ToneCurveIr>,
    pub hsl: Option<[HslIr; 8]>,
    pub crop: Option<CropIr>,

    /// Keys seen in the source with no faithful target (`(key, reason)`), for the report.
    pub seen_dropped: Vec<(String, String)>,
    /// Extra parser-level caveats beyond the inherent per-field tiering (`(key, reason)`).
    pub seen_approx: Vec<(String, String)>,
}

/// Tone curve control points in [0,1] (x = input, y = output), per channel; empty = identity.
#[derive(Debug, Clone, Default)]
pub struct ToneCurveIr {
    pub rgb: Vec<(f32, f32)>,
    pub r: Vec<(f32, f32)>,
    pub g: Vec<(f32, f32)>,
    pub b: Vec<(f32, f32)>,
}

/// One HSL band (hue/sat/lum, −100..100).
#[derive(Debug, Clone, Copy, Default)]
pub struct HslIr {
    pub h: f32,
    pub s: f32,
    pub l: f32,
}

/// Crop center + half-extents (normalized) + straighten angle (degrees, CCW+), matching `Crop`.
#[derive(Debug, Clone, Copy, Default)]
pub struct CropIr {
    pub cx: f32,
    pub cy: f32,
    pub hw: f32,
    pub hh: f32,
    pub angle: f32,
}
