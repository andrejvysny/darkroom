//! Module scope: the grouping of `DevelopParams` top-level fields into the modules a user sees in the
//! Develop panel. This is the **Rust source of truth**; `src/lib/presetScope.ts` mirrors it. Used by
//! the create-preset dialog (per-module checkboxes), copy/paste-settings scope, and import-report
//! grouping. The flattened field list also defines the full set of preset-able top-level keys.

/// One selectable module group → the `DevelopParams` (camelCase) top-level fields it contains.
pub struct ScopeGroup {
    pub key: &'static str,
    pub label: &'static str,
    pub fields: &'static [&'static str],
}

/// All module groups, in Develop-panel order. The union of `fields` is every preset-able top-level
/// field of `DevelopParams` (kept in sync via a drift test in `src-tauri`).
pub const GROUPS: &[ScopeGroup] = &[
    ScopeGroup {
        key: "whiteBalance",
        label: "White Balance",
        fields: &["temp", "tint"],
    },
    ScopeGroup {
        key: "light",
        label: "Light",
        fields: &[
            "exposure",
            "contrast",
            "highlights",
            "shadows",
            "whites",
            "blacks",
        ],
    },
    ScopeGroup {
        key: "baseTone",
        label: "Base Tone",
        fields: &["toneAmount"],
    },
    ScopeGroup {
        key: "toneCurve",
        label: "Tone Curve",
        fields: &["toneCurve"],
    },
    ScopeGroup {
        key: "colorMixer",
        label: "Color Mixer",
        fields: &["saturation", "hsl"],
    },
    ScopeGroup {
        key: "colorBalance",
        label: "Color Balance",
        fields: &["cbRgb"],
    },
    ScopeGroup {
        key: "detail",
        label: "Detail",
        fields: &["sharpen", "nrLuma", "nrColor"],
    },
    ScopeGroup {
        key: "lens",
        label: "Lens",
        fields: &["vignette"],
    },
    ScopeGroup {
        key: "crop",
        label: "Crop",
        fields: &["crop"],
    },
    ScopeGroup {
        key: "masks",
        label: "Masks",
        fields: &["masks"],
    },
];

/// Every preset-able top-level field key (camelCase), in panel order.
pub fn all_field_keys() -> Vec<&'static str> {
    GROUPS
        .iter()
        .flat_map(|g| g.fields.iter().copied())
        .collect()
}

/// Resolve a set of selected group keys to the flat list of field keys they cover (dedup-free; groups
/// are disjoint).
pub fn fields_for_groups(group_keys: &[String]) -> Vec<String> {
    GROUPS
        .iter()
        .filter(|g| group_keys.iter().any(|k| k == g.key))
        .flat_map(|g| g.fields.iter().map(|f| f.to_string()))
        .collect()
}
