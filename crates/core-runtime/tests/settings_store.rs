use core_model::settings::{set_native, SETTINGS_SCHEMA};
use core_runtime::settings_store::{LoadOutcome, SettingsPaths, SettingsStore, StoreError};
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

const KEY_BOOTSTRAPPED: &str = "darkroom.bootstrapped";
const KEY_AUTO_UPDATE_CHECK: &str = "darkroom.autoUpdateCheck";
const KEY_SUGGESTIONS_SHOW: &str = "darkroom.suggestions.show";
const KEY_EXPORT_DIR: &str = "darkroom.export.dir";
const KEY_EXPORT_PRESETS: &str = "darkroom.export.presets";
const KEY_EXPORT_LAST: &str = "darkroom.export.last";
const ALL_KEYS: [&str; 6] = [
    KEY_BOOTSTRAPPED,
    KEY_AUTO_UPDATE_CHECK,
    KEY_SUGGESTIONS_SHOW,
    KEY_EXPORT_DIR,
    KEY_EXPORT_PRESETS,
    KEY_EXPORT_LAST,
];

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "darkroom-settings-{tag}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn settings_file(&self) -> PathBuf {
        self.paths().settings_file()
    }
    fn bridge_file(&self) -> PathBuf {
        self.paths().bridge_file()
    }
    fn paths(&self) -> SettingsPaths {
        SettingsPaths::from_app_data_dir(self.0.clone())
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn snapshot_json(captured_at_ms: u64, presets_json: &str) -> String {
    // localStorage stores every value as a string; presets/last are JSON-encoded strings.
    let presets = serde_json::Value::String(presets_json.to_owned());
    let last = serde_json::Value::String(
        r#"{"template":"{original}","format":"png","quality":80}"#.to_owned(),
    );
    serde_json::json!({
        "format": "darkroom-web-settings",
        "version": 1,
        "capturedAtMs": captured_at_ms,
        "values": {
            "darkroom.bootstrapped": "1",
            "darkroom.autoUpdateCheck": "0",
            "darkroom.suggestions.show": "0",
            "darkroom.export.dir": "/tmp/exports",
            "darkroom.export.presets": presets,
            "darkroom.export.last": last
        }
    })
    .to_string()
}

fn bridge_presets(name: &str) -> String {
    format!(r#"[{{"name":"{name}","template":"{{{{seq}}}}-{{{{original}}}}"}}]"#)
}

fn write(path: &PathBuf, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

fn read(path: &PathBuf) -> String {
    fs::read_to_string(path).unwrap()
}

fn mtime(path: &PathBuf) -> std::time::SystemTime {
    fs::metadata(path).unwrap().modified().unwrap()
}

#[test]
fn fresh_dir_creates_defaults_on_disk() {
    let tmp = TempDir::new("fresh");
    let (store, outcome) = SettingsStore::open(tmp.paths(), 1).unwrap();
    assert!(matches!(outcome, LoadOutcome::CreatedDefaults));
    assert_eq!(store.settings(), &core_model::settings::Settings::default());
    let on_disk = read(&tmp.settings_file());
    assert_eq!(
        core_model::settings::parse_settings(&on_disk).unwrap(),
        core_model::settings::Settings::default()
    );
}

#[test]
fn update_persists_with_native_provenance() {
    let tmp = TempDir::new("update");
    let (mut store, _) = SettingsStore::open(tmp.paths(), 1).unwrap();
    store
        .update(|s| set_native(&mut s.export_dir, Some("/Volumes/Out".to_owned()), 123))
        .unwrap();
    drop(store);
    let (reopened, outcome) = SettingsStore::open(tmp.paths(), 2).unwrap();
    assert!(matches!(outcome, LoadOutcome::Loaded));
    let value = &reopened.settings().export_dir;
    assert_eq!(value.value.as_deref(), Some("/Volumes/Out"));
    assert_eq!(
        value.provenance,
        core_model::settings::Provenance::Native { set_at_ms: 123 }
    );
}

#[test]
fn corrupt_file_is_quarantined_and_defaults_take_over() {
    let tmp = TempDir::new("corrupt");
    let garbage = "{ not json at all";
    write(&tmp.settings_file(), garbage);
    let (store, outcome) = SettingsStore::open(tmp.paths(), 555).unwrap();
    let LoadOutcome::RecoveredFromCorrupt { moved_to, message } = outcome else {
        panic!("expected RecoveredFromCorrupt, got {outcome:?}");
    };
    assert_eq!(read(&moved_to), garbage);
    assert!(moved_to.to_string_lossy().contains("corrupt-555"));
    assert!(!message.is_empty());
    assert_eq!(store.settings(), &core_model::settings::Settings::default());
    assert_eq!(
        core_model::settings::parse_settings(&read(&tmp.settings_file())).unwrap(),
        core_model::settings::Settings::default()
    );
}

#[test]
fn future_schema_loads_read_only_without_touching_the_file() {
    let tmp = TempDir::new("future");
    let future = r#"{"schema": 99, "bootstrapped": {"value": true, "provenance": {"kind":"native","setAtMs":7}}}"#;
    write(&tmp.settings_file(), future);
    let (mut store, outcome) = SettingsStore::open(tmp.paths(), 1).unwrap();
    assert!(matches!(outcome, LoadOutcome::FutureSchema { found: 99 }));
    assert_eq!(store.settings(), &core_model::settings::Settings::default());
    let error = store
        .update(|s| set_native(&mut s.bootstrapped, true, 1))
        .unwrap_err();
    assert!(matches!(error, StoreError::ReadOnly));
    assert_eq!(read(&tmp.settings_file()), future);
}

#[test]
fn stale_tmp_file_is_ignored_on_load() {
    let tmp = TempDir::new("partial");
    let (mut store, _) = SettingsStore::open(tmp.paths(), 1).unwrap();
    store
        .update(|s| set_native(&mut s.show_suggestions, false, 9))
        .unwrap();
    drop(store);
    let valid = read(&tmp.settings_file());
    write(&tmp.0.join("settings.json.tmp-999-0"), "garbage leftover");
    let (store, outcome) = SettingsStore::open(tmp.paths(), 2).unwrap();
    assert!(matches!(outcome, LoadOutcome::Loaded));
    assert!(!store.settings().show_suggestions.value);
    assert_eq!(read(&tmp.settings_file()), valid);
}

#[test]
fn bridge_import_applies_once_then_is_a_no_op() {
    let tmp = TempDir::new("bridge");
    let (mut store, _) = SettingsStore::open(tmp.paths(), 1).unwrap();
    write(
        &tmp.bridge_file(),
        &snapshot_json(1000, &bridge_presets("web")),
    );
    let report = store.import_bridge(2000).unwrap().unwrap();
    assert_eq!(report.applied, ALL_KEYS);
    assert!(report.skipped_newer.is_empty() && report.rejected.is_empty());
    assert!(!report.already_imported);
    assert_eq!(
        store.settings().export_dir.value.as_deref(),
        Some("/tmp/exports")
    );
    assert_eq!(store.settings().export_presets.value.len(), 1);

    let before = mtime(&tmp.bridge_file());
    let bytes = read(&tmp.bridge_file());
    let second = store.import_bridge(3000).unwrap().unwrap();
    assert!(second.already_imported);
    assert!(second.applied.is_empty());
    assert_eq!(read(&tmp.bridge_file()), bytes);
    assert_eq!(mtime(&tmp.bridge_file()), before);
}

#[test]
fn native_wins_over_a_newer_bridge_snapshot() {
    let tmp = TempDir::new("native-first");
    let (mut store, _) = SettingsStore::open(tmp.paths(), 1).unwrap();
    write(
        &tmp.bridge_file(),
        &snapshot_json(1000, &bridge_presets("web")),
    );
    store.import_bridge(1500).unwrap().unwrap();

    store
        .update(|s| {
            let presets = &mut s.export_presets;
            presets
                .value
                .iter_mut()
                .for_each(|p| p.template = "{original}-v2".to_owned());
            set_native(presets, presets.value.clone(), 2000);
        })
        .unwrap();

    write(
        &tmp.bridge_file(),
        &snapshot_json(4000, &bridge_presets("web-later")),
    );
    let report = store.import_bridge(4000).unwrap().unwrap();
    assert_eq!(report.skipped_newer, vec![KEY_EXPORT_PRESETS]);
    assert_eq!(store.settings().export_presets.value.len(), 1);
    assert_eq!(store.settings().export_presets.value[0].name, "web");
    assert_eq!(
        store.settings().export_presets.value[0].template,
        "{original}-v2"
    );
}

#[test]
fn import_file_rejects_malformed_snapshot_without_touching_settings() {
    let tmp = TempDir::new("bad-file");
    let (mut store, _) = SettingsStore::open(tmp.paths(), 1).unwrap();
    let before = read(&tmp.settings_file());
    let bad = tmp.0.join("bad-export.json");
    write(&bad, "{ this is not json");
    let error = store.import_file(&bad, 10).unwrap_err();
    match error {
        StoreError::Invalid(message) => assert!(!message.is_empty(), "{message}"),
        other => panic!("expected Invalid, got {other:?}"),
    }
    assert_eq!(read(&tmp.settings_file()), before);

    // Wrong envelope: parseable JSON that is not a v1 web snapshot.
    let wrong = tmp.0.join("wrong-export.json");
    write(
        &wrong,
        r#"{"format":"something-else","version":1,"capturedAtMs":5,"values":{}}"#,
    );
    let error = store.import_file(&wrong, 11).unwrap_err();
    assert!(matches!(error, StoreError::Invalid(_)), "{error:?}");
    assert_eq!(read(&tmp.settings_file()), before);
}

#[test]
fn undecodable_keys_are_rejected_but_still_record_the_bridge() {
    let tmp = TempDir::new("rejected");
    let (mut store, _) = SettingsStore::open(tmp.paths(), 1).unwrap();
    let export = tmp.0.join("export.json");
    write(
        &export,
        r#"{"format":"darkroom-web-settings","version":1,"capturedAtMs":500,
             "values":{"darkroom.export.presets":"not json"}}"#,
    );
    let report = store.import_file(&export, 1000).unwrap();
    assert_eq!(report.rejected.len(), 1);
    assert_eq!(report.rejected[0].0, KEY_EXPORT_PRESETS);
    assert!(report.applied.is_empty());
    // The BridgeRecord persisted, so re-importing the same export is now a no-op.
    let second = store.import_file(&export, 2000).unwrap();
    assert!(second.already_imported);
    assert_eq!(store.settings().export_presets.value, Vec::new());
}

#[test]
fn duplicate_import_file_is_idempotent() {
    let tmp = TempDir::new("dup-file");
    let (mut store, _) = SettingsStore::open(tmp.paths(), 1).unwrap();
    let export = tmp.0.join("export.json");
    write(&export, &snapshot_json(1000, &bridge_presets("web")));
    let first = store.import_file(&export, 2000).unwrap();
    assert_eq!(first.applied, ALL_KEYS);
    let settings_bytes = read(&tmp.settings_file());

    let second = store.import_file(&export, 3000).unwrap();
    assert!(second.already_imported);
    assert!(second.applied.is_empty());
    assert_eq!(read(&tmp.settings_file()), settings_bytes);
    assert_eq!(
        core_model::settings::parse_settings(&settings_bytes)
            .unwrap()
            .schema,
        SETTINGS_SCHEMA
    );
}
