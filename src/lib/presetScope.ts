// Mirror of `core-preset::scope::GROUPS` (the Rust source of truth). Keep in sync — a drift test in
// `src-tauri` asserts the flattened field list equals the serialized DevelopParams keys.
//
// Used by: the Create-Preset dialog (per-module checkboxes), copy/paste-settings scope selection,
// and grouping the import report. Field keys are camelCase DevelopParams top-level fields.

export type ScopeGroup = { key: string; label: string; fields: string[] };

export const SCOPE_GROUPS: ScopeGroup[] = [
  { key: "whiteBalance", label: "White Balance", fields: ["temp", "tint"] },
  {
    key: "light",
    label: "Light",
    fields: [
      "exposure",
      "contrast",
      "highlights",
      "shadows",
      "whites",
      "blacks",
    ],
  },
  { key: "baseTone", label: "Base Tone", fields: ["toneAmount"] },
  { key: "toneCurve", label: "Tone Curve", fields: ["toneCurve"] },
  { key: "colorMixer", label: "Color Mixer", fields: ["saturation", "hsl"] },
  { key: "colorBalance", label: "Color Balance", fields: ["cbRgb"] },
  { key: "detail", label: "Detail", fields: ["sharpen", "nrLuma", "nrColor"] },
  { key: "lens", label: "Lens", fields: ["vignette"] },
  { key: "crop", label: "Crop", fields: ["crop"] },
  { key: "masks", label: "Masks", fields: ["masks"] },
];

export const ALL_FIELD_KEYS: string[] = SCOPE_GROUPS.flatMap((g) => g.fields);

/** Map selected group keys → the flat list of DevelopParams field keys they cover. */
export function fieldsForGroups(groupKeys: string[]): string[] {
  return SCOPE_GROUPS.filter((g) => groupKeys.includes(g.key)).flatMap(
    (g) => g.fields,
  );
}

/** The default module-scope selection for a new user preset: the global look, no geometry/masks. */
export const DEFAULT_PRESET_GROUPS: string[] = [
  "whiteBalance",
  "light",
  "baseTone",
  "toneCurve",
  "colorMixer",
  "colorBalance",
  "detail",
  "lens",
];
