// Develop fragment pipeline: cached linear RGB → display-referred sRGB RGBA8.

struct Params {
  wb_gain: vec3<f32>,
  exposure: f32,
  saturation: f32,
  contrast: f32,
  highlights: f32,
  shadows: f32,
  blacks: f32,
  whites: f32,
  _pad0: f32,
  _pad1: f32,
};

@group(0) @binding(0) var input_tex: texture_2d<f32>;
@group(0) @binding(1) var input_smp: sampler;
@group(0) @binding(2) var<uniform> P: Params;
// 256x1 RGBA8 display-space tone-curve LUT (.r/.g/.b hold the per-channel mapped output).
@group(0) @binding(3) var lut_tex: texture_2d<f32>;

// Secondary uniform: per-hue HSL bands as vec4(hue_shift, sat, lum, _pad), each in [-1,1].
struct Fx {
  hsl: array<vec4<f32>, 8>,
};
@group(0) @binding(4) var<uniform> FX: Fx;

// Mask alpha layers (R16Float texture array, MASK_CAP=16 layers). Layer i holds the composited
// coverage of mask i. Sampled with a filtering sampler. All-zero (or count==0) => no masking.
@group(0) @binding(5) var mask_tex: texture_2d_array<f32>;
@group(0) @binding(6) var mask_smp: sampler;

// One mask's scalar deltas (matches Rust `MaskParamsUniform`, std430). `wb_gain` is the
// multiplicative delta gain; the rest are additive deltas on top of the global develop.
struct MaskParams {
  wb_gain: vec3<f32>,
  exposure: f32,
  contrast: f32,
  saturation: f32,
  highlights: f32,
  shadows: f32,
  blacks: f32,
  whites: f32,
  opacity: f32,
  enabled: f32,
};
struct MaskBuffer {
  count: u32,
  _pad0: u32,
  _pad1: u32,
  _pad2: u32,
  masks: array<MaskParams, 16>,
};
@group(0) @binding(7) var<storage, read> M: MaskBuffer;

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
  // clip [-1,1] -> uv [0,1], flip Y (texture row 0 is the top).
  out.uv = vec2<f32>((xy.x + 1.0) * 0.5, 1.0 - (xy.y + 1.0) * 0.5);
  return out;
}

const LUMA = vec3<f32>(0.2126, 0.7152, 0.0722);

fn srgb_encode(c: vec3<f32>) -> vec3<f32> {
  let cut = vec3<f32>(0.0031308);
  let lo = c * 12.92;
  let hi = 1.055 * pow(max(c, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.4)) - 0.055;
  return select(hi, lo, c < cut);
}

fn rgb_to_hsv(c: vec3<f32>) -> vec3<f32> {
  let K = vec4<f32>(0.0, -1.0 / 3.0, 2.0 / 3.0, -1.0);
  let p = mix(vec4<f32>(c.bg, K.wz), vec4<f32>(c.gb, K.xy), step(c.b, c.g));
  let q = mix(vec4<f32>(p.xyw, c.r), vec4<f32>(c.r, p.yzx), step(p.x, c.r));
  let d = q.x - min(q.w, q.y);
  let e = 1.0e-10;
  return vec3<f32>(abs(q.z + (q.w - q.y) / (6.0 * d + e)), d / (q.x + e), q.x);
}

fn hsv_to_rgb(c: vec3<f32>) -> vec3<f32> {
  let K = vec4<f32>(1.0, 2.0 / 3.0, 1.0 / 3.0, 3.0);
  let p = abs(fract(vec3<f32>(c.x) + K.xyz) * 6.0 - vec3<f32>(K.w));
  return c.z * mix(vec3<f32>(K.x), clamp(p - vec3<f32>(K.x), vec3<f32>(0.0), vec3<f32>(1.0)), c.y);
}

// Smallest absolute angular distance (degrees) between two hues.
fn ang_dist(a: f32, b: f32) -> f32 {
  var d = abs(a - b);
  d = d - floor(d / 360.0) * 360.0;
  return min(d, 360.0 - d);
}

// Per-hue HSL mixer. Each pixel's hue is weighted across the 8 bands (overlapping windows,
// normalized to a partition of unity) so a uniform adjustment behaves like a global one.
fn apply_hsl(rgb_in: vec3<f32>) -> vec3<f32> {
  var centers = array<f32, 8>(0.0, 30.0, 60.0, 120.0, 180.0, 240.0, 290.0, 330.0);
  let hsv = rgb_to_hsv(clamp(rgb_in, vec3<f32>(0.0), vec3<f32>(1.0)));
  let hue_deg = hsv.x * 360.0;

  var wsum = 0.0;
  var dh = 0.0;
  var ds = 0.0;
  var dl = 0.0;
  for (var i = 0; i < 8; i = i + 1) {
    let w = max(0.0, 1.0 - ang_dist(hue_deg, centers[i]) / 60.0);
    wsum = wsum + w;
    dh = dh + w * FX.hsl[i].x;
    ds = ds + w * FX.hsl[i].y;
    dl = dl + w * FX.hsl[i].z;
  }
  if (wsum > 0.0) {
    dh = dh / wsum;
    ds = ds / wsum;
    dl = dl / wsum;
  }

  var h = hsv.x + (dh * 30.0) / 360.0; // up to ±30° hue rotation
  h = fract(h + 1.0);
  let s = clamp(hsv.y * (1.0 + ds), 0.0, 1.0);
  let v = clamp(hsv.z * (1.0 + dl), 0.0, 1.0);
  return hsv_to_rgb(vec3<f32>(h, s, v));
}

// Sample one display-space channel through the tone-curve LUT with linear interpolation.
// `sel` is a unit basis vector picking the LUT component (R/G/B).
fn curve_ch(x: f32, sel: vec3<f32>) -> f32 {
  let t = clamp(x, 0.0, 1.0) * 255.0;
  let i0 = i32(floor(t));
  let i1 = min(i0 + 1, 255);
  let f = t - floor(t);
  let v0 = textureLoad(lut_tex, vec2<i32>(i0, 0), 0).rgb;
  let v1 = textureLoad(lut_tex, vec2<i32>(i1, 0), 0).rgb;
  return dot(mix(v0, v1, f), sel);
}

// Scene-linear local adjustments (WB, exposure, highlights/shadows, saturation), parameterized so
// the base develop and every per-mask variant share identical math. Masking happens in linear light.
fn apply_local_linear(
  rgb_in: vec3<f32>,
  wb_gain: vec3<f32>,
  exposure: f32,
  highlights: f32,
  shadows: f32,
  saturation: f32,
) -> vec3<f32> {
  var rgb = rgb_in * wb_gain;          // 1. white balance
  rgb = rgb * exp2(exposure);          // 2. exposure
  rgb = max(rgb, vec3<f32>(0.0));

  // 3. highlights / shadows (luminance-masked, linear)
  let luma = dot(rgb, LUMA);
  let shadow_mask = exp(-luma * 4.0);
  let highlight_mask = 1.0 - exp(-max(luma - 0.5, 0.0) * 4.0);
  rgb = rgb * (1.0 + 0.6 * shadows * shadow_mask);
  rgb = rgb * (1.0 + 0.6 * highlights * highlight_mask);
  rgb = max(rgb, vec3<f32>(0.0));

  // 4. saturation (linear)
  let l2 = dot(rgb, LUMA);
  return max(mix(vec3<f32>(l2), rgb, 1.0 + saturation), vec3<f32>(0.0));
}

// Display-space local adjustments (contrast, blacks, whites), applied after the sRGB transform.
fn apply_local_display(d_in: vec3<f32>, contrast: f32, blacks: f32, whites: f32) -> vec3<f32> {
  var d = (d_in - vec3<f32>(0.5)) * (1.0 + contrast) + vec3<f32>(0.5); // contrast, pivot mid-gray
  d = d + vec3<f32>(blacks * 0.2);
  d = d * (1.0 + whites * 0.2);
  return d;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
  let base_rgb = textureSampleLevel(input_tex, input_smp, in.uv, 0.0).rgb;

  // --- SCENE-LINEAR STAGE: base develop, then composite each mask's linear deltas ---
  var lin = apply_local_linear(
    base_rgb, P.wb_gain, P.exposure, P.highlights, P.shadows, P.saturation);
  for (var i = 0u; i < M.count; i = i + 1u) {
    let mp = M.masks[i];
    let a = textureSampleLevel(mask_tex, mask_smp, in.uv, i32(i), 0.0).r * mp.opacity;
    if (a < 1e-4) { continue; }
    let li = apply_local_linear(
      base_rgb,
      P.wb_gain * mp.wb_gain,
      P.exposure + mp.exposure,
      P.highlights + mp.highlights,
      P.shadows + mp.shadows,
      P.saturation + mp.saturation,
    );
    lin = mix(lin, li, a);
  }

  // --- DISPLAY TRANSITION (shared, once): sRGB OETF, HSL mixer, tone curve (all global) ---
  var d = srgb_encode(clamp(lin, vec3<f32>(0.0), vec3<f32>(1.0)));
  d = apply_hsl(d);
  d = vec3<f32>(
    curve_ch(d.r, vec3<f32>(1.0, 0.0, 0.0)),
    curve_ch(d.g, vec3<f32>(0.0, 1.0, 0.0)),
    curve_ch(d.b, vec3<f32>(0.0, 0.0, 1.0)),
  );

  // --- DISPLAY STAGE: base develop, then composite each mask's display deltas ---
  var out = apply_local_display(d, P.contrast, P.blacks, P.whites);
  for (var i = 0u; i < M.count; i = i + 1u) {
    let mp = M.masks[i];
    let a = textureSampleLevel(mask_tex, mask_smp, in.uv, i32(i), 0.0).r * mp.opacity;
    if (a < 1e-4) { continue; }
    let di = apply_local_display(
      d, P.contrast + mp.contrast, P.blacks + mp.blacks, P.whites + mp.whites);
    out = mix(out, di, a);
  }

  return vec4<f32>(clamp(out, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
