//! Model session construction + first-run download/verify.
//!
//! Models are license-clean (D-FINE Apache-2.0, Florence-2 MIT) and hosted on Hugging Face. They are
//! downloaded once into the app-data `models/` dir (kept out of the `.dmg` for size/notarization).

use std::io::Read;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

#[cfg(target_os = "macos")]
use ort::ep::coreml::{ComputeUnits, ModelFormat};
use ort::session::builder::GraphOptimizationLevel;
use ort::session::Session;

use crate::error::AnalyzeError;

/// Build an `ort` Session with the platform's accelerated execution provider (CoreML on macOS,
/// DirectML on Windows, CPU elsewhere — all with graceful CPU fallback if the EP can't register)
/// at the given graph optimization level. `level1=true` is REQUIRED for the Florence-2 q4f16
/// components (default `All` trips a layernorm-fusion bug — see SPIKE.md); the D-FINE detector
/// loads fine at `All` (`level1=false`).
///
/// `mlprogram=true` selects CoreML's newer **MLProgram** model format (vs the legacy `NeuralNetwork`
/// default, which silently downcasts intermediates to FP16 and broadens op coverage). Used for the
/// detector to keep score boundaries stable/deterministic for the precision-gated decode; Florence-2
/// keeps the default (its q4f16 graph is tuned for it). Ignored on non-CoreML platforms.
pub fn build_session(
    model_path: &Path,
    level1: bool,
    mlprogram: bool,
) -> Result<Session, AnalyzeError> {
    if !model_path.exists() {
        return Err(AnalyzeError::ModelMissing(model_path.display().to_string()));
    }
    let mut b = Session::builder().map_err(AnalyzeError::inference)?;
    if level1 {
        b = b
            .with_optimization_level(GraphOptimizationLevel::Level1)
            .map_err(AnalyzeError::inference)?;
    }
    // Accelerated EP, selected per platform. Best-effort: ort logs a warning and falls back to
    // CPU if the EP can't register (e.g. no compatible GPU). `.build()` type-erases each EP to a
    // uniform `ExecutionProviderDispatch` so every platform feeds one list.
    #[allow(unused_mut)]
    let mut eps: Vec<ort::ep::ExecutionProviderDispatch> = Vec::new();
    #[cfg(target_os = "macos")]
    {
        let mut coreml = ort::ep::CoreML::default().with_compute_units(ComputeUnits::All);
        if mlprogram {
            // MLProgram can't compile the model's dynamic input dims; we always feed a fixed
            // shape, so require static input shapes (dynamic-shape nodes fall back to CPU).
            coreml = coreml
                .with_model_format(ModelFormat::MLProgram)
                .with_static_input_shapes(true);
        }
        eps.push(coreml.build());
    }
    #[cfg(target_os = "windows")]
    {
        // DirectML accelerates any DX12 GPU and needs no model-format hint.
        let _ = mlprogram;
        eps.push(ort::ep::DirectML::default().build());
    }
    // Other targets register no accelerated EP and run on CPU.
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let _ = mlprogram;
    // DirectML requires sequential execution with memory-pattern planning disabled; onnxruntime
    // otherwise disables mem-pattern itself (with a warning) when the DML EP registers. Set it
    // explicitly so the Windows path is deterministic. (Per-session `Run` is already serialized by
    // the `Mutex<Session>` each analyzer holds, satisfying DirectML's single-thread-Run rule.)
    #[cfg(target_os = "windows")]
    {
        b = b
            .with_memory_pattern(false)
            .map_err(AnalyzeError::inference)?
            .with_parallel_execution(false)
            .map_err(AnalyzeError::inference)?;
    }
    b = b
        .with_execution_providers(eps)
        .map_err(AnalyzeError::inference)?;
    b.commit_from_file(model_path)
        .map_err(AnalyzeError::inference)
}

/// Build a CPU-only `ort` Session (no CoreML EP). Used for models with dynamic sequence lengths that
/// the CoreML EP can't resize (e.g. the MobileCLIP text encoder, which runs only a handful of times
/// at startup so CPU latency is irrelevant).
pub fn build_session_cpu(model_path: &Path) -> Result<Session, AnalyzeError> {
    if !model_path.exists() {
        return Err(AnalyzeError::ModelMissing(model_path.display().to_string()));
    }
    Session::builder()
        .map_err(AnalyzeError::inference)?
        .commit_from_file(model_path)
        .map_err(AnalyzeError::inference)
}

/// A remote model file fetched on first run. `min_size` is a cheap pre-filter against truncated /
/// HTML-error bodies; `sha256` is the authoritative integrity check (lowercase hex, verified before
/// the atomic rename so a truncated / redirected / tampered body is never committed). The `.onnx`
/// pins are the Hugging Face LFS oids (= the file SHA-256); the two `tokenizer.json` and the
/// GitHub-hosted MegaDetector are pinned to the validated downloaded bytes.
pub struct RemoteFile {
    pub rel: &'static str,
    pub url: &'static str,
    pub min_size: u64,
    pub sha256: &'static str,
}

/// Object detector: D-FINE-M (52.3 mAP COCO, Apache-2.0). Same I/O as the spike's D-FINE-S.
pub const DETECTOR_FILES: &[RemoteFile] = &[RemoteFile {
    rel: "dfine_m.onnx",
    url: "https://huggingface.co/onnx-community/dfine_m_coco-ONNX/resolve/main/onnx/model.onnx",
    min_size: 40_000_000,
    sha256: "70aaa837978a06ba44ad17398c7079ae5a1a7b1a9032b5d7053981e1ada02d6b",
}];

/// Captioner: Florence-2-base-ft (MIT), q4f16 components + non-merged decoder pair + tokenizer.
pub const CAPTION_FILES: &[RemoteFile] = &[
    RemoteFile {
        rel: "florence2/vision_encoder.onnx",
        url: "https://huggingface.co/onnx-community/Florence-2-base-ft/resolve/main/onnx/vision_encoder_q4f16.onnx",
        min_size: 40_000_000,
        sha256: "1e993fb7081302294b5c286b2cc6c2a63283959f399317dc2be49eca94f2dd18",
    },
    RemoteFile {
        rel: "florence2/embed_tokens.onnx",
        url: "https://huggingface.co/onnx-community/Florence-2-base-ft/resolve/main/onnx/embed_tokens_q4f16.onnx",
        min_size: 40_000_000,
        sha256: "2c2a1663e8db3189699762d8e29ace6235cc9179b710326c99baa822c9ec96b8",
    },
    RemoteFile {
        rel: "florence2/encoder_model.onnx",
        url: "https://huggingface.co/onnx-community/Florence-2-base-ft/resolve/main/onnx/encoder_model_q4f16.onnx",
        min_size: 15_000_000,
        sha256: "1550d697836639b0fec53023dac96253342c2ec1e7fa682595fda80d157e9f64",
    },
    RemoteFile {
        rel: "florence2/decoder_model.onnx",
        url: "https://huggingface.co/onnx-community/Florence-2-base-ft/resolve/main/onnx/decoder_model_q4f16.onnx",
        min_size: 40_000_000,
        sha256: "ba61f607285efe9ee2c30c968ae7f4705957353479f76900a95c8d60573c54f3",
    },
    RemoteFile {
        rel: "florence2/tokenizer.json",
        url: "https://huggingface.co/onnx-community/Florence-2-base-ft/resolve/main/tokenizer.json",
        min_size: 1_000_000,
        sha256: "d69dcdb2323e124ac4f800cb9863ddccea0d7bb11e16125e8df3bd60f2f8aeac",
    },
];

/// Animal detector: MegaDetector v5a (MIT, YOLOv5x6). Community dynamic-axis ONNX — one file serves
/// both 640² and 1280² (the resolution is a runtime letterbox-target setting).
pub const ANIMAL_DETECTOR_FILES: &[RemoteFile] = &[RemoteFile {
    rel: "megadetector/md_v5a_dynamic.onnx",
    url: "https://github.com/bencevans/megadetector-onnx/releases/download/v0.2.0/md_v5a.0.0-dynamic.onnx",
    min_size: 400_000_000,
    sha256: "d00e778327cc2e67f0d4927d94da5a0494a470e23a13520d0fad569abf78adff",
}];

/// Face detector: SCRFD-10G-KPS (InsightFace `buffalo_l/detection`). NON-COMMERCIAL pretrained weights
/// — acceptable for this personal/local app; a commercial build must swap to a permissive model.
pub const FACE_DETECTOR_FILES: &[RemoteFile] = &[RemoteFile {
    rel: "faces/det_10g.onnx",
    url: "https://huggingface.co/immich-app/buffalo_l/resolve/main/detection/model.onnx",
    min_size: 15_000_000,
    sha256: "5838f7fe053675b1c7a08b633df49e7af5495cee0493c7dcf6697200b85b5b91",
}];

/// Face embedder: ArcFace `w600k_r50` (InsightFace `buffalo_l/recognition`), 512-d. Same
/// non-commercial caveat as the detector.
pub const FACE_EMBEDDER_FILES: &[RemoteFile] = &[RemoteFile {
    rel: "faces/w600k_r50.onnx",
    url: "https://huggingface.co/immich-app/buffalo_l/resolve/main/recognition/model.onnx",
    min_size: 150_000_000,
    sha256: "4c06341c33c2ca1f86781dab0e829f88ad5b64be9fba56e56bc9ebdefc619e43",
}];

/// Detection verifier: MobileCLIP-S1 (Apple, MIT) — fp32 vision + text encoders + CLIP tokenizer.
pub const VERIFIER_FILES: &[RemoteFile] = &[
    RemoteFile {
        rel: "mobileclip/vision_model.onnx",
        url: "https://huggingface.co/Xenova/mobileclip_s1/resolve/main/onnx/vision_model.onnx",
        min_size: 60_000_000,
        sha256: "5dece7da38f907d440f91c86cca841919c5a1affd2329ca0e94f8593fd0bfbfb",
    },
    RemoteFile {
        rel: "mobileclip/text_model.onnx",
        url: "https://huggingface.co/Xenova/mobileclip_s1/resolve/main/onnx/text_model.onnx",
        min_size: 150_000_000,
        sha256: "33b298fe97cfc9007e2a067fc8b5f8ae63689a161b4b72c9944e5078c1139b47",
    },
    RemoteFile {
        rel: "mobileclip/tokenizer.json",
        url: "https://huggingface.co/Xenova/mobileclip_s1/resolve/main/tokenizer.json",
        min_size: 1_000_000,
        sha256: "72ed5c96db5729294468543e4bc75fce14ca63f58e37300290189ba1c1e52b85",
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

    pub fn animal_detector_path(&self) -> PathBuf {
        self.path("megadetector/md_v5a_dynamic.onnx")
    }

    pub fn face_detector_path(&self) -> PathBuf {
        self.path("faces/det_10g.onnx")
    }

    pub fn face_embedder_path(&self) -> PathBuf {
        self.path("faces/w600k_r50.onnx")
    }

    /// `(vision, text, tokenizer)` paths for the MobileCLIP verifier.
    pub fn verifier_paths(&self) -> (PathBuf, PathBuf, PathBuf) {
        (
            self.path("mobileclip/vision_model.onnx"),
            self.path("mobileclip/text_model.onnx"),
            self.path("mobileclip/tokenizer.json"),
        )
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
                download(f.url, &dst, f.min_size, f.sha256)?;
            }
            progress(i + 1, total);
        }
        Ok(())
    }
}

fn download(url: &str, dst: &Path, min_size: u64, sha256: &str) -> Result<(), AnalyzeError> {
    let resp = ureq::get(url)
        .call()
        .map_err(|e| AnalyzeError::Download(format!("{url}: {e}")))?;
    let tmp = dst.with_extension("part");
    let digest = {
        let mut reader = resp.into_body().into_reader();
        let mut out = std::fs::File::create(&tmp)?;
        let mut hasher = Sha256::new();
        let mut buf = [0u8; 1 << 16];
        let mut written: u64 = 0;
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            std::io::Write::write_all(&mut out, &buf[..n])?;
            written += n as u64;
        }
        // Cheap pre-filter: a body shorter than the floor is a truncated / HTML-error response.
        if written < min_size {
            let _ = std::fs::remove_file(&tmp);
            return Err(AnalyzeError::Download(format!(
                "{url}: short body ({written} < {min_size})"
            )));
        }
        hex_lower(&hasher.finalize())
    };
    // Authoritative integrity gate: a truncated-but-large, redirected, or tampered/substituted body
    // fails here and is removed BEFORE the atomic rename, so a corrupt file never lands on disk (and
    // can never get stuck passing the `min_size`-only `present()` check forever). This rename is the
    // trust boundary; we never re-hash on the routine startup `present()`/`has_all()` path.
    if !digest.eq_ignore_ascii_case(sha256) {
        let _ = std::fs::remove_file(&tmp);
        return Err(AnalyzeError::Download(format!(
            "{url}: sha256 mismatch (expected {sha256}, got {digest})"
        )));
    }
    std::fs::rename(&tmp, dst)?;
    Ok(())
}

/// Lowercase-hex encoding of a digest.
fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}
