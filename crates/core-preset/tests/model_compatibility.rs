//! Exercise the existing sparse preset contract against the extracted typed model.
use core_model::{
    document::{EditDocument, ReadStatus},
    DevelopParams,
};
use serde_json::{json, Value};

#[test]
fn sparse_presets_preserve_untouched_edits_and_model_defaults() {
    let fixtures: Value =
        serde_json::from_str(include_str!("../../../tests/migration/model/legacy.json")).unwrap();
    for case in fixtures["cases"].as_array().unwrap() {
        let base = &case["resolved"];
        let mut overlay = json!({"exposure": 0.5}).as_object().unwrap().clone();
        core_preset::migrate_sparse(&mut overlay, 5);
        let applied = core_preset::apply_sparse(base, &overlay, 1.0);
        let mut expected = base.clone();
        expected["exposure"] = json!(0.5);
        let typed: DevelopParams = serde_json::from_value(applied).unwrap();
        assert_eq!(serde_json::to_value(typed).unwrap(), expected);
        let raw = serde_json::to_string(&expected).unwrap();
        assert_eq!(EditDocument::read(raw, 6).status(), &ReadStatus::Supported);
        assert_eq!(core_preset::apply_sparse(base, &overlay, 0.0), *base);
    }
}

#[test]
fn future_preset_fields_remain_in_json_and_fail_closed_in_typed_access() {
    let base = serde_json::to_value(DevelopParams::default()).unwrap();
    let overlay = json!({"future":{"strength":1}})
        .as_object()
        .unwrap()
        .clone();
    let merged = core_preset::apply_sparse(&base, &overlay, 1.0);
    let raw = serde_json::to_string(&merged).unwrap();
    let doc = EditDocument::read(&raw, 6);
    assert_eq!(doc.raw_json(), raw);
    assert_eq!(
        doc.status(),
        &ReadStatus::UnsupportedFields(vec!["/future".into()])
    );
    assert!(doc.preview().is_none());
}
