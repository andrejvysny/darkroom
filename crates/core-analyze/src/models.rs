//! Model session construction + first-run download/verify.
//!
//! Models are license-clean (D-FINE Apache-2.0, Florence-2 MIT) and hosted on Hugging Face. They are
//! downloaded once into the app-data `models/` dir (kept out of the `.dmg` for size/notarization).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

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

/// Build an `ort` Session from an in-memory ONNX (for a model embedded via `include_bytes!` — the
/// PMRID denoiser), with the same accelerated EP config as [`build_session`]. `mlprogram` selects
/// CoreML's MLProgram format with static input shapes (our denoise tiles are a fixed 256×256), which
/// keeps a pure-conv graph on the ANE/GPU.
pub fn build_session_from_memory(
    model_bytes: &[u8],
    level1: bool,
    mlprogram: bool,
) -> Result<Session, AnalyzeError> {
    let mut b = Session::builder().map_err(AnalyzeError::inference)?;
    if level1 {
        b = b
            .with_optimization_level(GraphOptimizationLevel::Level1)
            .map_err(AnalyzeError::inference)?;
    }
    #[allow(unused_mut)]
    let mut eps: Vec<ort::ep::ExecutionProviderDispatch> = Vec::new();
    #[cfg(target_os = "macos")]
    {
        let mut coreml = ort::ep::CoreML::default().with_compute_units(ComputeUnits::All);
        if mlprogram {
            coreml = coreml
                .with_model_format(ModelFormat::MLProgram)
                .with_static_input_shapes(true);
        }
        eps.push(coreml.build());
    }
    #[cfg(target_os = "windows")]
    {
        let _ = mlprogram;
        eps.push(ort::ep::DirectML::default().build());
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let _ = mlprogram;
    #[cfg(target_os = "windows")]
    {
        b = b
            .with_memory_pattern(false)
            .map_err(AnalyzeError::inference)?
            .with_parallel_execution(false)
            .map_err(AnalyzeError::inference)?;
    }
    b.with_execution_providers(eps)
        .map_err(AnalyzeError::inference)?
        .commit_from_memory(model_bytes)
        .map_err(AnalyzeError::inference)
}

/// A remote model file fetched on first run. `min_size` is a cheap pre-filter against truncated /
/// HTML-error bodies; `sha256` is the authoritative integrity check (lowercase hex, verified before
/// the atomic rename so a truncated / redirected / tampered body is never committed). `approx_size` is
/// a rough OVER-estimate of the real byte size — used only as a fallback for the progress denominator
/// and disk precheck when a HEAD `Content-Length` is unavailable. The `.onnx` pins are the Hugging
/// Face LFS oids (= the file SHA-256); the two `tokenizer.json` and the GitHub-hosted MegaDetector are
/// pinned to the validated downloaded bytes.
#[derive(Clone, Copy)]
pub struct RemoteFile {
    pub rel: &'static str,
    pub url: &'static str,
    pub min_size: u64,
    pub approx_size: u64,
    pub sha256: &'static str,
}

/// Object detector: D-FINE-M (52.3 mAP COCO, Apache-2.0). Same I/O as the spike's D-FINE-S.
pub const DETECTOR_FILES: &[RemoteFile] = &[RemoteFile {
    rel: "dfine_m.onnx",
    url: "https://huggingface.co/onnx-community/dfine_m_coco-ONNX/resolve/main/onnx/model.onnx",
    min_size: 40_000_000,
    approx_size: 130_000_000,
    sha256: "70aaa837978a06ba44ad17398c7079ae5a1a7b1a9032b5d7053981e1ada02d6b",
}];

/// Captioner: Florence-2-base-ft (MIT), q4f16 components + non-merged decoder pair + tokenizer.
pub const CAPTION_FILES: &[RemoteFile] = &[
    RemoteFile {
        rel: "florence2/vision_encoder.onnx",
        url: "https://huggingface.co/onnx-community/Florence-2-base-ft/resolve/main/onnx/vision_encoder_q4f16.onnx",
        min_size: 40_000_000,
        approx_size: 60_000_000,
        sha256: "1e993fb7081302294b5c286b2cc6c2a63283959f399317dc2be49eca94f2dd18",
    },
    RemoteFile {
        rel: "florence2/embed_tokens.onnx",
        url: "https://huggingface.co/onnx-community/Florence-2-base-ft/resolve/main/onnx/embed_tokens_q4f16.onnx",
        min_size: 40_000_000,
        approx_size: 70_000_000,
        sha256: "2c2a1663e8db3189699762d8e29ace6235cc9179b710326c99baa822c9ec96b8",
    },
    RemoteFile {
        rel: "florence2/encoder_model.onnx",
        url: "https://huggingface.co/onnx-community/Florence-2-base-ft/resolve/main/onnx/encoder_model_q4f16.onnx",
        min_size: 15_000_000,
        approx_size: 30_000_000,
        sha256: "1550d697836639b0fec53023dac96253342c2ec1e7fa682595fda80d157e9f64",
    },
    RemoteFile {
        rel: "florence2/decoder_model.onnx",
        url: "https://huggingface.co/onnx-community/Florence-2-base-ft/resolve/main/onnx/decoder_model_q4f16.onnx",
        min_size: 40_000_000,
        approx_size: 70_000_000,
        sha256: "ba61f607285efe9ee2c30c968ae7f4705957353479f76900a95c8d60573c54f3",
    },
    RemoteFile {
        rel: "florence2/tokenizer.json",
        url: "https://huggingface.co/onnx-community/Florence-2-base-ft/resolve/main/tokenizer.json",
        min_size: 1_000_000,
        approx_size: 3_000_000,
        sha256: "d69dcdb2323e124ac4f800cb9863ddccea0d7bb11e16125e8df3bd60f2f8aeac",
    },
];

/// Animal detector: MegaDetector v5a (MIT, YOLOv5x6). Community dynamic-axis ONNX — one file serves
/// both 640² and 1280² (the resolution is a runtime letterbox-target setting).
pub const ANIMAL_DETECTOR_FILES: &[RemoteFile] = &[RemoteFile {
    rel: "megadetector/md_v5a_dynamic.onnx",
    url: "https://github.com/bencevans/megadetector-onnx/releases/download/v0.2.0/md_v5a.0.0-dynamic.onnx",
    min_size: 400_000_000,
    approx_size: 560_000_000,
    sha256: "d00e778327cc2e67f0d4927d94da5a0494a470e23a13520d0fad569abf78adff",
}];

/// Face detector: SCRFD-10G-KPS (InsightFace `buffalo_l/detection`). NON-COMMERCIAL pretrained weights
/// — acceptable for this personal/local app; a commercial build must swap to a permissive model.
pub const FACE_DETECTOR_FILES: &[RemoteFile] = &[RemoteFile {
    rel: "faces/det_10g.onnx",
    url: "https://huggingface.co/immich-app/buffalo_l/resolve/main/detection/model.onnx",
    min_size: 15_000_000,
    approx_size: 20_000_000,
    sha256: "5838f7fe053675b1c7a08b633df49e7af5495cee0493c7dcf6697200b85b5b91",
}];

/// Face embedder: ArcFace `w600k_r50` (InsightFace `buffalo_l/recognition`), 512-d. Same
/// non-commercial caveat as the detector.
pub const FACE_EMBEDDER_FILES: &[RemoteFile] = &[RemoteFile {
    rel: "faces/w600k_r50.onnx",
    url: "https://huggingface.co/immich-app/buffalo_l/resolve/main/recognition/model.onnx",
    min_size: 150_000_000,
    approx_size: 180_000_000,
    sha256: "4c06341c33c2ca1f86781dab0e829f88ad5b64be9fba56e56bc9ebdefc619e43",
}];

/// Detection verifier: MobileCLIP-S1 (Apple, MIT) — fp32 vision + text encoders + CLIP tokenizer.
pub const VERIFIER_FILES: &[RemoteFile] = &[
    RemoteFile {
        rel: "mobileclip/vision_model.onnx",
        url: "https://huggingface.co/Xenova/mobileclip_s1/resolve/main/onnx/vision_model.onnx",
        min_size: 60_000_000,
        approx_size: 90_000_000,
        sha256: "5dece7da38f907d440f91c86cca841919c5a1affd2329ca0e94f8593fd0bfbfb",
    },
    RemoteFile {
        rel: "mobileclip/text_model.onnx",
        url: "https://huggingface.co/Xenova/mobileclip_s1/resolve/main/onnx/text_model.onnx",
        min_size: 150_000_000,
        approx_size: 180_000_000,
        sha256: "33b298fe97cfc9007e2a067fc8b5f8ae63689a161b4b72c9944e5078c1139b47",
    },
    RemoteFile {
        rel: "mobileclip/tokenizer.json",
        url: "https://huggingface.co/Xenova/mobileclip_s1/resolve/main/tokenizer.json",
        min_size: 1_000_000,
        approx_size: 3_000_000,
        sha256: "72ed5c96db5729294468543e4bc75fce14ca63f58e37300290189ba1c1e52b85",
    },
];

/// Interactive segmentation (AI masking): **MobileSAM** (Apache-2.0) ONNX export by `Acly/MobileSAM`,
/// pinned to an immutable HF commit. Split encode/decode: the TinyViT image encoder runs once per
/// image (`input_image` HWC f32 RGB 0–255, longest side pre-resized to 1024 → `image_embeddings
/// [1,256,64,64]`); the tiny mask decoder runs per click (`point_coords` in the resized frame + a
/// trailing `[0,0]`/label `-1` pad point, `orig_im_size=[H,W]` → `masks[1,4,H,W]` logits, `iou[1,4]`,
/// and `low_res_masks[1,4,256,256]`). We ship the **multi** decoder (returns 3–4 masks; pick best IoU).
/// Pinned to HF commit `0d3b4033…` (immutable). LFS oids = the file SHA-256.
/// Realtime tier — MobileSAM (Acly HWC-baked encoder + multi-mask decoder).
pub const SAM_MOBILE_FILES: &[RemoteFile] = &[
    RemoteFile {
        rel: "sam/mobile_sam_image_encoder.onnx",
        url: "https://huggingface.co/Acly/MobileSAM/resolve/0d3b403339b4674a82493d5e97964dd78089ddc8/mobile_sam_image_encoder.onnx",
        min_size: 20_000_000,
        approx_size: 35_000_000,
        sha256: "580f5fb648ea1062c0aabc26217aed56921985f03f0cbbd852bba81d760cc749",
    },
    RemoteFile {
        rel: "sam/sam_mask_decoder_multi.onnx",
        url: "https://huggingface.co/Acly/MobileSAM/resolve/0d3b403339b4674a82493d5e97964dd78089ddc8/sam_mask_decoder_multi.onnx",
        min_size: 12_000_000,
        approx_size: 18_000_000,
        sha256: "8976b90a87ba50a6a72217a5ff994f7d25ce16f2229fcc1ed259e1294c622ffe",
    },
];

/// Balanced tier — **SAM ViT-B** (359 MB encoder). Standard samexporter export: CHW-preprocessed
/// `[1,3,1024,1024]` encoder input + the standard decoder interface. Validated end-to-end (IoU 0.99).
pub const SAM_VITB_FILES: &[RemoteFile] = &[
    RemoteFile {
        rel: "sam/sam_vit_b.encoder.onnx",
        url: "https://huggingface.co/rajlab/sam_vit_b/resolve/main/encoder.onnx",
        min_size: 300_000_000,
        approx_size: 380_000_000,
        sha256: "08d1b45ed63bcb74a6504f01392dafd4b39b9c1df657321b914f4bf56b3754c9",
    },
    RemoteFile {
        rel: "sam/sam_vit_b.decoder.onnx",
        url: "https://huggingface.co/rajlab/sam_vit_b/resolve/main/decoder.onnx",
        min_size: 12_000_000,
        approx_size: 18_000_000,
        sha256: "67eeb654773f692dd6c5f5c54c8ea3de3be2790cc293ee3c8d0dee137a7751c7",
    },
];

/// Max tier — **SAM ViT-L** (1.23 GB encoder, Rookiehan export). Unlike Balanced's rajlab ViT-B, this
/// export BAKES preprocessing into the graph (HWC dynamic input, like MobileSAM → `HwcBaked` preproc);
/// standard decoder. Encode ~3.4 s CPU.
pub const SAM_VITL_FILES: &[RemoteFile] = &[
    RemoteFile {
        rel: "sam/sam_vit_l.encoder.onnx",
        url: "https://huggingface.co/Rookiehan/sam/resolve/main/sam_vit_l_0b3195.encoder.onnx",
        min_size: 1_000_000_000,
        approx_size: 1_300_000_000,
        sha256: "8218869dc78946dd5ef58031ebcf443f66aa26489ac19dd9359d47a780abbe8a",
    },
    RemoteFile {
        rel: "sam/sam_vit_l.decoder.onnx",
        url: "https://huggingface.co/Rookiehan/sam/resolve/main/sam_vit_l_0b3195.decoder.onnx",
        min_size: 12_000_000,
        approx_size: 18_000_000,
        sha256: "90905d3895cc7053ced195bc2e8815fad71b528ee809825fc14a402184faec72",
    },
];

/// Quality tier for interactive segmentation, user-selectable in Settings. Each maps to a distinct
/// model with its own encoder-preprocessing convention (the decoder interface is identical). Realtime
/// is a HWC-baked MobileSAM export; Balanced/Max are standard CHW-preprocessed SAM ViT-B/L exports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SamTier {
    Realtime,
    Balanced,
    Max,
}

impl SamTier {
    pub fn as_str(self) -> &'static str {
        match self {
            SamTier::Realtime => "realtime",
            SamTier::Balanced => "balanced",
            SamTier::Max => "max",
        }
    }

    /// Parse a stored tier tag (see [`Self::as_str`]); unknown tags fall back to `Realtime`.
    pub fn from_tag(s: &str) -> Self {
        match s {
            "balanced" => SamTier::Balanced,
            "max" => SamTier::Max,
            _ => SamTier::Realtime,
        }
    }

    /// All three tiers, in display order (Realtime → Balanced → Max).
    pub fn all() -> [SamTier; 3] {
        [SamTier::Realtime, SamTier::Balanced, SamTier::Max]
    }

    /// The two ONNX files (encoder, decoder) to download for this tier.
    pub fn files(self) -> &'static [RemoteFile] {
        match self {
            SamTier::Realtime => SAM_MOBILE_FILES,
            SamTier::Balanced => SAM_VITB_FILES,
            SamTier::Max => SAM_VITL_FILES,
        }
    }

    /// Relative on-disk encoder/decoder paths (index 0 = encoder, 1 = decoder in [`Self::files`]).
    pub fn encoder_rel(self) -> &'static str {
        self.files()[0].rel
    }
    pub fn decoder_rel(self) -> &'static str {
        self.files()[1].rel
    }

    /// Whether this tier's encoder must run CPU-only. The 1.23 GB ViT-L (Max) crashes the CoreML
    /// NeuralNetwork backend (no clean fallback); CPU runs it reliably at ~3.4 s.
    pub fn encoder_cpu(self) -> bool {
        matches!(self, SamTier::Max)
    }

    /// The encoder input convention for this tier — this differs by EXPORT SOURCE, not backbone:
    /// Realtime (Acly MobileSAM) and Max (Rookiehan ViT-L) bake resize/normalize/pad into the graph
    /// (`HwcBaked`), while Balanced (rajlab ViT-B) takes a pre-processed CHW tensor. Verified per model.
    pub fn preproc(self) -> crate::sam::SamPreproc {
        match self {
            SamTier::Realtime | SamTier::Max => crate::sam::SamPreproc::HwcBaked,
            SamTier::Balanced => crate::sam::SamPreproc::Chw,
        }
    }
}

/// Per-tick download progress passed to [`ModelStore::ensure_with`]. Byte fields let the UI render a
/// real-time bar + speed/ETA; `file_*` is the currently-downloading file, `group_*` is the running
/// aggregate across every not-yet-present file in the group.
pub struct DlProgress {
    pub file_index: usize,
    pub file_count: usize,
    pub file_name: String,
    pub file_bytes_done: u64,
    pub file_bytes_total: u64,
    pub group_bytes_done: u64,
    pub group_bytes_total: u64,
    /// `"downloading"` while transferring, `"done"` on the final tick.
    pub phase: &'static str,
}

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

    pub fn sam_encoder_path(&self, tier: SamTier) -> PathBuf {
        self.path(tier.encoder_rel())
    }

    pub fn sam_decoder_path(&self, tier: SamTier) -> PathBuf {
        self.path(tier.decoder_rel())
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

    /// On-disk byte size of the present files in `files` (0 for absent ones). For the manager's
    /// "installed size" readout.
    pub fn installed_bytes(&self, files: &[RemoteFile]) -> u64 {
        files
            .iter()
            .filter_map(|f| self.path(f.rel).metadata().ok().map(|m| m.len()))
            .sum()
    }

    /// Download any missing files. `progress(done, total)` fires per FILE. Kept for callers that only
    /// need file-count granularity; delegates to [`Self::ensure_with`] with a no-op cancel.
    pub fn ensure(
        &self,
        files: &[RemoteFile],
        mut progress: impl FnMut(usize, usize),
    ) -> Result<(), AnalyzeError> {
        self.ensure_with(files, |p| progress(p.file_index, p.file_count), &|| false)
    }

    /// Download any missing files with byte-level progress + cooperative cancellation. Idempotent:
    /// present files are skipped, downloads land via a unique `.part` temp + sha256-gated atomic
    /// rename (the trust boundary). Precondition-checks free disk space against the sum of the
    /// still-needed files (HEAD-sized, `approx_size` fallback) before transferring a single byte.
    pub fn ensure_with(
        &self,
        files: &[RemoteFile],
        mut progress: impl FnMut(DlProgress),
        cancel: &dyn Fn() -> bool,
    ) -> Result<(), AnalyzeError> {
        std::fs::create_dir_all(&self.dir)?;
        // Bail before any HEAD/network if already cancelled.
        if cancel() {
            return Err(AnalyzeError::Cancelled);
        }
        let file_count = files.len();

        // Size each still-needed file (present ones contribute 0 to the download bar). HEAD gives the
        // exact Content-Length; `approx_size`/`min_size` are the fallback if HEAD fails.
        let mut sizes: Vec<u64> = Vec::with_capacity(file_count);
        let mut group_bytes_total: u64 = 0;
        for f in files {
            if self.present(f) {
                sizes.push(0);
            } else {
                let sz = head_size(f.url).unwrap_or(f.approx_size).max(f.min_size);
                sizes.push(sz);
                group_bytes_total += sz;
            }
        }

        // Disk precheck for the remaining work (skip when everything is already present).
        if group_bytes_total > 0 {
            const MARGIN: u64 = 200 * 1024 * 1024;
            let needed = group_bytes_total.saturating_add(MARGIN);
            if let Ok(available) = fs4::statvfs(&self.dir).map(|s| s.available_space()) {
                if available < needed {
                    return Err(AnalyzeError::InsufficientSpace { needed, available });
                }
            }
        }

        let mut group_done: u64 = 0;
        for (i, f) in files.iter().enumerate() {
            if cancel() {
                return Err(AnalyzeError::Cancelled);
            }
            if self.present(f) {
                progress(DlProgress {
                    file_index: i + 1,
                    file_count,
                    file_name: f.rel.to_string(),
                    file_bytes_done: 0,
                    file_bytes_total: 0,
                    group_bytes_done: group_done,
                    group_bytes_total,
                    phase: "downloading",
                });
                continue;
            }
            let dst = self.path(f.rel);
            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let file_total = sizes[i];
            let base = group_done;
            download_with_retry(
                f.url,
                &dst,
                f.min_size,
                f.sha256,
                file_total,
                cancel,
                |fdone, ftotal| {
                    progress(DlProgress {
                        file_index: i + 1,
                        file_count,
                        file_name: f.rel.to_string(),
                        file_bytes_done: fdone,
                        file_bytes_total: ftotal,
                        group_bytes_done: base + fdone.min(file_total),
                        group_bytes_total,
                        phase: "downloading",
                    });
                },
            )?;
            group_done += file_total;
        }
        progress(DlProgress {
            file_index: file_count,
            file_count,
            file_name: String::new(),
            file_bytes_done: 0,
            file_bytes_total: 0,
            group_bytes_done: group_bytes_total,
            group_bytes_total,
            phase: "done",
        });
        Ok(())
    }

    /// Delete every file in `files` and prune any now-empty model subdirs. Used by the manager's
    /// "Remove" action to reclaim disk. Absent files are ignored.
    pub fn remove(&self, files: &[RemoteFile]) -> std::io::Result<()> {
        for f in files {
            let p = self.path(f.rel);
            if p.exists() {
                std::fs::remove_file(&p)?;
            }
        }
        for sub in ["florence2", "faces", "mobileclip", "megadetector", "sam"] {
            let d = self.dir.join(sub);
            if d.is_dir()
                && std::fs::read_dir(&d)
                    .map(|mut rd| rd.next().is_none())
                    .unwrap_or(false)
            {
                let _ = std::fs::remove_dir(&d);
            }
        }
        Ok(())
    }
}

/// HEAD a URL for its `Content-Length`, following redirects (HF `resolve` → CDN). `None` if the
/// server omits it or the request fails — the caller falls back to `approx_size`.
fn head_size(url: &str) -> Option<u64> {
    ureq::head(url)
        .call()
        .ok()
        .and_then(|r| r.body().content_length())
}

/// True for transient failures worth retrying. Integrity (sha mismatch), cancellation, and
/// insufficient-space are deterministic — retrying them is pointless.
fn is_retryable(e: &AnalyzeError) -> bool {
    matches!(e, AnalyzeError::Download(_) | AnalyzeError::Io(_))
}

/// [`download`] wrapped in a bounded retry with exponential backoff (1s/2s), for transient network
/// blips. Never retries a cancellation or an integrity failure. Runs inside `spawn_blocking`, so the
/// blocking sleep is fine.
fn download_with_retry(
    url: &str,
    dst: &Path,
    min_size: u64,
    sha256: &str,
    file_total: u64,
    cancel: &dyn Fn() -> bool,
    mut progress: impl FnMut(u64, u64),
) -> Result<(), AnalyzeError> {
    const MAX_ATTEMPTS: u32 = 3;
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        if cancel() {
            return Err(AnalyzeError::Cancelled);
        }
        match download(
            url,
            dst,
            min_size,
            sha256,
            file_total,
            &mut progress,
            cancel,
        ) {
            Ok(()) => return Ok(()),
            Err(e) if !is_retryable(&e) || attempt >= MAX_ATTEMPTS => return Err(e),
            Err(_) => std::thread::sleep(Duration::from_secs(1u64 << (attempt - 1))),
        }
    }
}

fn download(
    url: &str,
    dst: &Path,
    min_size: u64,
    sha256: &str,
    file_total: u64,
    mut progress: impl FnMut(u64, u64),
    cancel: &dyn Fn() -> bool,
) -> Result<(), AnalyzeError> {
    let resp = ureq::get(url)
        .call()
        .map_err(|e| AnalyzeError::Download(format!("{url}: {e}")))?;
    // Authoritative per-file total for the bar: the GET's own Content-Length, else the HEAD-derived
    // `file_total`, never below `min_size`.
    let total = resp
        .body()
        .content_length()
        .unwrap_or(file_total)
        .max(min_size);
    // UNIQUE temp per download: two concurrent downloads of the same file must not share a `.part`
    // (interleaved writes corrupt the on-disk bytes while each stream's own hasher still computes the
    // correct sha — the corrupt file then passes the sha gate and gets renamed into place).
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let uniq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = dst.with_extension(format!("part.{}.{uniq}", std::process::id()));

    // Stream → hash → write. Any early return here removes the temp so a failed/cancelled attempt
    // never leaves a stray `.part` behind.
    let stream = (|| -> Result<String, AnalyzeError> {
        let mut reader = resp.into_body().into_reader();
        let mut out = std::fs::File::create(&tmp)?;
        let mut hasher = Sha256::new();
        let mut buf = [0u8; 1 << 16];
        let mut written: u64 = 0;
        let mut last_emit = Instant::now();
        progress(0, total);
        loop {
            if cancel() {
                return Err(AnalyzeError::Cancelled);
            }
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            std::io::Write::write_all(&mut out, &buf[..n])?;
            written += n as u64;
            // Time-throttle events to ~10/s regardless of link speed.
            if last_emit.elapsed() >= Duration::from_millis(100) {
                progress(written, total.max(written));
                last_emit = Instant::now();
            }
        }
        // Cheap pre-filter: a body shorter than the floor is a truncated / HTML-error response.
        if written < min_size {
            return Err(AnalyzeError::Download(format!(
                "{url}: short body ({written} < {min_size})"
            )));
        }
        progress(written, written);
        Ok(hex_lower(&hasher.finalize()))
    })();
    let digest = match stream {
        Ok(d) => d,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
    };
    // Authoritative integrity gate: a truncated-but-large, redirected, or tampered/substituted body
    // fails here and is removed BEFORE the atomic rename, so a corrupt file never lands on disk (and
    // can never get stuck passing the `min_size`-only `present()` check forever). This rename is the
    // trust boundary; we never re-hash on the routine startup `present()`/`has_all()` path.
    if !digest.eq_ignore_ascii_case(sha256) {
        let _ = std::fs::remove_file(&tmp);
        return Err(AnalyzeError::Integrity(format!(
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

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir() -> PathBuf {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("darkroom_models_{}_{n}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    // A single fake file under a `sam/` subdir. Its URL is an unreachable discard port — the tests
    // below must never actually hit the network (present-skip / early-cancel paths only).
    const FAKE: &[RemoteFile] = &[RemoteFile {
        rel: "sam/test_model.bin",
        url: "http://127.0.0.1:9/never",
        min_size: 8,
        approx_size: 16,
        sha256: "00",
    }];

    #[test]
    fn ensure_with_skips_present_files_without_network() {
        let dir = tmpdir();
        let store = ModelStore::new(dir.clone());
        std::fs::create_dir_all(dir.join("sam")).unwrap();
        std::fs::write(store.path("sam/test_model.bin"), b"0123456789").unwrap();
        let mut done_seen = false;
        let r = store.ensure_with(
            FAKE,
            |p| {
                if p.phase == "done" {
                    done_seen = true;
                }
            },
            &|| false,
        );
        assert!(r.is_ok());
        assert!(done_seen, "final `done` progress tick expected");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ensure_with_cancels_before_any_network() {
        let dir = tmpdir();
        let store = ModelStore::new(dir.clone());
        // File is absent and cancel is already set → must return Cancelled before HEAD/GET.
        let r = store.ensure_with(FAKE, |_| {}, &|| true);
        assert!(matches!(r, Err(AnalyzeError::Cancelled)));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn remove_deletes_files_and_prunes_empty_subdirs() {
        let dir = tmpdir();
        let store = ModelStore::new(dir.clone());
        std::fs::create_dir_all(dir.join("sam")).unwrap();
        std::fs::write(store.path("sam/test_model.bin"), b"0123456789").unwrap();
        store.remove(FAKE).unwrap();
        assert!(!store.path("sam/test_model.bin").exists());
        assert!(
            !dir.join("sam").exists(),
            "emptied sam/ subdir should be pruned"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
