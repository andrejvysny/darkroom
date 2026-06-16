//! Model session construction + first-run download/verify.
//!
//! Models are license-clean (D-FINE Apache-2.0, Florence-2 MIT) and hosted on Hugging Face. They are
//! downloaded once into the app-data `models/` dir (kept out of the `.dmg` for size/notarization).

use std::io::Read;
use std::path::{Path, PathBuf};

use ort::ep::coreml::ComputeUnits;
use ort::session::builder::GraphOptimizationLevel;
use ort::session::Session;

use crate::error::AnalyzeError;

/// Build an `ort` Session with CoreML (graceful CPU fallback if unavailable) at the given graph
/// optimization level. `level1=true` is REQUIRED for the Florence-2 q4f16 components (default `All`
/// trips a layernorm-fusion bug — see SPIKE.md); the D-FINE detector loads fine at `All` (`level1=false`).
pub fn build_session(model_path: &Path, level1: bool) -> Result<Session, AnalyzeError> {
    if !model_path.exists() {
        return Err(AnalyzeError::ModelMissing(model_path.display().to_string()));
    }
    let coreml = ort::ep::CoreML::default()
        .with_compute_units(ComputeUnits::All)
        .build();
    let mut b = Session::builder().map_err(AnalyzeError::inference)?;
    if level1 {
        b = b
            .with_optimization_level(GraphOptimizationLevel::Level1)
            .map_err(AnalyzeError::inference)?;
    }
    // Best-effort EP: ort logs a warning and falls back to CPU if CoreML can't register.
    b = b
        .with_execution_providers([coreml])
        .map_err(AnalyzeError::inference)?;
    b.commit_from_file(model_path)
        .map_err(AnalyzeError::inference)
}

/// A remote model file fetched on first run. `min_size` guards against truncated / HTML-error bodies.
pub struct RemoteFile {
    pub rel: &'static str,
    pub url: &'static str,
    pub min_size: u64,
}

/// Object detector: D-FINE-M (52.3 mAP COCO, Apache-2.0). Same I/O as the spike's D-FINE-S.
pub const DETECTOR_FILES: &[RemoteFile] = &[RemoteFile {
    rel: "dfine_m.onnx",
    url: "https://huggingface.co/onnx-community/dfine_m_coco-ONNX/resolve/main/onnx/model.onnx",
    min_size: 40_000_000,
}];

/// Captioner: Florence-2-base-ft (MIT), q4f16 components + non-merged decoder pair + tokenizer.
pub const CAPTION_FILES: &[RemoteFile] = &[
    RemoteFile {
        rel: "florence2/vision_encoder.onnx",
        url: "https://huggingface.co/onnx-community/Florence-2-base-ft/resolve/main/onnx/vision_encoder_q4f16.onnx",
        min_size: 40_000_000,
    },
    RemoteFile {
        rel: "florence2/embed_tokens.onnx",
        url: "https://huggingface.co/onnx-community/Florence-2-base-ft/resolve/main/onnx/embed_tokens_q4f16.onnx",
        min_size: 40_000_000,
    },
    RemoteFile {
        rel: "florence2/encoder_model.onnx",
        url: "https://huggingface.co/onnx-community/Florence-2-base-ft/resolve/main/onnx/encoder_model_q4f16.onnx",
        min_size: 15_000_000,
    },
    RemoteFile {
        rel: "florence2/decoder_model.onnx",
        url: "https://huggingface.co/onnx-community/Florence-2-base-ft/resolve/main/onnx/decoder_model_q4f16.onnx",
        min_size: 40_000_000,
    },
    RemoteFile {
        rel: "florence2/tokenizer.json",
        url: "https://huggingface.co/onnx-community/Florence-2-base-ft/resolve/main/tokenizer.json",
        min_size: 1_000_000,
    },
];

/// On-disk model directory (typically `<app-data>/models`).
pub struct ModelStore {
    dir: PathBuf,
}

impl ModelStore {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.dir.join(rel)
    }

    pub fn detector_path(&self) -> PathBuf {
        self.path("dfine_m.onnx")
    }

    pub fn florence_dir(&self) -> PathBuf {
        self.path("florence2")
    }

    fn present(&self, f: &RemoteFile) -> bool {
        self.path(f.rel)
            .metadata()
            .map(|m| m.len() >= f.min_size)
            .unwrap_or(false)
    }

    /// True once every file in `files` is present at >= its `min_size`.
    pub fn has_all(&self, files: &[RemoteFile]) -> bool {
        files.iter().all(|f| self.present(f))
    }

    /// Download any missing files. `progress(done, total)` fires after each file. Idempotent: present
    /// files are skipped, downloads land via a `.part` temp + atomic rename.
    pub fn ensure(
        &self,
        files: &[RemoteFile],
        mut progress: impl FnMut(usize, usize),
    ) -> Result<(), AnalyzeError> {
        std::fs::create_dir_all(&self.dir)?;
        let total = files.len();
        for (i, f) in files.iter().enumerate() {
            if !self.present(f) {
                let dst = self.path(f.rel);
                if let Some(parent) = dst.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                download(f.url, &dst, f.min_size)?;
            }
            progress(i + 1, total);
        }
        Ok(())
    }
}

fn download(url: &str, dst: &Path, min_size: u64) -> Result<(), AnalyzeError> {
    let resp = ureq::get(url)
        .call()
        .map_err(|e| AnalyzeError::Download(format!("{url}: {e}")))?;
    let tmp = dst.with_extension("part");
    {
        let mut reader = resp.into_body().into_reader();
        let mut out = std::fs::File::create(&tmp)?;
        let mut buf = [0u8; 1 << 16];
        let mut written: u64 = 0;
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            std::io::Write::write_all(&mut out, &buf[..n])?;
            written += n as u64;
        }
        if written < min_size {
            let _ = std::fs::remove_file(&tmp);
            return Err(AnalyzeError::Download(format!(
                "{url}: short body ({written} < {min_size})"
            )));
        }
    }
    std::fs::rename(&tmp, dst)?;
    Ok(())
}
