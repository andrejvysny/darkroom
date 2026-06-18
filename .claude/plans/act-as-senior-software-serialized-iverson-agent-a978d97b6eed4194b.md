# GPU pipeline + WGSL audit — findings (read-only)

Scope: crates/core-pipeline/src/{backend,params,curve,histogram,mask,encode,error,lib}.rs + all \*.wgsl.

## Verified-correct (NOT flagged)

- ParamsUniform wb_gain+exposure pack (known-correct).
- MaskParamsUniform = 48 bytes (vec3 + 9 scalars), matches shader + test masks.rs:92.
- WbUniform mat3 = 3x vec4 columns; col packing matches WGSL mat3x3\*v; identity at (0,0) (test).
- pp_to_srgb rows sum to 1.0 (neutral preserved).
- highlight_rolloff C1-continuous at a=0.75, asymptotes to 1.0, never exceeds.
- curve.rs Fritsch-Carlson monotone clamp + [0,1] clamp; identity LUT i->i.
- WB CAT col packing, kim_xy branch identity.
- readback bpr 256-align + per-row repack correct.

## Findings (real)

1. MED — mask display-stage double-applies global contrast/blacks/whites under masked pixels.
   develop.wgsl 308-316: out starts = apply_local_display(d, P.contrast,...); then per mask
   di = apply_local_display(d, P.contrast+mp.contrast,...) and mix(out,di,a). At a=1 result=di
   (correct, replaces). At 0<a<1 it lerps two display curves — acceptable. NOT a bug after recheck.
   -> downgrade/drop.

2. LOW — w*h*4 / w*h*16 u32 multiply can overflow in release for >100MP (panics via under-alloc,
   not corruption). backend.rs:315,349. Low likelihood on real cameras.

3. INFO — pp_to_srgb derived from XYZ_TO_SRGB_D65 over a D50 ProPhoto buffer with only row-normalize
   (no D50->D65 CAT). Documented approximation; slight white-point error. Not a regression.

4. MED — mask_prepass color_cov sat_w uses hardcoded 0.35 window ignoring `sat` param magnitude
   mismatch; minor selection inaccuracy. Verify.

5. Check: refine passthrough threshold vs caller — OK.

(Final findings delivered via StructuredOutput.)
