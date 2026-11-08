// GPU mip-chain builder: one dispatch per level, mirrors `downsample_rgba_f32` (backend.rs).

struct Dims {
    src: vec2<u32>,
    dst: vec2<u32>,
}

@group(0) @binding(0) var src_tex: texture_2d<f32>;
@group(0) @binding(1) var dst_tex: texture_storage_2d<rgba32float, write>;
@group(0) @binding(2) var<uniform> dims: Dims;

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= dims.dst.x || gid.y >= dims.dst.y) {
        return;
    }
    let sw = dims.src.x;
    let sh = dims.src.y;
    let nw = dims.dst.x;
    let nh = dims.dst.y;
    let exact = (sw == 1u || sw % 2u == 0u) && (sh == 1u || sh % 2u == 0u);
    var out = vec4<f32>(0.0);
    if (exact) {
        // 2x2 box with clamped indices (duplicated sample on a 1-px axis). The CPU computes
        // 0.25*(a+b+c+d) and only falls back to scaling before summing (in f64) when the f32 sum
        // overflows. Scale-first never overflows and avoids an isfinite test (which fast-math
        // shader compilers may fold away); it rounds in a different order than the CPU sum, so
        // results agree to ~1 ulp, not bit-for-bit (tests/gpu_mips.rs bounds it at 1e-6 rel).
        let x0 = min(gid.x * 2u, sw - 1u);
        let x1 = min(gid.x * 2u + 1u, sw - 1u);
        let y0 = min(gid.y * 2u, sh - 1u);
        let y1 = min(gid.y * 2u + 1u, sh - 1u);
        let a = textureLoad(src_tex, vec2<u32>(x0, y0), 0);
        let b = textureLoad(src_tex, vec2<u32>(x1, y0), 0);
        let c = textureLoad(src_tex, vec2<u32>(x0, y1), 0);
        let d = textureLoad(src_tex, vec2<u32>(x1, y1), 0);
        // fma chain keeps the scale-before-sum order against compiler reassociation.
        out = fma(a, vec4<f32>(0.25), fma(b, vec4<f32>(0.25), fma(c, vec4<f32>(0.25), d * 0.25)));
    } else {
        // Integer-overlap footprint (<=3x3 texels, total weight sw*sh), as on the CPU. The CPU
        // accumulates in f64; here weights are pre-normalised (w/(sw*sh)) and summed in f32, so
        // large HDR values cannot overflow and the error stays ~1e-7 relative.
        let left = gid.x * sw;
        let right = (gid.x + 1u) * sw;
        let top = gid.y * sh;
        let bottom = (gid.y + 1u) * sh;
        let inv_total = 1.0 / f32(sw * sh);
        for (var sy = top / nh; sy < (bottom + nh - 1u) / nh; sy++) {
            let wy = min(bottom, (sy + 1u) * nh) - max(top, sy * nh);
            for (var sx = left / nw; sx < (right + nw - 1u) / nw; sx++) {
                let wx = min(right, (sx + 1u) * nw) - max(left, sx * nw);
                let weight = f32(wx * wy) * inv_total;
                out += textureLoad(src_tex, vec2<u32>(sx, sy), 0) * weight;
            }
        }
    }
    textureStore(dst_tex, vec2<u32>(gid.x, gid.y), out);
}
