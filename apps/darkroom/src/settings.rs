//! Host-side settings wiring: the native app owns its preferences, on disk, in the app-data
//! directory — never in browser/WebKit storage.
//!
//! Two entry points share one resolution rule (see [`settings_dir`]):
//! - startup ([`SettingsState::load`], called from `window.rs`): open the store, ingest the
//!   legacy `bridge/web-settings-v1.json` the old Tauri UI left behind exactly once, and turn
//!   anything the user should know about (corrupt file repaired, newer schema, rejected legacy
//!   keys) into status-area notices + stderr lines. Nothing here is fatal: unreadable settings
//!   must never stop the app from opening a catalog.
//! - `--import-settings FILE` (see [`import_settings_file`], called from `main.rs`): the
//!   recovery path for a user who exported settings out of the legacy UI — import that file and
//!   print the report as JSON.

use core_model::settings::parse_web_snapshot;
use core_runtime::settings_store::{LoadOutcome, SettingsPaths, SettingsStore};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

/// Bundle identifier folder the app-data directory is named after; matches Tauri's
/// `app_data_dir` and the `dirs`-based resolution the eval harnesses use.
pub const APP_DATA_DIR_NAME: &str = "com.andrejvysny.darkroom";

/// Wall-clock milliseconds; the single clock of the native host (`core_model::settings` takes
/// `now_ms` from its caller instead of reading a clock itself).
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// Platform application-support directory for [`APP_DATA_DIR_NAME`], or `None` when no home
/// directory is known (`core_runtime` deliberately never derives a home path itself).
pub fn app_data_dir() -> Option<PathBuf> {
    let base = if cfg!(target_os = "macos") {
        home().map(|h| h.join("Library/Application Support"))
    } else if cfg!(windows) {
        std::env::var_os("APPDATA")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| home().map(|h| h.join(".local/share")))
    };
    base.map(|base| base.join(APP_DATA_DIR_NAME))
}

/// Where `settings.json` lives: the directory holding `catalog.db`, i.e. the catalog's parent
/// for `--catalog PATH` (that file *is* the app-data directory), else the platform app-data
/// directory.
pub fn settings_dir(catalog: Option<&Path>) -> Option<PathBuf> {
    match catalog {
        Some(catalog) => Some(
            catalog
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .map_or_else(|| PathBuf::from("."), PathBuf::from),
        ),
        None => app_data_dir(),
    }
}

/// The settings store plus whatever the user should be told about it, kept in the window host's
/// state for the lifetime of the session.
pub struct SettingsState {
    store: Option<SettingsStore>,
    /// Startup diagnostics shown in the status area (see `application::settings_notices`) and
    /// echoed to stderr: cleared by the Dismiss button, never re-added after that.
    pub notices: Vec<String>,
}

impl SettingsState {
    /// Open the store for `catalog`'s directory and import the legacy bridge file once. Every
    /// failure degrades to defaults plus a notice; the app still starts.
    pub fn load(catalog: Option<&Path>) -> Self {
        let now = now_ms();
        let mut state = Self {
            store: None,
            notices: Vec::new(),
        };
        let Some(dir) = settings_dir(catalog) else {
            state.note(
                "Darkroom could not find its application-support folder, so preferences stay at \
                 their defaults for this session.",
            );
            eprintln!("{}", state.describe());
            return state;
        };
        let paths = SettingsPaths::from_app_data_dir(dir);
        let (mut store, outcome) = match SettingsStore::open(paths, now) {
            Ok(opened) => opened,
            Err(error) => {
                state.note(format!(
                    "Darkroom could not open its settings file ({error}); preferences stay at \
                     their defaults for this session."
                ));
                eprintln!("{}", state.describe());
                return state;
            }
        };
        match &outcome {
            LoadOutcome::Loaded | LoadOutcome::CreatedDefaults => {}
            LoadOutcome::RecoveredFromCorrupt { moved_to, message } => state.note(format!(
                "Darkroom repaired its settings: the unreadable file moved to {} and defaults \
                 are in use ({message}).",
                moved_to.display()
            )),
            LoadOutcome::FutureSchema { found } => state.note(format!(
                "Darkroom's settings file was written by a newer version (schema {found}); this \
                 build uses defaults and never overwrites it."
            )),
        }
        // A future-schema store is read-only, so the bridge import would only fail.
        if !matches!(outcome, LoadOutcome::FutureSchema { .. }) {
            match store.import_bridge(now) {
                Ok(None) => {}
                Ok(Some(report)) => {
                    for (key, reason) in &report.rejected {
                        state.note(format!(
                            "Darkroom skipped the legacy setting {key}: {reason}."
                        ));
                    }
                    if !report.already_imported {
                        eprintln!(
                            "settings bridge: {} imported, {} kept, {} rejected",
                            report.applied.len(),
                            report.skipped_newer.len(),
                            report.rejected.len()
                        );
                    }
                }
                Err(error) => state.note(format!(
                    "Darkroom could not import the legacy web settings ({error}); nothing was \
                     changed."
                )),
            }
        }
        state.store = Some(store);
        eprintln!("{}", state.describe());
        state
    }

    /// One-line provenance summary of what is loaded, logged to stderr at startup.
    pub fn describe(&self) -> String {
        match &self.store {
            None => "settings: not loaded, defaults in memory".to_owned(),
            Some(store) => {
                let settings = store.settings();
                format!(
                    "settings: schema {}, bootstrapped {}, auto-update-check {}, suggestions \
                     {}, export dir {:?}, {} export preset(s)",
                    settings.schema,
                    settings.bootstrapped.value,
                    settings.auto_update_check.value,
                    settings.show_suggestions.value,
                    settings.export_dir.value,
                    settings.export_presets.value.len(),
                )
            }
        }
    }

    fn note(&mut self, message: impl Into<String>) {
        let message = message.into();
        eprintln!("{message}");
        self.notices.push(message);
    }
}

/// `--import-settings FILE`: merge a legacy exported settings file into the settings directory
/// resolved from `catalog` and return the report as JSON. The catalog itself is never opened.
///
/// The file is parsed *before* the store is opened, so a malformed file cannot even create
/// `settings.json`.
pub fn import_settings_file(
    catalog: Option<&Path>,
    file: &Path,
) -> Result<serde_json::Value, String> {
    let raw = fs::read_to_string(file).map_err(|error| format!("{}: {error}", file.display()))?;
    parse_web_snapshot(&raw).map_err(|error| format!("{}: {error}", file.display()))?;
    let Some(dir) = settings_dir(catalog) else {
        return Err(
            "no application-support folder could be resolved; pass --catalog PATH to name the \
             settings directory"
                .to_owned(),
        );
    };
    let paths = SettingsPaths::from_app_data_dir(dir);
    let now = now_ms();
    let (mut store, outcome) = SettingsStore::open(paths.clone(), now)
        .map_err(|error| format!("{}: {error}", paths.settings_file().display()))?;
    let recovered = match &outcome {
        LoadOutcome::FutureSchema { found } => {
            return Err(format!(
                "{} was written by a newer version (schema {found}); refusing to import into it",
                paths.settings_file().display()
            ))
        }
        LoadOutcome::RecoveredFromCorrupt { moved_to, .. } => {
            let moved_to = moved_to.display().to_string();
            eprintln!("settings: unreadable file moved to {moved_to}");
            Some(moved_to)
        }
        LoadOutcome::Loaded | LoadOutcome::CreatedDefaults => None,
    };
    let report = store
        .import_file(file, now)
        .map_err(|error| format!("{}: {error}", file.display()))?;
    Ok(serde_json::json!({
        "status": "ok",
        "settingsFile": paths.settings_file(),
        "applied": report.applied,
        "skippedNewer": report.skipped_newer,
        "rejected": report
            .rejected
            .iter()
            .map(|(key, reason)| serde_json::json!({"key": key, "reason": reason}))
            .collect::<Vec<_>>(),
        "alreadyImported": report.already_imported,
        "recoveredFrom": recovered,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_model::settings::{parse_settings, Settings};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Scratch(PathBuf);
    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "dr_settings_{tag}_{}_{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn catalog(&self) -> PathBuf {
            self.0.join("catalog.db")
        }
        fn settings(&self) -> PathBuf {
            self.0.join("settings.json")
        }
        fn bridge(&self) -> PathBuf {
            let file = self.0.join("bridge").join("web-settings-v1.json");
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            file
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn snapshot(captured_at_ms: u64, values: &[(&str, &str)]) -> String {
        let entries: Vec<String> = values
            .iter()
            .map(|(key, value)| format!("{:?}: {:?}", key, value))
            .collect();
        format!(
            r#"{{"format":"darkroom-web-settings","version":1,"capturedAtMs":{captured_at_ms},"values":{{{}}}}}"#,
            entries.join(",")
        )
    }

    fn read_settings(file: &Path) -> Settings {
        parse_settings(&fs::read_to_string(file).unwrap()).unwrap()
    }

    #[test]
    fn settings_dir_is_the_catalog_directory() {
        assert_eq!(
            settings_dir(Some(Path::new("/tmp/one/catalog.db"))),
            Some(PathBuf::from("/tmp/one"))
        );
        // A bare relative filename has no usable parent: the current directory is the answer.
        assert_eq!(
            settings_dir(Some(Path::new("catalog.db"))),
            Some(PathBuf::from("."))
        );
        assert_eq!(settings_dir(None), app_data_dir());
    }

    #[test]
    fn app_data_dir_is_named_after_the_bundle_identifier() {
        if let Some(dir) = app_data_dir() {
            assert!(
                dir.ends_with(APP_DATA_DIR_NAME),
                "unexpected app-data dir {dir:?}"
            );
        }
    }

    #[test]
    fn startup_loads_settings_and_imports_the_bridge_once() {
        let scratch = Scratch::new("bridge");
        fs::write(
            scratch.bridge(),
            snapshot(
                1_700_000_000_000,
                &[
                    ("darkroom.bootstrapped", "1"),
                    ("darkroom.export.presets", "[{\"name\":\"Studio\",\"template\":\"{original}-{seq}\"},{\"name\":\"Web\",\"template\":\"{original}_web\"},{\"name\":\"Broken\"}]"),
                    ("darkroom.export.dir", "exports"),
                ],
            ),
        )
        .unwrap();

        let state = SettingsState::load(Some(&scratch.catalog()));
        assert!(
            state.store.is_some(),
            "store must stay in the application state"
        );
        let settings = state.store.as_ref().expect("store loaded").settings();
        assert!(settings.bootstrapped.value);
        assert_eq!(
            settings
                .export_presets
                .value
                .iter()
                .map(|preset| preset.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Studio", "Web"],
            "the valid presets import, the malformed one is dropped"
        );
        assert_eq!(
            settings.export_dir.value, None,
            "a relative dir is rejected"
        );
        assert!(
            state
                .notices
                .iter()
                .any(|notice| notice.contains("darkroom.export.dir")),
            "rejected legacy keys must be surfaced: {:?}",
            state.notices
        );
        // The startup log line describes what actually loaded, not the pre-load state.
        let described = state.describe();
        assert!(
            described.contains("2 export preset(s)") && described.contains("bootstrapped true"),
            "unexpected startup settings summary: {described}"
        );
        // Persisted, not just in memory.
        assert_eq!(
            read_settings(&scratch.settings())
                .export_presets
                .value
                .len(),
            2
        );
        // The bridge record makes a second startup a no-op: the file is still there, the
        // settings (and their provenance) are unchanged.
        let before = fs::read_to_string(scratch.settings()).unwrap();
        let again = SettingsState::load(Some(&scratch.catalog()));
        assert!(again
            .notices
            .iter()
            .all(|notice| !notice.contains("skipped the legacy")),);
        assert_eq!(fs::read_to_string(scratch.settings()).unwrap(), before);
    }

    #[test]
    fn a_settings_file_from_a_newer_build_is_never_overwritten() {
        let scratch = Scratch::new("future");
        fs::write(
            scratch.bridge(),
            snapshot(1_700_000_000_000, &[("darkroom.bootstrapped", "1")]),
        )
        .unwrap();
        fs::write(scratch.settings(), r#"{"schema":99}"#).unwrap();
        let before = fs::read(scratch.settings()).unwrap();

        let state = SettingsState::load(Some(&scratch.catalog()));
        assert!(
            state
                .notices
                .iter()
                .any(|notice| notice.contains("newer version") && notice.contains("99")),
            "{:?}",
            state.notices
        );
        assert!(
            !state.store.as_ref().unwrap().settings().bootstrapped.value,
            "in-memory defaults are used"
        );
        assert_eq!(
            fs::read(scratch.settings()).unwrap(),
            before,
            "a file written by a newer build is never rewritten, bridge file or not"
        );
    }

    #[test]
    fn corrupt_settings_are_repaired_with_a_notice() {
        let scratch = Scratch::new("corrupt");
        fs::write(scratch.settings(), b"{ not json").unwrap();
        let state = SettingsState::load(Some(&scratch.catalog()));
        assert!(state.store.is_some());
        assert!(
            state
                .notices
                .iter()
                .any(|notice| notice.contains("repaired its settings")),
            "{:?}",
            state.notices
        );
        assert_eq!(read_settings(&scratch.settings()), Settings::default());
        assert!(
            fs::read_dir(&scratch.0)
                .unwrap()
                .flatten()
                .any(|entry| entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("settings.json.corrupt-")),
            "the corrupt file is renamed aside, never deleted"
        );
    }
}
