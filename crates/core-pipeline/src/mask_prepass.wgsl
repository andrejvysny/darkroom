// Mask pre-pass: compute one mask's composited alpha into a single R16Float layer.
// Components are looped and combined (Add=union / Subtract / Intersect) with optional invert.
// Coordinates are normalized [0,1], origin top-left (same uv convention as develop.wgsl).
//
// Coverage sources: Linear (0) and Radial (1) are procedural; Brush (2) samples the per-mask baked
// brush texture; LuminanceRange (3) / ColorRange (4) read the scene-linear input image (display-luma
// / hue·sat selection). Ai (5) is zero until a model fills its alpha (future).

struct Comp {
  kind: u32,   // 0 linear, 1 radial, 2 brush, 3 lumaRange, 4 colorRange, 5 ai
  op: u32,     // 0 add, 1 subtract, 2 intersect
  invert: u32, // 0/1
  _pad: u32,
  a: vec4<f32>, // linear:(p0.xy,p1.xy) · radial:(center.xy,radius.xy) · luma:(lo,hi,feather,_) · color:(hue,sat,tol,feather)
  b: vec4<f32>, // radial:(angle,feather,_,_)
};

struct Pre {
  count: u32,
  _p0: u32,
  _p1: u32,
  _p2: u32,
  comps: array<Comp, 8>,
};

@group(0) @binding(0) var<uniform> PRE: Pre;
@group(0) @binding(1) var brush_tex: texture_2d<f32>;
@group(0) @binding(2) var brush_smp: sampler;
@group(0) @binding(3) var input_tex: texture_2d<f32>;

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

fn rgb_to_hsv(c: vec3<f32>) -> vec3<f32> {
  let K = vec4<f32>(0.0, -1.0 / 3.0, 2.0 / 3.0, -1.0);
  let p = mix(vec4<f32>(c.bg, K.wz), vec4<f32>(c.gb, K.xy), step(c.b, c.g));
  let q = mix(vec4<f32>(p.xyw, c.r), vec4<f32>(c.r, p.yzx), step(p.x, c.r));
  let d = q.x - min(q.w, q.y);
  let e = 1.0e-10;
  return vec3<f32>(abs(q.z + (q.w - q.y) / (6.0 * d + e)), d / (q.x + e), q.x);
}

// Sample the input image at uv as display-referred sRGB (what the user sees when picking ranges).
fn sample_display(uv: vec2<f32>) -> vec3<f32> {
  let dims = vec2<f32>(textureDimensions(input_tex));
  let px = vec2<i32>(clamp(uv, vec2<f32>(0.0), vec2<f32>(1.0)) * (dims - 1.0));
  let lin = textureLoad(input_tex, px, 0).rgb;
  return srgb_encode(clamp(lin, vec3<f32>(0.0), vec3<f32>(1.0)));
}

fn linear_cov(uv: vec2<f32>, p0: vec2<f32>, p1: vec2<f32>) -> f32 {
  let axis = p1 - p0;
  let len2 = dot(axis, axis);
  if (len2 < 1e-8) { return 1.0; }
  let t = dot(uv - p0, axis) / len2;
  return 1.0 - smoothstep(0.0, 1.0, clamp(t, 0.0, 1.0));
}

fn radial_cov(uv: vec2<f32>, center: vec2<f32>, radius: vec2<f32>, angle: f32, feather: f32) -> f32 {
  let co = cos(angle);
  let si = sin(angle);
  let d = uv - center;
  let rx = (d.x * co + d.y * si) / max(radius.x, 1e-5);
  let ry = (-d.x * si + d.y * co) / max(radius.y, 1e-5);
  let rr = sqrt(rx * rx + ry * ry);
  let inner = 1.0 - clamp(feather, 0.0, 1.0);
  let t = clamp((rr - inner) / max(1.0 - inner, 1e-5), 0.0, 1.0);
  return 1.0 - smoothstep(0.0, 1.0, t);
}

// Luminance range: trapezoid on display luma with soft `feather` ramps at both ends.
fn luma_cov(uv: vec2<f32>, lo: f32, hi: f32, feather: f32) -> f32 {
  let l = dot(sample_display(uv), LUMA);
  let f = max(feather, 1e-3);
  let lower = smoothstep(lo - f, lo + f, l);
  let upper = 1.0 - smoothstep(hi - f, hi + f, l);
  return clamp(lower * upper, 0.0, 1.0);
}

// Color range: select pixels near a target hue & saturation, softened by tol/feather.
fn color_cov(uv: vec2<f32>, hue: f32, sat: f32, tol: f32, feather: f32) -> f32 {
  let hsv = rgb_to_hsv(sample_display(uv));
  // circular hue distance in [0,0.5]
  var dh = abs(hsv.x - hue);
  dh = min(dh, 1.0 - dh);
  let f = max(feather, 1e-3);
  let hue_w = 1.0 - smoothstep(tol, tol + f, dh);
  let sat_w = 1.0 - smoothstep(0.35, 0.35 + f, abs(hsv.y - sat));
  // Require some saturation so greys don't match every hue.
  let chroma = smoothstep(0.04, 0.12, hsv.y);
  return clamp(hue_w * sat_w * chroma, 0.0, 1.0);
}

@fragment
fn fs_prepass(in: VsOut) -> @location(0) vec4<f32> {
  var alpha = 0.0;
  for (var i = 0u; i < PRE.count; i = i + 1u) {
    let c = PRE.comps[i];
    var cov = 0.0;
    if (c.kind == 0u) {
      cov = linear_cov(in.uv, c.a.xy, c.a.zw);
    } else if (c.kind == 1u) {
      cov = radial_cov(in.uv, c.a.xy, c.a.zw, c.b.x, c.b.y);
    } else if (c.kind == 2u) {
      cov = textureSampleLevel(brush_tex, brush_smp, in.uv, 0.0).r;
    } else if (c.kind == 3u) {
      cov = luma_cov(in.uv, c.a.x, c.a.y, c.a.z);
    } else if (c.kind == 4u) {
      cov = color_cov(in.uv, c.a.x, c.a.y, c.a.z, c.a.w);
    }
    // kind 5 (ai): 0 coverage for now.
    if (c.invert == 1u) { cov = 1.0 - cov; }

    if (c.op == 0u) {
      alpha = max(alpha, cov);          // Add (union)
    } else if (c.op == 1u) {
      alpha = alpha * (1.0 - cov);      // Subtract
    } else {
      alpha = alpha * cov;              // Intersect
    }
  }
  return vec4<f32>(alpha, 0.0, 0.0, 1.0);
}
