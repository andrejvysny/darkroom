//! `--import-settings FILE`: the legacy-export recovery path, driven through the real binary
//! against temp directories only — never the user's `~/Library/Application Support` (the
//! settings directory is pinned with `--catalog PATH`, whose file is never opened).

use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

struct Scratch(PathBuf);
impl Scratch {
    fn new(tag: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "dr_settings_cli_{tag}_{}_{}",
            std::process::id(),
            n
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    /// Only its *parent* matters: `--import-settings` never opens the catalog.
    fn catalog(&self) -> PathBuf {
        self.0.join("catalog.db")
    }
    fn settings(&self) -> PathBuf {
        self.0.join("settings.json")
    }
    fn write_export(&self, tag: &str, contents: &str) -> PathBuf {
        let file = self.0.join(format!("{tag}.json"));
        fs::write(&file, contents).unwrap();
        file
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A legacy `darkroom-web-settings` v1 export: two valid presets plus one malformed entry and an
/// out-of-range export quality, so the report has both applied and rejected keys.
fn web_export(captured_at_ms: u64) -> String {
    let presets = r#"[{"name":"Studio","template":"{original}-{seq}"},
                      {"name":"Web","template":"{original}_web"},
                      {"name":"Broken"}]"#;
    json!({
        "format": "darkroom-web-settings",
        "version": 1,
        "capturedAtMs": captured_at_ms,
        "values": {
            "darkroom.bootstrapped": "1",
            "darkroom.autoUpdateCheck": "0",
            "darkroom.export.dir": "/Users/tester/Exports",
            "darkroom.export.presets": presets,
            "darkroom.export.last": r#"{"template":"{original}","format":"jpeg","quality":300}"#,
        }
    })
    .to_string()
}

fn import_settings(export: &Path, catalog: &Path) -> (i32, Value) {
    let output = Command::new(env!("CARGO_BIN_EXE_darkroom-native"))
        .args([
            "--import-settings",
            export.to_str().unwrap(),
            "--catalog",
            catalog.to_str().unwrap(),
        ])
        .output()
        .expect("run darkroom-native --import-settings");
    let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
    let line = stdout
        .lines()
        .find(|line| line.starts_with('{'))
        .unwrap_or_else(|| panic!("no JSON on stdout: {stdout:?}"));
    let code = output.status.code().expect("exit code");
    (
        code,
        serde_json::from_str(line).expect("report is valid JSON"),
    )
}

fn read_settings(file: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(file).unwrap()).expect("settings.json is JSON")
}

#[test]
fn valid_export_creates_the_native_settings_file() {
    let scratch = Scratch::new("import");
    // Deliberately garbage: the settings import must not open (or care about) the catalog.
    fs::write(scratch.catalog(), b"definitely not sqlite").unwrap();
    let export = scratch.write_export("web", &web_export(1_700_000_000_000));

    let (code, report) = import_settings(&export, &scratch.catalog());
    assert_eq!(code, 0, "a valid export must import cleanly: {report}");
    assert_eq!(report["status"], "ok");
    assert_eq!(report["alreadyImported"], false);
    assert_eq!(report["settingsFile"], json!(scratch.settings()));
    for key in [
        "darkroom.bootstrapped",
        "darkroom.autoUpdateCheck",
        "darkroom.export.dir",
        "darkroom.export.presets",
    ] {
        assert!(
            report["applied"]
                .as_array()
                .unwrap()
                .iter()
                .any(|applied| applied == key),
            "{key} should have been applied: {report}"
        );
    }
    let rejected: Vec<&str> = report["rejected"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["key"].as_str().unwrap())
        .collect();
    assert!(
        rejected.contains(&"darkroom.export.presets[2]"),
        "the preset without a template is rejected, not dropped silently: {report}"
    );
    assert!(
        rejected.contains(&"darkroom.export.last"),
        "quality 300 is out of range and must be reported: {report}"
    );

    let settings = read_settings(&scratch.settings());
    assert_eq!(settings["schema"], json!(1));
    assert_eq!(settings["bootstrapped"]["value"], json!(true));
    assert_eq!(settings["autoUpdateCheck"]["value"], json!(false));
    assert_eq!(
        settings["exportDir"]["value"],
        json!("/Users/tester/Exports")
    );
    assert_eq!(
        settings["exportPresets"]["value"]
            .as_array()
            .unwrap()
            .iter()
            .map(|preset| preset["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["Studio", "Web"],
        "the valid presets import and the malformed one is refused"
    );
    assert_eq!(
        settings["exportPresets"]["value"][0]["template"],
        json!("{original}-{seq}")
    );
    assert_eq!(
        settings["exportPresets"]["provenance"]["kind"],
        json!("webBridge")
    );
    // Importing is recorded, which is what makes the bridge idempotent.
    let bridge = &settings["migrations"]["web_bridge"];
    assert_eq!(bridge["captured_at_ms"], json!(1_700_000_000_000_u64));
    assert_eq!(bridge["source"], json!(export));
    assert!(bridge["keys_rejected"]
        .as_array()
        .unwrap()
        .iter()
        .any(|key| key == "darkroom.export.last"));
}

#[test]
fn importing_twice_is_a_no_op() {
    let scratch = Scratch::new("twice");
    let export = scratch.write_export("web", &web_export(1_700_000_000_000));
    let (code, _) = import_settings(&export, &scratch.catalog());
    assert_eq!(code, 0);
    let before = fs::read(scratch.settings()).unwrap();

    let (code, report) = import_settings(&export, &scratch.catalog());
    assert_eq!(code, 0, "a repeat import is not a failure: {report}");
    assert_eq!(report["status"], "ok");
    assert_eq!(report["alreadyImported"], true);
    assert_eq!(report["applied"].as_array().unwrap().len(), 0);
    assert_eq!(
        fs::read(scratch.settings()).unwrap(),
        before,
        "an already-imported snapshot must not rewrite the file"
    );
}

#[test]
fn a_newer_snapshot_still_imports() {
    let scratch = Scratch::new("newer");
    let older = scratch.write_export("older", &web_export(1_600_000_000_000));
    assert_eq!(import_settings(&older, &scratch.catalog()).0, 0);
    let export = scratch.write_export("newer", &web_export(1_700_000_000_000));

    let (code, report) = import_settings(&export, &scratch.catalog());
    assert_eq!(code, 0, "{report}");
    assert_eq!(report["alreadyImported"], false, "a newer snapshot applies");
    let settings = read_settings(&scratch.settings());
    assert_eq!(
        settings["migrations"]["web_bridge"]["captured_at_ms"],
        json!(1_700_000_000_000_u64)
    );
    assert_eq!(
        settings["exportPresets"]["value"].as_array().unwrap().len(),
        2
    );
}

#[test]
fn malformed_export_exits_1_and_leaves_settings_untouched() {
    let scratch = Scratch::new("malformed");
    let good = scratch.write_export("web", &web_export(1_700_000_000_000));
    assert_eq!(import_settings(&good, &scratch.catalog()).0, 0);
    let before = fs::read(scratch.settings()).unwrap();

    for (tag, contents) in [
        ("truncated", r#"{"format": "#),
        (
            "wrong-format",
            r#"{"format":"nope","version":1,"values":{}}"#,
        ),
        (
            "wrong-version",
            r#"{"format":"darkroom-web-settings","version":2,"values":{}}"#,
        ),
        ("not-an-object", "42"),
    ] {
        let export = scratch.write_export(tag, contents);
        let (code, report) = import_settings(&export, &scratch.catalog());
        assert_eq!(code, 1, "{tag} must fail: {report}");
        assert_eq!(report["status"], "error");
        assert_eq!(report["kind"], "settings");
        assert_eq!(
            fs::read(scratch.settings()).unwrap(),
            before,
            "{tag} must not change settings.json"
        );
    }

    // A missing file is the same shape of failure.
    let missing = scratch.0.join("nope.json");
    let (code, report) = import_settings(&missing, &scratch.catalog());
    assert_eq!(code, 1, "a missing export must fail: {report}");
    assert_eq!(report["status"], "error");
    assert_eq!(
        fs::read(scratch.settings()).unwrap(),
        before,
        "a missing export must not change settings.json"
    );
}

#[test]
fn malformed_export_never_creates_a_settings_file() {
    let scratch = Scratch::new("no-file");
    let export = scratch.write_export("bad", "not json at all");
    let (code, report) = import_settings(&export, &scratch.catalog());
    assert_eq!(code, 1, "{report}");
    assert_eq!(report["status"], "error");
    assert!(
        !scratch.settings().exists(),
        "a file that is not a settings export must not initialize the store"
    );
}
