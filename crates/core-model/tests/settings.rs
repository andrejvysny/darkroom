//! Web-snapshot bridge and settings-document behavior for `core_model::settings`.

use core_model::settings::{
    import_web, parse_settings, parse_web_snapshot, set_native, to_json, BridgeRecord,
    ExportFormat, ExportLast, ExportPreset, ImportReport, Provenance, Settings, SettingsError,
    WebSnapshot, SETTINGS_SCHEMA,
};
use core_model::DevelopParams;
use serde_json::json;
use std::collections::BTreeMap;

fn snapshot(captured_at_ms: u64, values: &[(&str, Option<&str>)]) -> WebSnapshot {
    WebSnapshot {
        format: "darkroom-web-settings".to_owned(),
        version: 1,
        captured_at_ms,
        values: values
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.map(str::to_owned)))
            .collect::<BTreeMap<_, _>>(),
    }
}

fn web_bridge(at: u64) -> Provenance {
    Provenance::WebBridge { captured_at_ms: at }
}

#[test]
fn full_snapshot_imports_all_six_legacy_keys() {
    let snap = snapshot(
        1000,
        &[
            ("darkroom.bootstrapped", Some("1")),
            ("darkroom.autoUpdateCheck", Some("0")),
            ("darkroom.suggestions.show", Some("0")),
            ("darkroom.export.dir", Some("/Users/me/Exports")),
            (
                "darkroom.export.presets",
                Some(r#"[{"name":"Date","template":"{date}_{original}"}]"#),
            ),
            (
                "darkroom.export.last",
                Some(r#"{"template":"{original}","format":"jpeg","quality":90}"#),
            ),
        ],
    );
    let (settings, report) = import_web(&Settings::default(), &snap, "export-file", 2000);

    assert!(!report.already_imported);
    assert_eq!(report.applied.len(), 6);
    assert!(report.rejected.is_empty() && report.skipped_newer.is_empty());
    assert!(settings.bootstrapped.value);
    assert!(!settings.auto_update_check.value);
    assert!(!settings.show_suggestions.value);
    assert_eq!(
        settings.export_dir.value.as_deref(),
        Some("/Users/me/Exports")
    );
    assert_eq!(
        settings.export_presets.value,
        vec![ExportPreset {
            name: "Date".to_owned(),
            template: "{date}_{original}".to_owned()
        }]
    );
    assert_eq!(
        settings.export_last.value,
        Some(ExportLast {
            template: "{original}".to_owned(),
            format: ExportFormat::Jpeg,
            quality: 90
        })
    );
    assert_eq!(settings.bootstrapped.provenance, web_bridge(1000));
    assert_eq!(
        settings.migrations.web_bridge,
        Some(BridgeRecord {
            captured_at_ms: 1000,
            imported_at_ms: 2000,
            source: "export-file".to_owned(),
            keys_imported: report.applied.clone(),
            keys_rejected: vec![],
        })
    );
}

#[test]
fn boolean_encodings_zero_one_and_absent() {
    // "1" boots; "0" turns the default-on switches off.
    let (s, _) = import_web(
        &Settings::default(),
        &snapshot(
            1,
            &[
                ("darkroom.bootstrapped", Some("1")),
                ("darkroom.autoUpdateCheck", Some("0")),
                ("darkroom.suggestions.show", Some("0")),
            ],
        ),
        "t",
        1,
    );
    assert_eq!(
        (
            s.bootstrapped.value,
            s.auto_update_check.value,
            s.show_suggestions.value
        ),
        (true, false, false)
    );

    // Any other string keeps legacy semantics: bootstrapped only at exactly "1".
    let (s, _) = import_web(
        &Settings::default(),
        &snapshot(
            1,
            &[
                ("darkroom.bootstrapped", Some("yes")),
                ("darkroom.autoUpdateCheck", Some("1")),
                ("darkroom.suggestions.show", Some("")),
            ],
        ),
        "t",
        1,
    );
    assert_eq!(
        (
            s.bootstrapped.value,
            s.auto_update_check.value,
            s.show_suggestions.value
        ),
        (false, true, true)
    );

    // Absent (missing or null) = legacy default: no-op, provenance stays Default, value intact.
    let (s, report) = import_web(
        &Settings::default(),
        &snapshot(
            1,
            &[
                ("darkroom.bootstrapped", None),
                ("darkroom.autoUpdateCheck", None),
            ],
        ),
        "t",
        1,
    );
    let mut expected = Settings::default();
    // Rule 5: the bridge record is written even when nothing was applied.
    expected.migrations.web_bridge = Some(BridgeRecord {
        captured_at_ms: 1,
        imported_at_ms: 1,
        source: "t".to_owned(),
        keys_imported: vec![],
        keys_rejected: vec![],
    });
    assert_eq!(s, expected);
    assert!(report.applied.is_empty());
    assert_eq!(s.bootstrapped.provenance, Provenance::Default);
}

#[test]
fn same_snapshot_twice_is_already_imported_and_identical() {
    let snap = snapshot(
        500,
        &[
            ("darkroom.bootstrapped", Some("1")),
            ("darkroom.export.dir", Some("/tmp/x")),
        ],
    );
    let (once, first) = import_web(&Settings::default(), &snap, "f", 600);
    let (twice, second) = import_web(&once, &snap, "f", 700);
    assert!(!first.already_imported);
    assert!(second.already_imported);
    assert_eq!(once, twice);
    assert_eq!(second.applied, Vec::<String>::new());
}

#[test]
fn older_snapshot_after_newer_never_applies() {
    let newer = snapshot(900, &[("darkroom.autoUpdateCheck", Some("0"))]);
    let older = snapshot(
        100,
        &[
            ("darkroom.autoUpdateCheck", Some("1")),
            ("darkroom.export.dir", Some("/stale")),
        ],
    );
    let (after_new, _) = import_web(&Settings::default(), &newer, "f", 1000);
    let (after_old, report) = import_web(&after_new, &older, "f", 1100);
    assert!(report.already_imported);
    assert_eq!(after_new, after_old);
    assert!(!after_old.auto_update_check.value);
    assert_eq!(after_old.export_dir.value, None);
}

#[test]
fn native_values_survive_later_snapshots_as_skipped_newer() {
    let first = snapshot(100, &[("darkroom.autoUpdateCheck", Some("1"))]);
    let (mut s, _) = import_web(&Settings::default(), &first, "f", 150);
    set_native(&mut s.auto_update_check, false, 200);

    let later = snapshot(
        300,
        &[
            ("darkroom.autoUpdateCheck", Some("1")),
            ("darkroom.export.dir", Some("/ok")),
        ],
    );
    let (s, report) = import_web(&s, &later, "f", 400);
    assert_eq!(
        report.skipped_newer,
        vec!["darkroom.autoUpdateCheck".to_owned()]
    );
    assert_eq!(report.applied, vec!["darkroom.export.dir".to_owned()]);
    assert!(!s.auto_update_check.value);
    assert_eq!(
        s.auto_update_check.provenance,
        Provenance::Native { set_at_ms: 200 }
    );
}

#[test]
fn older_web_bridge_value_is_updated_by_newer_snapshot() {
    let s1 = snapshot(100, &[("darkroom.export.dir", Some("/a"))]);
    let (s, _) = import_web(&Settings::default(), &s1, "f", 100);
    let s2 = snapshot(200, &[("darkroom.export.dir", Some("/b"))]);
    let (s, report) = import_web(&s, &s2, "f", 200);
    assert_eq!(report.applied, vec!["darkroom.export.dir".to_owned()]);
    assert_eq!(s.export_dir.value.as_deref(), Some("/b"));
    assert_eq!(s.export_dir.provenance, web_bridge(200));
}

#[test]
fn corrupt_presets_json_rejects_key_but_applies_the_rest() {
    let snap = snapshot(
        10,
        &[
            ("darkroom.bootstrapped", Some("1")),
            ("darkroom.export.dir", Some("/ok")),
            ("darkroom.export.presets", Some("{not json")),
        ],
    );
    let (s, report) = import_web(&Settings::default(), &snap, "f", 10);
    assert_eq!(
        report
            .rejected
            .iter()
            .map(|(k, _)| k.as_str())
            .collect::<Vec<_>>(),
        vec!["darkroom.export.presets"]
    );
    assert!(!report.rejected[0].1.is_empty(), "human-readable reason");
    assert_eq!(
        report.applied,
        vec!["darkroom.bootstrapped", "darkroom.export.dir"]
    );
    assert_eq!(s.export_presets.value, Vec::<ExportPreset>::new());
    assert_eq!(s.export_presets.provenance, Provenance::Default);
    assert!(s.bootstrapped.value);
    assert_eq!(s.export_dir.value.as_deref(), Some("/ok"));
    // The rejected key is recorded in the bridge log.
    assert_eq!(
        s.migrations.web_bridge.as_ref().unwrap().keys_rejected,
        vec!["darkroom.export.presets"]
    );
}

#[test]
fn mixed_presets_keep_valid_reject_invalid_per_item_and_dedupe() {
    let raw = r#"[
        {"name":"A","template":"{original}"},
        "garbage",
        {"name":"","template":"x"},
        {"name":"B","template":"{date}"},
        {"name":"A","template":"dup"},
        {"name":"C"}
    ]"#;
    let snap = snapshot(10, &[("darkroom.export.presets", Some(raw))]);
    let (s, report) = import_web(&Settings::default(), &snap, "f", 10);
    assert_eq!(
        s.export_presets.value,
        vec![
            ExportPreset {
                name: "A".to_owned(),
                template: "{original}".to_owned()
            },
            ExportPreset {
                name: "B".to_owned(),
                template: "{date}".to_owned()
            }
        ]
    );
    assert_eq!(
        report
            .rejected
            .iter()
            .map(|(k, _)| k.as_str())
            .collect::<Vec<_>>(),
        vec![
            "darkroom.export.presets[1]",
            "darkroom.export.presets[2]",
            "darkroom.export.presets[5]"
        ]
    );
    assert_eq!(report.applied, vec!["darkroom.export.presets".to_owned()]);
    assert_eq!(s.export_presets.provenance, web_bridge(10));
}

#[test]
fn all_invalid_presets_never_wipe_existing_ones() {
    let good = snapshot(
        10,
        &[(
            "darkroom.export.presets",
            Some(r#"[{"name":"A","template":"t"}]"#),
        )],
    );
    let (s, _) = import_web(&Settings::default(), &good, "f", 10);
    let bad = snapshot(
        20,
        &[(
            "darkroom.export.presets",
            Some(r#"[{"nope":1},{"name":"","template":"t"}]"#),
        )],
    );
    let (s, report) = import_web(&s, &bad, "f", 20);
    assert_eq!(s.export_presets.value.len(), 1);
    assert!(report.applied.is_empty());
    assert_eq!(report.rejected.len(), 2);
}

#[test]
fn export_last_validation_rejects_bad_quality_and_format() {
    for bad in [
        r#"{"template":"t","format":"jpeg","quality":0}"#,
        r#"{"template":"t","format":"jpeg","quality":101}"#,
        r#"{"template":"t","format":"jpeg","quality":-5}"#,
        r#"{"template":"t","format":"gif","quality":90}"#,
        r#"{"template":"t","format":"jpeg"}"#,
        r#"{"template":7,"format":"jpeg","quality":90}"#,
        "nonsense",
    ] {
        let snap = snapshot(10, &[("darkroom.export.last", Some(bad))]);
        let (s, report) = import_web(&Settings::default(), &snap, "f", 10);
        assert_eq!(
            report
                .rejected
                .iter()
                .map(|(k, _)| k.as_str())
                .collect::<Vec<_>>(),
            vec!["darkroom.export.last"],
            "must reject: {bad}"
        );
        assert_eq!(s.export_last.value, None);
    }
    // PNG with quality in range is fine (quality is stored, ignored for PNG downstream).
    let snap = snapshot(
        10,
        &[(
            "darkroom.export.last",
            Some(r#"{"template":"{seq}","format":"png","quality":50}"#),
        )],
    );
    let (s, report) = import_web(&Settings::default(), &snap, "f", 10);
    assert!(report.rejected.is_empty());
    assert_eq!(s.export_last.value.unwrap().format, ExportFormat::Png);
}

#[test]
fn relative_export_dir_is_rejected() {
    for dir in ["", "Exports", "relative/dir"] {
        let snap = snapshot(10, &[("darkroom.export.dir", Some(dir))]);
        let (s, report) = import_web(&Settings::default(), &snap, "f", 10);
        assert_eq!(
            report
                .rejected
                .iter()
                .map(|(k, _)| k.as_str())
                .collect::<Vec<_>>(),
            vec!["darkroom.export.dir"],
            "must reject: {dir:?}"
        );
        assert_eq!(s.export_dir.value, None);
    }
    // Absolute POSIX and Windows-drive paths both pass.
    let snap = snapshot(10, &[("darkroom.export.dir", Some("/Users/me/E"))]);
    let (s, r) = import_web(&Settings::default(), &snap, "f", 10);
    assert!(r.rejected.is_empty() && s.export_dir.value.is_some());
    let snap = snapshot(10, &[("darkroom.export.dir", Some(r#"C:\Users\me\E"#))]);
    let (s, r) = import_web(&Settings::default(), &snap, "f", 10);
    assert!(r.rejected.is_empty() && s.export_dir.value.is_some());
}

#[test]
fn unknown_snapshot_keys_are_ignored() {
    let snap = snapshot(
        10,
        &[
            ("darkroom.some.future.key", Some("x")),
            ("darkroom.bootstrapped", Some("1")),
        ],
    );
    let (_, report) = import_web(&Settings::default(), &snap, "f", 10);
    assert_eq!(report.applied, vec!["darkroom.bootstrapped".to_owned()]);
    assert!(report.rejected.is_empty() && report.skipped_newer.is_empty());
}

#[test]
fn parse_settings_schema_and_corruption() {
    assert_eq!(
        parse_settings(r#"{"schema": 99}"#),
        Err(SettingsError::FutureSchema(99))
    );
    assert!(matches!(
        parse_settings("{"),
        Err(SettingsError::Corrupt(_))
    ));
    assert!(matches!(
        parse_settings("not json at all"),
        Err(SettingsError::Corrupt(_))
    ));
    assert_eq!(parse_settings("{}").unwrap(), Settings::default());

    // Defaults: switches on, nothing exported, all values marked Default, schema current.
    let s = parse_settings("{}").unwrap();
    assert_eq!(s.schema, SETTINGS_SCHEMA);
    assert!(!s.bootstrapped.value);
    assert!(s.auto_update_check.value);
    assert!(s.show_suggestions.value);
    assert_eq!(s.export_dir.value, None);
    assert_eq!(s.export_presets.value, Vec::<ExportPreset>::new());
    assert_eq!(s.export_last.value, None);
    for prov in [
        &s.bootstrapped.provenance,
        &s.auto_update_check.provenance,
        &s.show_suggestions.provenance,
        &s.export_dir.provenance,
        &s.export_presets.provenance,
        &s.export_last.provenance,
    ] {
        assert_eq!(prov, &Provenance::Default);
    }

    // Unknown extra fields are tolerated (forward-compatible minor additions).
    let s = parse_settings(r#"{"schema":1,"futureToggle":true,"layout":{"extra":1}}"#).unwrap();
    assert_eq!(s, Settings::default());
}

#[test]
fn future_schema_with_incompatible_shapes_is_future_not_corrupt() {
    let future = format!(
        r#"{{"schema": {}, "bootstrapped": 5, "autoUpdateCheck": "yes", "exportDir": 5}}"#,
        SETTINGS_SCHEMA + 1
    );
    assert_eq!(
        parse_settings(&future),
        Err(SettingsError::FutureSchema(SETTINGS_SCHEMA + 1))
    );
    // Same bad shapes under the current schema stay Corrupt.
    let current = format!(r#"{{"schema": {SETTINGS_SCHEMA}, "bootstrapped": 5}}"#);
    assert!(matches!(
        parse_settings(&current),
        Err(SettingsError::Corrupt(_))
    ));
    assert!(matches!(
        parse_settings("[99]"),
        Err(SettingsError::Corrupt(_))
    ));
}

#[test]
fn parse_web_snapshot_envelope_checks() {
    let ok = r#"{"format":"darkroom-web-settings","version":1,"capturedAtMs":5,"values":{}}"#;
    assert!(parse_web_snapshot(ok).is_ok());
    for bad in [
        r#"{"format":"other","version":1,"capturedAtMs":5,"values":{}}"#,
        r#"{"format":"darkroom-web-settings","version":2,"capturedAtMs":5,"values":{}}"#,
        r#"{"version":1,"capturedAtMs":5,"values":{}}"#,
    ] {
        assert_eq!(
            parse_web_snapshot(bad),
            Err(SettingsError::Corrupt(
                "unsupported web snapshot".to_owned()
            )),
            "must reject envelope: {bad}"
        );
    }
    assert!(matches!(
        parse_web_snapshot("{"),
        Err(SettingsError::Corrupt(_))
    ));
}

#[test]
fn json_round_trip_preserves_everything() {
    let snap = snapshot(
        100,
        &[
            ("darkroom.bootstrapped", Some("1")),
            ("darkroom.export.dir", Some("/Users/me/E")),
            (
                "darkroom.export.presets",
                Some(r#"[{"name":"A","template":"{original}"}]"#),
            ),
            (
                "darkroom.export.last",
                Some(r#"{"template":"{date}","format":"png","quality":7}"#),
            ),
        ],
    );
    let (mut s, _) = import_web(&Settings::default(), &snap, "export-file", 150);
    set_native(&mut s.show_suggestions, false, 250);
    s.layout.left_panel_width = Some(232.5);
    s.layout.window = Some(core_model::settings::WindowRect {
        x: -10,
        y: 20,
        width: 1280,
        height: 800,
    });

    let json = to_json(&s);
    assert_eq!(parse_settings(&json).unwrap(), s);
    // Pretty-printed and re-serializing is byte-stable.
    assert!(json.contains('\n'));
    assert_eq!(to_json(&parse_settings(&json).unwrap()), json);
}

#[test]
fn export_presets_are_structurally_not_develop_presets() {
    // Export presets are name+template only — no shared type or develop-payload fields.
    let v = serde_json::to_value(ExportPreset {
        name: "A".to_owned(),
        template: "{original}".to_owned(),
    })
    .unwrap();
    assert_eq!(v, json!({"name": "A", "template": "{original}"}));
    let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(keys, vec!["name", "template"]);
    // Develop params (the catalog-backed preset payload) is a distinct type with distinct fields.
    let d = serde_json::to_value(DevelopParams::default()).unwrap();
    assert!(d.as_object().is_none_or(|o| !o.is_empty()));
    assert!(!d.as_object().unwrap().contains_key("template"));
    let _: &DevelopParams = &DevelopParams::default(); // no type shared with ExportPreset
}

#[test]
fn import_report_defaults_are_empty() {
    let r = ImportReport::default();
    assert!(!r.already_imported);
    assert!(r.applied.is_empty() && r.skipped_newer.is_empty() && r.rejected.is_empty());
}
