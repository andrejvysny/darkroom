// Develop fragment pipeline: cached linear **ProPhoto** RGB → display-referred sRGB RGBA8.
// Scene-linear edits (WB/exposure/highlights/shadows/saturation + masks) run in wide-gamut ProPhoto;
// the scene-referred base tone operator (ACR-matched) maps scene-linear→display-linear in ProPhoto,
// then the display transition converts ProPhoto→sRGB and applies the sRGB OETF.

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

// Global white-balance chromatic-adaptation matrix (std140 mat3 = 3 vec4 columns). Built on the CPU
// from temp/tint (Planckian-locus target white + Bradford CAT); identity at temp=tint=0.
struct Wb {
  c0: vec4<f32>,
  c1: vec4<f32>,
  c2: vec4<f32>,
};
@group(0) @binding(8) var<uniform> WB: Wb;

fn wb_apply(v: vec3<f32>) -> vec3<f32> {
  return mat3x3<f32>(WB.c0.xyz, WB.c1.xyz, WB.c2.xyz) * v;
}

// Detail (sharpen / luma+color NR) + lens vignette + image texel size. @binding(9).
struct Extra {
  detail: vec4<f32>, // sharpen (0..1.5), nr_luma (0..1), nr_color (0..1), vignette (-1..1)
  texel: vec4<f32>,  // 1/width, 1/height, _, _
};
@group(0) @binding(9) var<uniform> EX: Extra;

// Scene-referred base tone operator. `params` = (u_min, u_max, hue_protect, _pad): the log-exposure
// domain the LUT spans + the hue-protection strength. The curve itself rides `base_lut` (R32Float
// Nx1), mapping scene-linear ProPhoto [0,∞) → display-linear [0,1) — applied BEFORE pp_to_srgb.
struct ToneOp {
  params: vec4<f32>,
};
@group(0) @binding(10) var<uniform> TO: ToneOp;
@group(0) @binding(11) var base_lut: texture_2d<f32>;

// Crop + straighten geometry. crop=(cx,cy,hw,hh); rot=(cos θ, sin θ, autozoom, active);
// aspect=(src W/H, out W/H, _, _). Inverse-maps output uv → source uv; mirrors params.rs geom_src_uv.
struct Geom {
  crop: vec4<f32>,
  rot: vec4<f32>,
  aspect: vec4<f32>,
};
@group(0) @binding(12) var<uniform> GEO: Geom;

// Viewport + mask overlay. rect=(origin.xy, size.xy) visible window in CROP-LOCAL uv [0,1];
// flags=(active 0/1, overlay_layer (-1=off), overlay_strength, _); color=(rgb, _) overlay tint.
// active=0 ⇒ legacy geom_resolve path + dead overlay ⇒ byte-identical to a pre-binding render.
struct View {
  rect: vec4<f32>,
  flags: vec4<f32>,
  color: vec4<f32>,
};
@group(0) @binding(13) var<uniform> VIEW: View;

// Color-balance-RGB grading (@binding(14)). fwd/inv = ProPhoto(D50)⇄grading-RGB(D65) mat3 (3 vec4
// columns each, packed CPU-side from the verified chain). global/shadows/midtones/highlights = per-
// channel grading-RGB vectors in .xyz. params = (contrast, saturation, active 0/1, _). active=0 ⇒
// the whole stage is skipped (byte-identical to a pre-binding-14 render).
struct CbRgb {
  fwd0: vec4<f32>, fwd1: vec4<f32>, fwd2: vec4<f32>,
  inv0: vec4<f32>, inv1: vec4<f32>, inv2: vec4<f32>,
  global: vec4<f32>,
  shadows: vec4<f32>,
  midtones: vec4<f32>,
  highlights: vec4<f32>,
  params: vec4<f32>,
};
@group(0) @binding(14) var<uniform> CB: CbRgb;

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

// Linear ProPhoto (working space) → linear sRGB (display primaries). Row-normalized so neutral maps
// to neutral; derived in crates/core-raw/examples/print_color_matrices.rs. Out-of-sRGB colors land
// <0 or >1 and are gamut-clipped downstream (the rolloff's max(.,0) + the OETF's clamp).
fn pp_to_srgb(c: vec3<f32>) -> vec3<f32> {
  return vec3<f32>(
    dot(vec3<f32>( 1.8215216, -0.5579748, -0.2635469), c),
    dot(vec3<f32>(-0.2385862,  1.2344216,  0.0041646), c),
    dot(vec3<f32>(-0.0199185, -0.1907297,  1.2106482), c),
  );
}

// Base tone-operator LUT length (matches `BASE_LUT_SIZE` in params.rs).
const BASE_LUT_N: i32 = 512;

// Sample the base tone operator f(x) for one scalar channel via the log-exposure LUT.
// x in scene-linear ProPhoto; returns display-linear in [0,1). f(0)=0; out-of-domain extremes clamp
// to the LUT ends (deep shadows ≈ 0, far highlights ≈ the asymptote).
fn base_curve_lookup(x: f32) -> f32 {
  if (x <= 0.0) { return 0.0; }
  let u = log2(x / 0.18);
  let t = clamp((u - TO.params.x) / (TO.params.y - TO.params.x), 0.0, 1.0) * f32(BASE_LUT_N - 1);
  let i0 = i32(floor(t));
  let i1 = min(i0 + 1, BASE_LUT_N - 1);
  let f = t - floor(t);
  let v0 = textureLoad(base_lut, vec2<i32>(i0, 0), 0).r;
  let v1 = textureLoad(base_lut, vec2<i32>(i1, 0), 0).r;
  return mix(v0, v1, f);
}

// Apply the scene-referred base tone operator in ProPhoto (Codex: per-channel on Rec.709 is unsafe).
// Default is the per-channel curve (photographic highlight desaturation); for SATURATED HIGHLIGHTS it
// blends toward a hue-preserving luminance-ratio variant (weight rises with chroma × highlight
// excursion, gated by TO.params.z) to tame neon hue twists. Midtones/low-sat stay per-channel.
fn apply_base_tone(c: vec3<f32>) -> vec3<f32> {
  // Display-referred bypass (JPEG/PNG): the image is already tone-mapped, so skip the scene-referred
  // operator and pass it through (clamped). pp_to_srgb + OETF then reproduce the source exactly.
  if (TO.params.w > 0.5) { return clamp(c, vec3<f32>(0.0), vec3<f32>(1.0)); }
  let rgb = max(c, vec3<f32>(0.0));
  let pc = vec3<f32>(base_curve_lookup(rgb.r), base_curve_lookup(rgb.g), base_curve_lookup(rgb.b));
  let y = dot(rgb, LUMA);
  let ratio = select(vec3<f32>(0.0), rgb * (base_curve_lookup(y) / max(y, 1e-6)), y > 1e-6);
  let mx = max(rgb.r, max(rgb.g, rgb.b));
  let mn = min(rgb.r, min(rgb.g, rgb.b));
  let chroma = (mx - mn) / max(mx, 1e-6);
  let w = clamp(TO.params.z * chroma * smoothstep(0.5, 2.0, mx), 0.0, 1.0);
  return mix(pc, ratio, w);
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

// Exact texel fetch at uv + pixel offset (input is non-filterable Rgba32Float — textureLoad only).
fn load_px(uv: vec2<f32>, off: vec2<f32>) -> vec3<f32> {
  let dims = vec2<f32>(textureDimensions(input_tex));
  let p = clamp((uv + off) * dims, vec2<f32>(0.0), dims - vec2<f32>(1.0));
  return textureLoad(input_tex, vec2<i32>(p), 0).rgb;
}

// 3×3 Gaussian blur (1-2-1) of the input at uv — shared by NR and unsharp sharpening.
fn blur3(uv: vec2<f32>) -> vec3<f32> {
  let t = EX.texel.xy;
  let s =
      load_px(uv, vec2<f32>(-t.x, -t.y)) + load_px(uv, vec2<f32>(0.0, -t.y)) * 2.0 + load_px(uv, vec2<f32>(t.x, -t.y))
    + load_px(uv, vec2<f32>(-t.x, 0.0)) * 2.0 + load_px(uv, vec2<f32>(0.0, 0.0)) * 4.0 + load_px(uv, vec2<f32>(t.x, 0.0)) * 2.0
    + load_px(uv, vec2<f32>(-t.x, t.y)) + load_px(uv, vec2<f32>(0.0, t.y)) * 2.0 + load_px(uv, vec2<f32>(t.x, t.y));
  return s / 16.0;
}

// Detail stage: luma/color noise reduction (blend toward the blurred neighborhood, independently per
// luminance + chroma) then unsharp-mask sharpening. Operates on the linear input before WB/develop.
fn apply_detail(uv: vec2<f32>, base: vec3<f32>) -> vec3<f32> {
  let sharpen = EX.detail.x;
  let nr_l = EX.detail.y;
  let nr_c = EX.detail.z;
  if (sharpen < 1e-4 && nr_l < 1e-4 && nr_c < 1e-4) { return base; }
  let b = blur3(uv);
  var rgb = base;
  if (nr_l > 1e-4 || nr_c > 1e-4) {
    let yl = dot(rgb, LUMA);
    let yb = dot(b, LUMA);
    let y = mix(yl, yb, nr_l);                              // luminance NR
    let c = mix(rgb - vec3<f32>(yl), b - vec3<f32>(yb), nr_c); // chroma (color) NR
    rgb = vec3<f32>(y) + c;
  }
  if (sharpen > 1e-4) {
    rgb = rgb + (rgb - b) * sharpen;                        // unsharp mask
  }
  return max(rgb, vec3<f32>(0.0));
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
  // Engage highlights from upper-midtones up (linear 0.25 ≈ sRGB 0.54), not just bright highlights,
  // so the slider reaches more of the tonal range (closer to Lightroom's Highlights behavior).
  let highlight_mask = 1.0 - exp(-max(luma - 0.25, 0.0) * 4.0);
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
  // Endpoint pivots so the two don't fight: whites scales about the black point (0 fixed); blacks
  // pivots about the white point (1 fixed). Lifting blacks no longer also lifts the whites.
  d = d * (1.0 + whites * 0.2);
  d = vec3<f32>(1.0) - (vec3<f32>(1.0) - d) * (1.0 - blacks * 0.2);
  return d;
}

// Letterbox surround for preview pixels outside the cropped frame (export never letterboxes).
const LETTERBOX = vec3<f32>(0.0, 0.0, 0.0);

// 4-tap bilinear fetch of the linear input at an arbitrary uv (texel-center convention). Needed by
// the geometry remap since the input texture is non-filterable (textureLoad only). The `-0.5` is
// essential — without it the identity render shifts half a texel.
fn sample_bilinear(uv: vec2<f32>) -> vec3<f32> {
  let dims = vec2<f32>(textureDimensions(input_tex));
  let p = uv * dims - vec2<f32>(0.5);
  let i0 = floor(p);
  let f = p - i0;
  let lo = vec2<i32>(0, 0);
  let hi = vec2<i32>(dims) - vec2<i32>(1, 1);
  let c0 = clamp(vec2<i32>(i0), lo, hi);
  let c1 = clamp(vec2<i32>(i0) + vec2<i32>(1, 1), lo, hi);
  let t00 = textureLoad(input_tex, vec2<i32>(c0.x, c0.y), 0).rgb;
  let t10 = textureLoad(input_tex, vec2<i32>(c1.x, c0.y), 0).rgb;
  let t01 = textureLoad(input_tex, vec2<i32>(c0.x, c1.y), 0).rgb;
  let t11 = textureLoad(input_tex, vec2<i32>(c1.x, c1.y), 0).rgb;
  return mix(mix(t00, t10, f.x), mix(t01, t11, f.x), f.y);
}

struct GeomResult {
  u: vec2<f32>,       // crop-local output uv [0,1] (vignette/letterbox space)
  src_uv: vec2<f32>,  // source sampling uv
  inside: bool,       // false ⇒ this output pixel is letterbox (outside the cropped frame)
};

// Map a fullscreen output uv → (crop-local u, source uv). Letterbox-fits the crop into the output
// frame (preserve aspect) for preview; for export out_aspect == crop aspect so it fills. Rotation is
// in pixel space (rotating normalized uv would shear a non-square image).
fn geom_resolve(out_uv: vec2<f32>) -> GeomResult {
  var r: GeomResult;
  if (GEO.rot.w < 0.5) {           // identity fast-path: no crop, no rotation
    r.u = out_uv;
    r.src_uv = out_uv;
    r.inside = true;
    return r;
  }
  let src_aspect = GEO.aspect.x;
  let out_aspect = GEO.aspect.y;
  let hw = GEO.crop.z;
  let hh = GEO.crop.w;
  let ac = (hw / hh) * src_aspect; // crop pixel aspect
  var content = vec2<f32>(1.0, 1.0);
  if (out_aspect > ac) {
    content = vec2<f32>(ac / out_aspect, 1.0);
  } else {
    content = vec2<f32>(1.0, out_aspect / ac);
  }
  let cmin = (vec2<f32>(1.0) - content) * 0.5;
  let u = (out_uv - cmin) / content;
  r.u = u;
  if (u.x < 0.0 || u.x > 1.0 || u.y < 0.0 || u.y > 1.0) {
    r.src_uv = vec2<f32>(0.0);
    r.inside = false;
    return r;
  }
  let ct = GEO.rot.x;
  let st = GEO.rot.y;
  let z = GEO.rot.z;
  let d = vec2<f32>((2.0 * u.x - 1.0) * hw, (2.0 * u.y - 1.0) * hh);
  let dpx = vec2<f32>(d.x * src_aspect, d.y);
  let rot = vec2<f32>(ct * dpx.x + st * dpx.y, -st * dpx.x + ct * dpx.y); // inverse rotation R(-θ)
  let disp = vec2<f32>(rot.x / src_aspect / z, rot.y / z);
  r.src_uv = vec2<f32>(GEO.crop.x, GEO.crop.y) + disp;
  r.inside = true;
  return r;
}

// Viewport path: map a CROP-LOCAL uv ([0,1] = the whole cropped frame) directly to source uv. The
// view rect already supplies the visible window + aspect, so the letterbox-fit of `geom_resolve` is
// skipped (it would double-apply aspect — Codex). `out_aspect` plays no role here.
fn crop_to_source(cuv: vec2<f32>) -> GeomResult {
  var r: GeomResult;
  r.u = cuv;
  if (cuv.x < 0.0 || cuv.x > 1.0 || cuv.y < 0.0 || cuv.y > 1.0) {
    r.src_uv = vec2<f32>(0.0);
    r.inside = false;
    return r;
  }
  if (GEO.rot.w < 0.5) {            // no crop / rotation ⇒ crop-local uv IS source uv
    r.src_uv = cuv;
    r.inside = true;
    return r;
  }
  let src_aspect = GEO.aspect.x;
  let hw = GEO.crop.z;
  let hh = GEO.crop.w;
  let ct = GEO.rot.x;
  let st = GEO.rot.y;
  let z = GEO.rot.z;
  let d = vec2<f32>((2.0 * cuv.x - 1.0) * hw, (2.0 * cuv.y - 1.0) * hh);
  let dpx = vec2<f32>(d.x * src_aspect, d.y);
  let rot = vec2<f32>(ct * dpx.x + st * dpx.y, -st * dpx.x + ct * dpx.y); // inverse rotation R(-θ)
  let disp = vec2<f32>(rot.x / src_aspect / z, rot.y / z);
  r.src_uv = vec2<f32>(GEO.crop.x, GEO.crop.y) + disp;
  r.inside = true;
  return r;
}

// --- Color-balance-RGB grading (faithful subset of darktable colorbalancergb) --------------------
// Runs in the scene-linear stage (after develop + masks, before the base tone operator). All work is
// in the D65 grading RGB; the verified ProPhoto⇄grading matrices ride the uniform.
const CB_YW = vec3<f32>(0.78777, 0.250455, 0.0); // grading-RGB luminance weights (= [.6899,.3483,0]·Q)
const CB_MASK_FULCRUM = 0.5;   // pow(neutral grading-Y ≈0.19, 0.41012) ≈ 0.5 (mid-grey → 0.5)
const CB_GREY = 0.1845;        // grading-luminance fulcrum for scene-linear contrast
const CB_SW = 4.0;             // shadows / highlights / midtones opacity weights (darktable defaults)
const CB_HW = 4.0;
const CB_MW = 8.0;

fn cb_fwd(v: vec3<f32>) -> vec3<f32> {
  return mat3x3<f32>(CB.fwd0.xyz, CB.fwd1.xyz, CB.fwd2.xyz) * v;
}
fn cb_inv(v: vec3<f32>) -> vec3<f32> {
  return mat3x3<f32>(CB.inv0.xyz, CB.inv1.xyz, CB.inv2.xyz) * v;
}

fn apply_color_balance(lin: vec3<f32>) -> vec3<f32> {
  if (CB.params.z < 0.5) { return lin; }  // inactive ⇒ identity (skips the round trip; byte-exact)
  var g = cb_fwd(lin);

  // Tonal opacity masks (darktable `opacity_masks`): center mid-grey at the fulcrum in a perceptual x.
  let yv = max(dot(g, CB_YW), 0.0);
  let x = pow(yv, 0.4101205819200422);
  let xo = (x - CB_MASK_FULCRUM) / CB_MASK_FULCRUM;
  let alpha = 1.0 / (1.0 + exp(xo * CB_SW));        // shadows opacity
  let beta = 1.0 / (1.0 + exp(-xo * CB_HW));        // highlights opacity
  let ac = 1.0 - alpha;
  let bc = 1.0 - beta;
  let dx = x - CB_MASK_FULCRUM;
  let gm = clamp(exp(-dx * dx * CB_MW / 4.0) * ac * ac * bc * bc * 8.0, 0.0, 1.0); // midtones opacity

  // 4-way grade.
  g = g + CB.global.xyz;                                  // global offset (all tones)
  g = g + CB.shadows.xyz * alpha;                         // shadows lift
  g = g * (vec3<f32>(1.0) + CB.highlights.xyz * beta);    // highlights gain
  let expo = vec3<f32>(1.0) - CB.midtones.xyz * gm;       // midtones per-channel power (sign-aware)
  g = sign(g) * pow(abs(g) + 1e-6, expo);

  // Scene-linear contrast about the grading-luminance fulcrum.
  let con = CB.params.x;
  if (abs(con) > 1e-4) {
    let y2 = max(dot(g, CB_YW), 1e-6);
    let yc = CB_GREY * pow(y2 / CB_GREY, 1.0 + con);
    g = g * (yc / y2);
  }
  // Global chroma / saturation about luminance.
  let sat = CB.params.y;
  if (abs(sat) > 1e-4) {
    let y3 = dot(g, CB_YW);
    g = mix(vec3<f32>(y3), g, 1.0 + sat);
  }

  return max(cb_inv(g), vec3<f32>(0.0));
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
  // Viewport path (active) maps a crop-local sub-rect; legacy path (inactive) letterbox-fits the
  // whole crop — byte-identical to before this binding.
  var geo: GeomResult;
  if (VIEW.flags.x > 0.5) {
    geo = crop_to_source(VIEW.rect.xy + in.uv * VIEW.rect.zw);
  } else {
    geo = geom_resolve(in.uv);
  }
  if (!geo.inside) {
    return vec4<f32>(LETTERBOX, 1.0);
  }
  let suv = geo.src_uv;
  // Detail stage (sharpen + NR) on the linear input at the geometry-remapped source uv, then global
  // white balance (chromatic adaptation), once, in linear ProPhoto. P.wb_gain is held at identity now
  // that global WB rides this matrix; masks keep their gain delta.
  let base_rgb = apply_detail(suv, sample_bilinear(suv));
  // Scene-linear baseline gain (EX.texel.z, default 1.0) normalizes exposure so a correctly-exposed
  // mid-grey reaches the ACR base curve's 0.18 input. Applied once, before develop + the tone curve.
  let base_wb = wb_apply(base_rgb) * EX.texel.z;

  // --- SCENE-LINEAR STAGE: base develop, then composite each mask's linear deltas ---
  var lin = apply_local_linear(
    base_wb, vec3<f32>(1.0), P.exposure, P.highlights, P.shadows, P.saturation);
  for (var i = 0u; i < M.count; i = i + 1u) {
    let mp = M.masks[i];
    let a = textureSampleLevel(mask_tex, mask_smp, suv, i32(i), 0.0).r * mp.opacity;
    if (a < 1e-4) { continue; }
    let li = apply_local_linear(
      base_wb,
      mp.wb_gain,
      P.exposure + mp.exposure,
      P.highlights + mp.highlights,
      P.shadows + mp.shadows,
      P.saturation + mp.saturation,
    );
    lin = mix(lin, li, a);
  }

  // Color-balance-RGB grading (scene-linear, after develop+masks, before the base tone operator).
  lin = apply_color_balance(lin);

  // --- DISPLAY TRANSITION (shared, once): base tone operator (ProPhoto) → ProPhoto→sRGB → OETF → HSL → tone curve ---
  // The base operator maps scene-linear→display-linear in ProPhoto (covering >1.0 headroom), then the
  // matrix + OETF take it to display-encoded sRGB. Out-of-sRGB-gamut colors clamp at the final step.
  var d = srgb_encode(pp_to_srgb(apply_base_tone(lin)));
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
    let a = textureSampleLevel(mask_tex, mask_smp, suv, i32(i), 0.0).r * mp.opacity;
    if (a < 1e-4) { continue; }
    let di = apply_local_display(
      d, P.contrast + mp.contrast, P.blacks + mp.blacks, P.whites + mp.whites);
    out = mix(out, di, a);
  }

  // Mask coverage overlay (Lightroom-style red tint). flags.y = packed layer index (-1 = off).
  // Sits on the developed pixels but BEFORE the vignette so the overlay itself isn't vignetted.
  let ov = i32(round(VIEW.flags.y));
  if (ov >= 0) {
    let cov = clamp(textureSampleLevel(mask_tex, mask_smp, suv, ov, 0.0).r, 0.0, 1.0);
    out = mix(out, VIEW.color.xyz, cov * VIEW.flags.z);
  }

  // Lens vignette: radial darken(−) / brighten(+) toward the corners of the CROPPED frame (uses the
  // crop-local uv so the vignette follows the crop, like Lightroom).
  let vig = EX.detail.w;
  if (abs(vig) > 1e-4) {
    let rad = distance(geo.u, vec2<f32>(0.5)) / 0.70710678; // 0 center .. 1 corner
    out = out * (1.0 + vig * 0.6 * smoothstep(0.3, 1.0, rad));
  }

  return vec4<f32>(clamp(out, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
