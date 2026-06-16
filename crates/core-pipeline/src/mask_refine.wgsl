// Edge-aware mask refinement: a separable cross-bilateral blur of the mask alpha guided by the
// image's display luminance. Run as two passes (horizontal then vertical). Smooths the mask while
// snapping its transitions to image edges (so brush/range masks feather along real boundaries).
// sigma_px == 0 ⇒ passthrough (used as a plain copy when a mask isn't feathered).
//
// This is the bilateral/joint-filter form of edge-aware feathering (cf. guided filter, He et al.);
// taps are bounded (17 per pass) and spaced by sigma so feather scale is resolution-independent.

struct RefineU {
  dir: vec2<f32>,      // (1,0) horizontal | (0,1) vertical
  sigma_px: f32,       // spatial sigma in pixels (0 = passthrough)
  luma_sigma: f32,     // range sigma on display luma
  texel: vec2<f32>,    // (1/w, 1/h)
  _pad: vec2<f32>,
};

@group(0) @binding(0) var src_tex: texture_2d<f32>;
@group(0) @binding(1) var src_smp: sampler;
@group(0) @binding(2) var input_tex: texture_2d<f32>;
@group(0) @binding(3) var<uniform> U: RefineU;

const LUMA = vec3<f32>(0.2126, 0.7152, 0.0722);

struct VsOut {
  @builtin(position) pos: vec4<f32>,
  @location(0) uv: vec2<f32>,
};

@vertex
fn vs(@builtin(vertex_index) vid: u32) -> VsOut {
  var verts = array<vec2<f32>, 3>(
    vec2<f32>(-1.0, -1.0),
    vec2<f32>( 3.0, -1.0),
    vec2<f32>(-1.0,  3.0),
  );
  let xy = verts[vid];
  var out: VsOut;
  out.pos = vec4<f32>(xy, 0.0, 1.0);
  out.uv = vec2<f32>((xy.x + 1.0) * 0.5, 1.0 - (xy.y + 1.0) * 0.5);
  return out;
}

fn srgb_encode(c: vec3<f32>) -> vec3<f32> {
  let cut = vec3<f32>(0.0031308);
  let lo = c * 12.92;
  let hi = 1.055 * pow(max(c, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.4)) - 0.055;
  return select(hi, lo, c < cut);
}

fn luma_at(uv: vec2<f32>) -> f32 {
  let dims = vec2<f32>(textureDimensions(input_tex));
  let px = vec2<i32>(clamp(uv, vec2<f32>(0.0), vec2<f32>(1.0)) * (dims - 1.0));
  let lin = textureLoad(input_tex, px, 0).rgb;
  return dot(srgb_encode(clamp(lin, vec3<f32>(0.0), vec3<f32>(1.0))), LUMA);
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
  let center = textureSampleLevel(src_tex, src_smp, in.uv, 0.0).r;
  if (U.sigma_px < 0.25) {
    return vec4<f32>(center, 0.0, 0.0, 1.0); // passthrough copy
  }
  let cl = luma_at(in.uv);
  let spacing = max(U.sigma_px * 0.25, 0.5); // px between taps; ±8 taps ⇒ ±2σ
  let s2 = 2.0 * U.sigma_px * U.sigma_px;
  let r2 = 2.0 * max(U.luma_sigma * U.luma_sigma, 1e-5);

  var sum = 0.0;
  var wsum = 0.0;
  for (var i = -8; i <= 8; i = i + 1) {
    let off = U.dir * (f32(i) * spacing) * U.texel;
    let uv = in.uv + off;
    let a = textureSampleLevel(src_tex, src_smp, uv, 0.0).r;
    let l = luma_at(uv);
    let dpx = f32(i) * spacing;
    let ws = exp(-(dpx * dpx) / s2);
    let dl = l - cl;
    let wr = exp(-(dl * dl) / r2);
    let w = ws * wr;
    sum = sum + a * w;
    wsum = wsum + w;
  }
  let out = select(center, sum / wsum, wsum > 1e-6);
  return vec4<f32>(out, 0.0, 0.0, 1.0);
}
