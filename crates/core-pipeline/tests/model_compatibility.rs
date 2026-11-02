#[path = "support/model_snapshot.rs"]
mod model_snapshot;
#[test]
fn frozen_serialization_and_gpu_uniform_abi() {
    let expected: serde_json::Value =
        serde_json::from_str(include_str!("../../../tests/migration/model/legacy.json")).unwrap();
    assert_eq!(model_snapshot::snapshot(), expected);
}
