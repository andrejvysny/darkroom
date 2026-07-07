#!/usr/bin/env python3
"""Export the pretrained PMRID raw-Bayer denoiser to a fixed-shape ONNX for Darkroom.

PMRID (Megvii, ECCV'20, Apache-2.0) is a ~1M-param 4-channel RGGB-in / 4-channel RGGB-out
denoise-only residual U-Net. We bake a STATIC [1,4,256,256] input (no dynamic axes) so the ONNX
Runtime CoreML EP accelerates it fully, then embed the ~4 MB file via include_bytes! in core-analyze.
k-Sigma (ISO conditioning) is applied in Rust, OUTSIDE the graph, so the graph stays a pure conv net.

Run (no venv management needed — uv installs ephemeral deps):

    uv run --python 3.11 \
        --with torch --with onnx --with onnxruntime --with numpy \
        scripts/export_pmrid.py

Output: crates/core-analyze/assets/pmrid_4x256x256.onnx
"""
from __future__ import annotations

import pickle
import subprocess
import sys
from pathlib import Path

import numpy as np
import torch

REPO = "https://github.com/megvii-research/PMRID.git"
TILE = 256
OPSET = 17

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "crates" / "core-analyze" / "assets" / "pmrid_4x256x256.onnx"
CACHE = ROOT / "target" / "pmrid_repo"  # throwaway clone, gitignored under target/


def ensure_repo() -> Path:
    if not (CACHE / "models" / "net_torch.py").exists():
        CACHE.parent.mkdir(parents=True, exist_ok=True)
        print(f"cloning {REPO} → {CACHE}")
        subprocess.run(
            ["git", "clone", "--depth", "1", REPO, str(CACHE)], check=True
        )
    return CACHE


def load_net(repo: Path) -> torch.nn.Module:
    sys.path.insert(0, str(repo))
    from models.net_torch import Network  # type: ignore

    net = Network()
    ckpt = repo / "models" / "torch_pretrained.ckp"
    with ckpt.open("rb") as f:
        try:
            states = pickle.load(f)
        except Exception:
            f.seek(0)
            states = torch.load(f, map_location="cpu")
    if isinstance(states, dict) and "state_dict" in states:
        states = states["state_dict"]
    net.load_state_dict(states)
    net.eval()
    n_params = sum(p.numel() for p in net.parameters())
    print(f"loaded PMRID: {n_params/1e6:.2f}M params")
    return net


def main() -> int:
    repo = ensure_repo()
    net = load_net(repo)

    dummy = torch.zeros(1, 4, TILE, TILE, dtype=torch.float32)
    OUT.parent.mkdir(parents=True, exist_ok=True)
    torch.onnx.export(
        net,
        dummy,
        str(OUT),
        input_names=["mosaic"],
        output_names=["denoised"],
        opset_version=OPSET,
        dynamic_axes=None,          # STATIC shape — required for the CoreML EP
        do_constant_folding=True,
        dynamo=False,               # legacy TorchScript exporter (no onnxscript dep; best EP compat)
    )
    print(f"wrote {OUT}  ({OUT.stat().st_size/1e6:.2f} MB)")

    # Parity: PyTorch vs onnxruntime(CPU) on a realistic k-Sigma-domain tile.
    import onnxruntime as ort

    rng = np.random.default_rng(0)
    x = rng.uniform(0.0, 4.0, size=(1, 4, TILE, TILE)).astype(np.float32)  # ~inp_scale domain
    with torch.no_grad():
        y_torch = net(torch.from_numpy(x)).numpy()
    sess = ort.InferenceSession(str(OUT), providers=["CPUExecutionProvider"])
    y_ort = sess.run(["denoised"], {"mosaic": x})[0]
    max_abs = float(np.max(np.abs(y_torch - y_ort)))
    print(f"torch↔ort max-abs diff: {max_abs:.3e}")
    if max_abs > 1e-3:
        print("WARNING: parity worse than 1e-3 — inspect the export", file=sys.stderr)
        return 1
    print("OK: PMRID exported and parity-validated.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
