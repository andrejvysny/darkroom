use core_model::DevelopParams;
use serde_json::Value;

#[test]
fn resolved_serialization_matches_pre_extraction_defaults_and_all_fields() {
    let inputs: Value =
        serde_json::from_str(include_str!("../../../tests/migration/model/inputs.json")).unwrap();
    let legacy: Value =
        serde_json::from_str(include_str!("../../../tests/migration/model/legacy.json")).unwrap();
    let inputs = inputs.as_array().unwrap();
    let cases = legacy["cases"].as_array().unwrap();
    assert_eq!(inputs.len(), cases.len());
    for (input, expected) in inputs.iter().zip(cases) {
        assert_eq!(input["name"], expected["name"]);
        let params: DevelopParams = serde_json::from_value(input["params"].clone()).unwrap();
        let resolved = serde_json::to_value(&params).unwrap();
        assert_eq!(resolved, expected["resolved"]);
        let roundtrip: DevelopParams = serde_json::from_value(resolved.clone()).unwrap();
        assert_eq!(serde_json::to_value(roundtrip).unwrap(), resolved);
    }
}

#[test]
fn display_referred_stays_transient_and_never_serializes() {
    let params = DevelopParams {
        display_referred: true,
        ..Default::default()
    };
    let serialized = serde_json::to_value(&params).unwrap();
    assert!(!serialized
        .as_object()
        .unwrap()
        .contains_key("displayReferred"));
    let restored: DevelopParams = serde_json::from_value(serialized).unwrap();
    assert!(!restored.display_referred);
}
