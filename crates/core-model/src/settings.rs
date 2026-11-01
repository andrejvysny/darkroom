//! Versioned native settings with per-value provenance, plus an idempotent bridge from the
//! legacy browser UI's `localStorage` snapshot. Pure: no file IO, no clocks — callers pass
//! `now_ms` and own persistence.
//!
//! Provenance is what keeps a stale web snapshot from clobbering native choices: a value last
//! set natively is never overwritten by an import, and re-importing an already-seen (or older)
//! snapshot is a no-op.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

/// Current on-disk schema version. Bump only with a migration; reads of higher versions fail
/// with [`SettingsError::FutureSchema`] so the caller never overwrites a newer file.
pub const SETTINGS_SCHEMA: u32 = 1;

/// `format` marker the legacy Tauri UI writes into its settings export.
pub const WEB_SNAPSHOT_FORMAT: &str = "darkroom-web-settings";
/// Snapshot `version` the bridge understands.
pub const WEB_SNAPSHOT_VERSION: u32 = 1;

// Legacy `localStorage` keys, exactly as the browser UI spells them (useLibrary.ts,
// useUpdater.ts, store/app.ts, ExportModal.tsx).
const KEY_BOOTSTRAPPED: &str = "darkroom.bootstrapped";
const KEY_AUTO_UPDATE_CHECK: &str = "darkroom.autoUpdateCheck";
const KEY_SUGGESTIONS_SHOW: &str = "darkroom.suggestions.show";
const KEY_EXPORT_DIR: &str = "darkroom.export.dir";
const KEY_EXPORT_PRESETS: &str = "darkroom.export.presets";
const KEY_EXPORT_LAST: &str = "darkroom.export.last";

/// Why a value holds what it holds.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Provenance {
    /// Never explicitly set; still the shipped default.
    #[default]
    Default,
    /// Written by the web-snapshot bridge; `captured_at_ms` is the snapshot's capture time.
    #[serde(rename_all = "camelCase")]
    WebBridge {
        /// Snapshot `capturedAtMs` this value came from.
        captured_at_ms: u64,
    },
    /// Set through the native UI; import must never overwrite it.
    #[serde(rename_all = "camelCase")]
    Native {
        /// Wall-clock ms when the user set it.
        set_at_ms: u64,
    },
}

/// A setting value paired with where it came from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Setting<T> {
    /// The value itself.
    pub value: T,
    /// Origin of `value`; gates snapshot imports.
    pub provenance: Provenance,
}

impl<T: Default> Default for Setting<T> {
    fn default() -> Self {
        Self {
            value: T::default(),
            provenance: Provenance::Default,
        }
    }
}

/// A user-defined export filename preset. Deliberately distinct from Develop presets,
/// which are catalog-backed and never live in settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportPreset {
    /// Display name; unique within the list.
    pub name: String,
    /// Filename template with `{original}` `{date}` `{seq}` tokens.
    pub template: String,
}

/// Export image format as spelled by the legacy UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExportFormat {
    /// JPEG.
    Jpeg,
    /// PNG.
    Png,
}

/// The last-used export dialog configuration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportLast {
    /// Filename template of the last export.
    pub template: String,
    /// Image format of the last export.
    pub format: ExportFormat,
    /// JPEG quality 1..=100 (stored for PNG too, ignored there).
    pub quality: u8,
}

/// Persisted preferences. Window/layout lives in the separate [`LayoutPrefs`] section;
/// documents (edits) and transient interaction state never live here.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// Schema version of this document; see [`SETTINGS_SCHEMA`].
    pub schema: u32,
    /// Whether first-launch bootstrap has ever run (`darkroom.bootstrapped == "1"`).
    pub bootstrapped: Setting<bool>,
    /// Background update checks (`darkroom.autoUpdateCheck != "0"`).
    pub auto_update_check: Setting<bool>,
    /// Inline suggestion banners (`darkroom.suggestions.show != "0"`).
    pub show_suggestions: Setting<bool>,
    /// Last export destination folder, absolute.
    pub export_dir: Setting<Option<String>>,
    /// User export filename presets (builtins are code, not settings).
    pub export_presets: Setting<Vec<ExportPreset>>,
    /// Last export configuration.
    pub export_last: Setting<Option<ExportLast>>,
    /// Window and panel geometry.
    pub layout: LayoutPrefs,
    /// One-shot migration bookkeeping.
    pub migrations: MigrationLog,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema: SETTINGS_SCHEMA,
            bootstrapped: Setting::default(),
            auto_update_check: Setting {
                value: true,
                provenance: Provenance::Default,
            },
            show_suggestions: Setting {
                value: true,
                provenance: Provenance::Default,
            },
            export_dir: Setting::default(),
            export_presets: Setting::default(),
            export_last: Setting::default(),
            layout: LayoutPrefs::default(),
            migrations: MigrationLog::default(),
        }
    }
}

/// Window/panel geometry; its own section so it can evolve (or be reset) independently.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LayoutPrefs {
    /// Collapsed/expanded width of the left panel.
    pub left_panel_width: Option<f32>,
    /// Width of the right panel.
    pub right_panel_width: Option<f32>,
    /// Last window position and size.
    pub window: Option<WindowRect>,
}

/// Window rectangle in screen coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowRect {
    /// Left edge.
    pub x: i32,
    /// Top edge.
    pub y: i32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

/// Bookkeeping for one-shot migrations run at load time.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationLog {
    /// Set once a web snapshot has been imported; makes the bridge idempotent.
    pub web_bridge: Option<BridgeRecord>,
}

/// Evidence that a web snapshot was imported, keyed by its capture time.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BridgeRecord {
    /// `capturedAtMs` of the imported snapshot.
    pub captured_at_ms: u64,
    /// Wall-clock ms of the import.
    pub imported_at_ms: u64,
    /// Where the snapshot came from (file path or `"export-file"`).
    pub source: String,
    /// Legacy keys whose values were written.
    pub keys_imported: Vec<String>,
    /// Legacy keys whose values failed to decode and kept their current value.
    pub keys_rejected: Vec<String>,
}

/// Failure loading a settings document or web snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SettingsError {
    /// Unparseable JSON or an unsupported snapshot envelope. Caller may reset to defaults.
    Corrupt(String),
    /// Document written by a newer app version; the caller MUST NOT overwrite the file.
    FutureSchema(u32),
}

impl fmt::Display for SettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Corrupt(reason) => write!(f, "settings are corrupt: {reason}"),
            Self::FutureSchema(s) => write!(f, "settings schema {s} newer than {SETTINGS_SCHEMA}"),
        }
    }
}

impl std::error::Error for SettingsError {}

/// Raw `localStorage` dump the legacy Tauri UI writes (and the "Export settings…" file):
/// every value is the stored string, or `None` for keys that were never written.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebSnapshot {
    /// Must equal [`WEB_SNAPSHOT_FORMAT`].
    pub format: String,
    /// Must equal [`WEB_SNAPSHOT_VERSION`].
    pub version: u32,
    /// Wall-clock ms when the browser captured the values.
    pub captured_at_ms: u64,
    /// Legacy key → stored string.
    pub values: BTreeMap<String, Option<String>>,
}

/// Load a settings document. Unknown extra fields are ignored on read, so forward-compatible
/// minor additions written by a newer app still load. Missing fields take [`Settings::default`].
pub fn parse_settings(json: &str) -> Result<Settings, SettingsError> {
    let settings: Settings =
        serde_json::from_str(json).map_err(|e| SettingsError::Corrupt(e.to_string()))?;
    if settings.schema > SETTINGS_SCHEMA {
        return Err(SettingsError::FutureSchema(settings.schema));
    }
    Ok(settings)
}

/// Serialize with pretty printing and stable (declaration-order) keys.
pub fn to_json(settings: &Settings) -> String {
    // Infallible for this shape: no non-string map keys; non-finite f32 serializes as null.
    serde_json::to_string_pretty(settings)
        .expect("Settings is a plain data shape; serialization cannot fail")
}

/// Envelope probe for [`parse_web_snapshot`]; fields optional so a missing `format`/`version`
/// reports the same "unsupported web snapshot" as a wrong one.
#[derive(Deserialize)]
struct SnapshotEnvelope {
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    version: Option<u32>,
}

/// Load a web snapshot, rejecting anything that is not a v1 `darkroom-web-settings` envelope.
pub fn parse_web_snapshot(json: &str) -> Result<WebSnapshot, SettingsError> {
    // Only unparseable JSON surfaces the serde message.
    let envelope: SnapshotEnvelope =
        serde_json::from_str(json).map_err(|e| SettingsError::Corrupt(e.to_string()))?;
    if envelope.format.as_deref() != Some(WEB_SNAPSHOT_FORMAT)
        || envelope.version != Some(WEB_SNAPSHOT_VERSION)
    {
        return Err(SettingsError::Corrupt(
            "unsupported web snapshot".to_owned(),
        ));
    }
    serde_json::from_str(json).map_err(|e| SettingsError::Corrupt(e.to_string()))
}

/// Record that the user changed a setting natively; shields it from future snapshot imports.
pub fn set_native<T>(setting: &mut Setting<T>, value: T, now_ms: u64) {
    setting.value = value;
    setting.provenance = Provenance::Native { set_at_ms: now_ms };
}

/// Outcome of [`import_web`]; legacy key names throughout.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportReport {
    /// Keys whose values were written.
    pub applied: Vec<String>,
    /// Keys whose values were valid but the target is Native (or newer WebBridge).
    pub skipped_newer: Vec<String>,
    /// Keys whose value failed to decode, with a human-readable reason.
    pub rejected: Vec<(String, String)>,
    /// The snapshot was at or before the last imported one; nothing changed.
    pub already_imported: bool,
}

/// Merge a web snapshot into `settings` (returns the new state; input is untouched).
///
/// Idempotent: a snapshot no newer than the recorded [`BridgeRecord`] never applies. Native
/// values are never overwritten; unknown snapshot keys are ignored.
pub fn import_web(
    settings: &Settings,
    snap: &WebSnapshot,
    source: &str,
    now_ms: u64,
) -> (Settings, ImportReport) {
    if let Some(prev) = &settings.migrations.web_bridge {
        if prev.captured_at_ms >= snap.captured_at_ms {
            return (
                settings.clone(),
                ImportReport {
                    already_imported: true,
                    ..Default::default()
                },
            );
        }
    }
    let mut out = settings.clone();
    let mut imp = Importer {
        snap,
        applied: Vec::new(),
        skipped_newer: Vec::new(),
        rejected: Vec::new(),
    };
    // Legacy encodings: bootstrapped is true only when exactly "1"; the two switches are
    // default-on and off only at "0".
    imp.import_bool(KEY_BOOTSTRAPPED, &mut out.bootstrapped, |v| v == "1");
    imp.import_bool(KEY_AUTO_UPDATE_CHECK, &mut out.auto_update_check, |v| {
        v != "0"
    });
    imp.import_bool(KEY_SUGGESTIONS_SHOW, &mut out.show_suggestions, |v| {
        v != "0"
    });
    imp.import_export_dir(KEY_EXPORT_DIR, &mut out.export_dir);
    imp.import_presets(KEY_EXPORT_PRESETS, &mut out.export_presets);
    imp.import_last(KEY_EXPORT_LAST, &mut out.export_last);
    out.migrations.web_bridge = Some(BridgeRecord {
        captured_at_ms: snap.captured_at_ms,
        imported_at_ms: now_ms,
        source: source.to_owned(),
        keys_imported: imp.applied.clone(),
        keys_rejected: imp.rejected.iter().map(|(k, _)| k.clone()).collect(),
    });
    (
        out,
        ImportReport {
            applied: imp.applied,
            skipped_newer: imp.skipped_newer,
            rejected: imp.rejected,
            already_imported: false,
        },
    )
}

struct Importer<'a> {
    snap: &'a WebSnapshot,
    applied: Vec<String>,
    skipped_newer: Vec<String>,
    rejected: Vec<(String, String)>,
}

impl<'a> Importer<'a> {
    fn raw(&self, key: &str) -> Option<&'a str> {
        self.snap.values.get(key).and_then(|v| v.as_deref())
    }

    /// True unless the target is Native or a WebBridge value from a newer snapshot.
    fn overwritable(&self, prov: &Provenance) -> bool {
        match prov {
            Provenance::Default => true,
            Provenance::WebBridge { captured_at_ms } => *captured_at_ms < self.snap.captured_at_ms,
            Provenance::Native { .. } => false,
        }
    }

    /// Gate one decoded candidate value: reject on decode failure, skip Native targets,
    /// otherwise stamp the value with this snapshot's provenance.
    fn set<T>(&mut self, key: &str, slot: &mut Setting<T>, decoded: Result<T, String>) {
        let value = match decoded {
            Ok(v) => v,
            Err(reason) => {
                self.rejected.push((key.to_owned(), reason));
                return;
            }
        };
        if !self.overwritable(&slot.provenance) {
            self.skipped_newer.push(key.to_owned());
            return;
        }
        slot.value = value;
        slot.provenance = Provenance::WebBridge {
            captured_at_ms: self.snap.captured_at_ms,
        };
        self.applied.push(key.to_owned());
    }

    /// An absent key means "legacy default", which the target already holds; never reset from
    /// absence, so missing values are a no-op.
    fn import_bool(&mut self, key: &str, slot: &mut Setting<bool>, decode: impl Fn(&str) -> bool) {
        if let Some(raw) = self.raw(key) {
            self.set(key, slot, Ok(decode(raw)));
        }
    }

    fn import_export_dir(&mut self, key: &str, slot: &mut Setting<Option<String>>) {
        let Some(raw) = self.raw(key) else { return };
        let decoded = if is_absolute_path(raw) {
            Ok(Some(raw.to_owned()))
        } else {
            Err(format!("export dir is not an absolute path: {raw:?}"))
        };
        self.set(key, slot, decoded);
    }

    fn import_presets(&mut self, key: &str, slot: &mut Setting<Vec<ExportPreset>>) {
        let Some(raw) = self.raw(key) else { return };
        // Deserializing to Vec fails for non-arrays (including `null`), which is a whole-key reject.
        let items: Vec<serde_json::Value> = match serde_json::from_str(raw) {
            Ok(items) => items,
            Err(e) => {
                self.set(key, slot, Err(format!("invalid JSON: {e}")));
                return;
            }
        };
        let mut kept: Vec<ExportPreset> = Vec::new();
        let was_empty = items.is_empty();
        for (i, item) in items.into_iter().enumerate() {
            match decode_preset(&item) {
                // Dedupe by name keeping the first; duplicates are dropped, not rejected.
                Ok(preset) if !kept.iter().any(|k| k.name == preset.name) => kept.push(preset),
                Ok(_) => {}
                Err(reason) => self.rejected.push((format!("{key}[{i}]"), reason)),
            }
        }
        // Only wipe existing presets when the snapshot legitimately held an empty array.
        if !kept.is_empty() || was_empty {
            self.set(key, slot, Ok(kept));
        }
    }

    fn import_last(&mut self, key: &str, slot: &mut Setting<Option<ExportLast>>) {
        let Some(raw) = self.raw(key) else { return };
        self.set(key, slot, decode_last(raw).map(Some));
    }
}

fn is_absolute_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    // Leading '/' (POSIX) or `X:\` / `X:/` (the app also ships a Windows installer).
    path.starts_with('/')
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && (bytes[2] == b'\\' || bytes[2] == b'/'))
}

fn decode_preset(item: &serde_json::Value) -> Result<ExportPreset, String> {
    let name = item
        .get("name")
        .and_then(serde_json::Value::as_str)
        .ok_or("preset name is missing or not a string")?;
    let template = item
        .get("template")
        .and_then(serde_json::Value::as_str)
        .ok_or("preset template is missing or not a string")?;
    if name.is_empty() {
        return Err("preset name is empty".to_owned());
    }
    Ok(ExportPreset {
        name: name.to_owned(),
        template: template.to_owned(),
    })
}

/// `darkroom.export.last` wire shape; quality as `i64` so out-of-range values (e.g. `-5`,
/// `300`) produce our own message instead of a serde type error.
#[derive(Deserialize)]
struct ExportLastRaw {
    template: String,
    format: String,
    quality: i64,
}

fn decode_last(raw: &str) -> Result<ExportLast, String> {
    let last: ExportLastRaw =
        serde_json::from_str(raw).map_err(|e| format!("invalid JSON: {e}"))?;
    let format = match last.format.as_str() {
        "jpeg" => ExportFormat::Jpeg,
        "png" => ExportFormat::Png,
        other => return Err(format!("unknown export format {other:?}")),
    };
    if !(1..=100).contains(&last.quality) {
        return Err(format!(
            "export quality {} out of range 1..=100",
            last.quality
        ));
    }
    Ok(ExportLast {
        template: last.template,
        format,
        quality: last.quality as u8,
    })
}
