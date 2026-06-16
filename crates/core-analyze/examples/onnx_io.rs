//! Phase 0 helper: print the input/output signature of one or more ONNX models.
//! Tries graph-optimization levels (default=All, then Level1, then Disable) so we can tell whether
//! a load failure is an optimizer-fusion bug (fixable by lowering the level) vs a model-validity bug.
//! Usage: cargo run -p core-analyze --example onnx_io -- a.onnx b.onnx ...

use anyhow::{anyhow, Result};
use ort::session::builder::GraphOptimizationLevel;
use ort::session::Session;

fn try_load(path: &str, level: GraphOptimizationLevel) -> Result<Session> {
    let mut b = Session::builder().map_err(|e| anyhow!("{e}"))?;
    b = b
        .with_optimization_level(level)
        .map_err(|e| anyhow!("{e}"))?;
    b.commit_from_file(path).map_err(|e| anyhow!("{e}"))
}

fn main() {
    for path in std::env::args().skip(1) {
        println!("\n===== {} =====", path.rsplit('/').next().unwrap_or(&path));
        let levels = [
            ("All", GraphOptimizationLevel::All),
            ("Level1", GraphOptimizationLevel::Level1),
            ("Disable", GraphOptimizationLevel::Disable),
        ];
        let mut loaded = None;
        for (name, lvl) in levels {
            match try_load(&path, lvl) {
                Ok(s) => {
                    println!("  loaded OK @ optimization={name}");
                    loaded = Some(s);
                    break;
                }
                Err(e) => {
                    let msg = e.to_string();
                    let short = msg
                        .lines()
                        .next()
                        .unwrap_or("")
                        .chars()
                        .take(140)
                        .collect::<String>();
                    println!("  FAIL @ optimization={name}: {short}");
                }
            }
        }
        if let Some(s) = loaded {
            for i in s.inputs() {
                println!("    IN  {}", outlet(i));
            }
            for o in s.outputs() {
                println!("    OUT {}", outlet(o));
            }
        }
    }
}

/// Condense an Outlet's Debug into `name : ty [shape]`.
fn outlet<T: std::fmt::Debug>(o: &T) -> String {
    let d = format!("{o:?}");
    let name = d
        .split("name: \"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .unwrap_or("?");
    let ty = d
        .split("ty: ")
        .nth(1)
        .and_then(|s| s.split(',').next())
        .unwrap_or("?");
    let shape = d
        .split("shape: [")
        .nth(1)
        .and_then(|s| s.split(']').next())
        .map(|s| s.split_whitespace().collect::<String>())
        .unwrap_or_default();
    format!("{name} : {ty} [{shape}]")
}
