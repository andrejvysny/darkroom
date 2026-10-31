//! Explicit recorder; compare output with the frozen pre-extraction fixture before replacing it.
#[path = "../tests/support/model_snapshot.rs"]
mod model_snapshot;
fn main() {
    println!(
        "{}",
        serde_json::to_string_pretty(&model_snapshot::snapshot()).unwrap()
    );
}
