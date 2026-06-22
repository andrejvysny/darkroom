I'll research current denoise approaches across the major RAW processors. Let me dig into how each handles high-noise RAW files.Good detail on Lightroom. Now let me research Darktable's denoise approach.Now let me research DxO DeepPRIME, Topaz, and the classical algorithm foundations (BM3D, etc.).I have strong coverage across the major tools. Let me build a diagram to anchor the summary.Here's how the field breaks down. The single most important decision is **where denoise sits relative to demosaicing** — and that splits every tool into two camps:# RAW denoise — research summary

## 1. The problem has two parts

High-ISO RAW noise is **two separate signals**, and every good denoiser treats them differently:

- **Luminance (luma) noise** → the *grainy* texture. Carries real detail, so smooth it **conservatively** (or leave some as pleasing grain).
- **Chrominance (chroma) noise** → the *colored blotches*. Carries almost no detail, so it can be **crushed aggressively** without visible loss.

Noise is also **signal-dependent**: it follows a **Poisson–Gaussian** model (photon shot noise + sensor read noise). Knowing the per-ISO noise level is what lets a denoiser apply the right strength per brightness zone.

## 2. The classical algorithm families (your building blocks)

| Method | How it works | Best for |
|---|---|---|
| **Wavelet thresholding** | Decompose into multi-scale frequency bands; shrink small coefficients per band | **Chroma** + coarse-grain control |
| **Non-local means (NLM)** | Average a pixel with *similar patches* found anywhere in the image (not just neighbors) | **Luma**; preserves texture. Heavy. |
| **BM3D** | Group similar patches into 3D stacks → collaborative transform-domain filtering → aggregate | Classical **state-of-the-art** quality |
| **Total variation (ROF)** | Minimize total gradient while staying near the input | Edge-preserving; can **"cartoon"** flat areas |
| **Profiled / model-based** | Use a measured per-camera, per-ISO noise profile; often an **Anscombe transform** to stabilize variance first | Knowing *how much* to remove |

## 3. How the major software does it

### Adobe Lightroom / Camera Raw — two systems
- **Manual sliders** (now "Manual Noise Reduction"): classic Luminance + Color sliders, work on any file.
- **"Denoise" (AI, 2023)**: the headline feature. Its models are trained to perform demosaicing and denoising together in a single step, trained on millions of pairs of high-noise and low-noise image patches. Key constraints:
  - **RAW mosaic files only** — it doesn't work with JPEG, TIFF, PSD, or PNG, only with RAW files that haven't been demosaiced.
  - **Apply early** — Adobe recommends applying Denoise early in the workflow, before healing and masking, because downstream AI tools work better on a clean image.
  - **GPU-heavy** — Adobe recommends a GPU with at least 8GB memory; from v14.4 it's **non-destructive** (no more separate `-Enhanced.dng` file).
  - Source: [Denoise Demystified — Adobe blog](https://blog.adobe.com/en/publish/2023/04/18/denoise-demystified)

### Darktable — `denoise (profiled)` module (open source, your closest reference)
- **Profiled**: ships with measured noise profiles per camera/ISO, so the auto mode "just works."
- **Two engines**, both in one module instance: non-local means is better suited to luma noise; wavelet is stronger on chroma noise.
- **Luma/chroma split** via a **Y0U0V0** color mode (separate curves for brightness vs color).
- **Pipeline position matters**: the module is placed before the input color profile module in the pixelpipe so the profile parameters are accurate — i.e. it runs in **linear, scene-referred** space.
- Darktable also has separate **raw denoise** (pre-demosaic) and **astrophoto denoise** modules.
- Source: [darktable manual — denoise (profiled)](https://docs.darktable.org/usermanual/development/en/module-reference/processing-modules/denoise-profiled/)

### DxO PhotoLab / PureRAW — DeepPRIME / DeepPRIME XD (the quality benchmark)
- In 2020 DxO introduced the first commercially available neural network to perform denoising and demosaicing jointly in a single pass.
- Latest **XD3** goes further: a single neural network performs denoising, demosaicing, and chromatic aberration correction simultaneously, on both **Bayer and X-Trans**.
- Real-world gain: roughly two extra stops of ISO detail with DeepPRIME, up to three with XD. Trained on billions of noisy-input / clean-output sample pairs, and **GPU-bound**.
- Why joint? In sequential pipelines each module is unaware of the previous one, so demosaicing errors like moiré get amplified by the noise-reduction stage instead of corrected.
- Source: [DxO DeepPRIME technology](https://www.dxo.com/technology/deepprime/) · [DeepPRIME XD3](https://www.dxo.com/news/deepprime-xd3-fourth-generation/)

### Others (brief)
- **Topaz Photo AI** — AI denoise, often praised as the most *aggressive* detail recovery; runs as plugin/standalone.
- **RawTherapee** — classic, manual: impulse-noise filter + wavelet + chroma/luminance control in Lab space.
- **Capture One** — traditional Luminance + Color noise sliders, plus single-pixel/hot-pixel correction.

## 4. Takeaways for a pixel-pipeline design

Since this maps to a denoise stage in a real engine, the decisions that matter:

1. **Sequential vs joint** — *joint demosaic+denoise* is the quality ceiling, but it **couples two stages** and needs an ML model + GPU. Classical *sequential* is far simpler to ship and debug.
2. **Work in linear, scene-referred space** — like darktable, denoise *before* the input color transform.
3. **Always split luma/chroma** — separate strength controls; crush chroma harder.
4. **Drive strength from a noise model** — a Poisson–Gaussian profile (optionally Anscombe-stabilized) beats a flat global slider.
5. **Order**: denoise **early**, before sharpening and local edits; optionally **add grain back** at the end for a natural look.
6. **Pragmatic hybrid** (common in practice): *wavelet for chroma + NLM/BM3D for luma*, profiled — strong results without training a network.

---

Ready for the next task.


Now act as AI and Image processing expert. Your task is to now do deep research on available denoise AI models or how to train custom one - goal is to implement powerful denoise functionality in Rust into my custom Photo Library/Editing software. 


Ask me additional questions for more context and preferences if needed.


Personal use removes the licensing constraints — so I can point you at the genuinely best models and datasets regardless of commercial terms. Let me verify the current state of the Rust ML tooling and the latest denoise architectures before I write this up.Now the raw-domain denoise models specifically — that's the key category for a RAW editor.Now self-supervised approaches (train without clean ground truth) and the current sRGB-domain SOTA.Let me confirm the current sRGB-domain leaders and their export/deployment story.Personal use changes everything — you can use SIDD, DxO-grade synthesis tricks, and any model weights freely, and you have a huge advantage most people don't: **a single known sensor (Canon EOS R7)**, which makes a custom model genuinely worth it.

Here's the bottom line first, then the detail.

## TL;DR — my recommendation

- **Domain:** Build it as a **linear-RGB denoiser first** (drop-in, post-demosaic, slots into your 14-stage pipeline cleanly), then add a **raw-domain model later** for the quality ceiling. Don't start with raw — it couples to your demosaic stage and complicates everything.
- **Pretrained vs custom:** **Both, phased.** Ship a **pretrained NAFNet** now; then **self-supervise a custom model on your own R7 RAW files** (no clean ground truth needed). On *your* sensor, a fine-tuned model will beat any generic one.
- **Rust runtime:** Train in **PyTorch**, export **ONNX**, run via the **`ort` crate** (ONNX Runtime) with the **CUDA** (NVIDIA) or **CoreML** (Apple) execution provider. Pure-Rust options exist (`candle`, `burn`) if you want to avoid the C++ dependency.## 1. Raw vs RGB domain — the tradeoff

| | **Raw / Bayer domain** | **Linear RGB (post-demosaic)** |
|---|---|---|
| **Quality ceiling** | Highest. Noise is still uncorrelated + signal-dependent | Slightly lower. Demosaic has spread noise spatially |
| **Best version** | **Joint demosaic + denoise** (what Adobe/DxO do) | Standard restoration CNN/transformer |
| **Pipeline fit** | Hard — must sit at/replace your demosaic stage | **Easy — drop-in module** after demosaic |
| **Model supply** | Smaller (PMRID, CycleISP, Unprocessing) | Huge (NAFNet, Restormer, SCUNet, …) |
| **Per-sensor work** | Must handle CFA pattern, black level, white balance | Sensor-agnostic once in RGB |
| **Why it's harder** | in a sequential pipeline each stage is unaware of the previous one, so demosaic errors get amplified by denoising instead of corrected | Real sRGB noise is **spatially correlated** — trips up naive blind-spot methods |

**Verdict:** linear-RGB for v1 is the 80/20. Apply it in **linear, scene-referred space right after demosaic** — the same spot Darktable uses, since it places denoise before the input color profile so noise statistics stay accurate. Move to raw/joint only when you want to chase the last stop of quality.

## 2. Pretrained models worth using

**sRGB / RGB-domain** (feed linear RGB):

| Model | Type | Notes |
|---|---|---|
| **NAFNet** | CNN, activation-free | **My pick.** 40.30 dB PSNR on SIDD, beating prior SOTA with under half the compute. No nonlinear ops → hardware/export-friendly. Cleanest ONNX export. |
| **Restormer** | Transformer | ~40.0 dB on SIDD/DND; heavy (~140 GMACs) and fiddlier to export. |
| **SCUNet** | Swin-Conv U-Net | Strong real-noise model with a built-in degradation/noise synthesis recipe. |
| **Uformer / MPRNet / HINet** | Transformer / multi-stage | Comparable quality; mostly superseded by NAFNet on the efficiency axis. |

**Raw-domain** (feed packed Bayer):

| Model | Type | Notes |
|---|---|---|
| **Unprocessing (UPI)** | Simple CNN | 14–38% lower error and 9–18× faster than the prior SOTA on DND, and generalizes to unseen sensors. Great practical baseline. |
| **CycleISP** | Learned ISP + denoiser | models the camera pipeline forward and reverse, denoises in both RAW and sRGB, SOTA on DND/SIDD with ~5× fewer parameters. |
| **PMRID** | Lightweight CNN | built for mobile; uses a k-sigma transform to remove the ISO dependence of noise plus a sensor-calibration method. Best if you want fast + small. |
| **ELD** | Physics noise model | a physics-based noise formation model for extreme low-light raw denoising. |
| **Retinex-RAWMamba** (2024) | Mamba/SSM | Recent joint demosaic + denoise for low-light RAW — research-grade but state-of-the-art direction. |

> Practical tip: **NAFNet is doubly attractive** — it's both efficient *and*, being purely convolutional with no exotic activations, it exports to ONNX without the dynamic-shape and unsupported-op headaches that plague transformer exports.

## 3. Training your own

Two routes — and self-supervised is the standout for you.

**A. Supervised (needs paired data):**
- **Datasets:** **SIDD** (real noisy/clean smartphone pairs, raw + sRGB), **DND** (50 consumer-camera pairs, benchmark only), **SID/See-in-the-Dark** (extreme low-light raw), **MIT-Adobe FiveK** (clean RAW for synthesis).
- **Synthesis (no real pairs needed):** take clean RGB, run the ISP *backwards* to raw, add **Poisson–Gaussian** noise. This is the **Unprocessing** trick — invert each ISP step in closed form to render large RGB datasets back to the sensor domain for training. **PMRID's k-sigma** transform then makes one model cover all ISOs.

**B. Self-supervised — no clean ground truth at all** (best fit for personal use on one camera):

- **Noise2Noise** — learns from two noisy realizations of the same scene. If you can shoot the same static scene twice on a tripod, this is the simplest and strongest.
- **Neighbor2Neighbor** — subsamples a single noisy image into a noisy–noisy pair, removing the need for noisy/clean pairs; it operates **directly on raw-RGB by sampling within the 2×2 Bayer neighborhood** — i.e. it fits a raw denoiser naturally.
- **Blind2Unblind** — improves on blind-spot training with a visible-blind-spot scheme; usually beats Noise2Void.
- **AP-BSN** — uses pixel-shuffle downsampling to break the spatial correlation of real-world noise; the right choice if you train on already-demosaiced sRGB where noise is correlated.

The workflow: point one of these at a few hundred of your own R7 RAW files, no labelling, and you get a denoiser **tuned to your exact sensor's noise** — something no off-the-shelf model has.

## 4. Pretrained vs custom — verdict

- **Pretrained** wins on time-to-working and proven quality, but is locked to its training noise distribution (which may not match the R7) and transformer exports can be fiddly.
- **Custom** is worth it *specifically because* your scope is narrow: **one sensor + personal use = a small, well-matched, license-free training target.** Self-supervised removes the data-collection burden entirely.
- **Do both:** pretrained NAFNet to validate the integration and pipeline plumbing, then a self-supervised R7-specific model to surpass it.

## 5. Rust inference runtime

| Crate | Pure Rust? | GPU | Training | Use it when |
|---|---|---|---|---|
| **`ort`** (ONNX Runtime) | No (C++ lib) | CUDA, TensorRT, CoreML, DirectML, OpenVINO | No | **Default.** 3–5× faster than Python, mature, best model coverage. |
| **`candle`** (HuggingFace) | Yes | CUDA, Metal, WebGPU | Experimental — inference-first | Pure-Rust inference, loads safetensors directly. |
| **`burn`** | Yes | CUDA, Metal, **wgpu** | **Yes** | You want pure-Rust *training* on your existing wgpu device. |
| **`wonnx`** | Yes | WebGPU only | No | Reuse your exact wgpu device, zero CPU round-trip — but limited op coverage. |
| **`tract`** | Yes | CPU only | No | Pure-Rust CPU fallback. |

**Recommendation:** train in **PyTorch** (every model repo — NAFNet, Neighbor2Neighbor — is PyTorch), export **ONNX**, infer with **`ort`**. If avoiding the C++ dependency matters more than peak speed, **`candle`** is the pure-Rust inference path, and **`burn`** is the one to watch if you ever want to train inside your wgpu stack.

## 6. Rust implementation notes

- **Tile with overlap.** Denoisers are fully convolutional, so process the (large) R7 ~33 MP file in **256–512 px tiles with ~16–32 px overlap**, feather-blended. Keeps VRAM bounded and lets you show progress.
- **Bayer packing (raw models):** pack RGGB into a **4-channel half-resolution** tensor before the network; unpack after.
- **fp16 on GPU** roughly halves latency and memory with negligible quality loss for denoising.
- **Noise conditioning:** models like FFDNet take a noise-level map — estimate σ from **ISO + your sensor's calibrated noise curve** (the PMRID k-sigma idea) so one model spans all ISOs.
- **Bake a fixed tile size into the ONNX export** to dodge dynamic-shape issues in `ort`.
- **GPU interop:** `ort` round-trips tiles through CPU; for a tile-based UI that's fine. Only reach for `wonnx`/`burn` on wgpu if you measure that round-trip as a real bottleneck.

---

### Key sources
- NAFNet — [github.com/megvii-research/NAFNet](https://github.com/megvii-research/NAFNet); paper arXiv:2204.04676
- Unprocessing (Brooks et al., CVPR 2019) — arXiv:1811.11127
- CycleISP (Zamir et al., CVPR 2020) — arXiv:2003.07761
- PMRID (Wang et al., ECCV 2020) — see [Low-Level-Vision-Paper-Record](https://github.com/lcybuzz/Low-Level-Vision-Paper-Record/blob/master/Denoising.md)
- Neighbor2Neighbor (Huang et al., CVPR 2021) — arXiv:2101.02824
- Blind2Unblind (Wang et al., CVPR 2022) — [CVF open access](https://openaccess.thecvf.com/content/CVPR2022/papers/Wang_Blind2Unblind_Self-Supervised_Image_Denoising_With_Visible_Blind_Spots_CVPR_2022_paper.pdf)
- Multi-model restoration framework (NAFNet, Restormer, SCUNet, SwinIR) — [github.com/murufeng/FUIR](https://github.com/murufeng/FUIR)
- Rust ML runtime comparison (`ort` / `candle` / `burn`) — [dev.to guide](https://dev.to/mayu2008/building-sentence-transformers-in-rust-a-practical-guide-with-burn-onnx-runtime-and-candle-281k); candle — [github.com/huggingface/candle](https://github.com/huggingface/candle)

Want me to take any branch deeper — e.g. the concrete `ort`-in-Rust tiling code, the self-supervised training script for your R7, or the raw-domain joint demosaic+denoise architecture?