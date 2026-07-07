//! Emits `cfg(have_pmrid)` when the PMRID denoise ONNX has been exported into `assets/`, so the crate
//! compiles both before and after the model exists (see `scripts/export_pmrid.py`). Without the model
//! the denoise feature transparently falls back to the classical `CpuDenoiser`.
use std::path::Path;

fn main() {
    println!("cargo:rustc-check-cfg=cfg(have_pmrid)");
    let onnx = "assets/pmrid_4x256x256.onnx";
    println!("cargo:rerun-if-changed={onnx}");
    if Path::new(onnx).exists() {
        println!("cargo:rustc-cfg=have_pmrid");
    }
}
