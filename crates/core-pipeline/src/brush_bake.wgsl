// Brush bake: rasterize one stroke's dabs into the brush coverage texture (R16Float, image-sized).
// Instanced — one quad per dab. Paint strokes use MAX blend (overlapping dabs don't darken); erase
// strokes use a multiply blend (dst*(1-src)) set on the pipeline. Dabs are circular in IMAGE-pixel
// space (aspect-corrected) so the brush feels round on screen.

struct BakeU {
  // (L/w, L/h) where L = longest edge — converts a longest-edge-fraction size into uv radii.
  aspect: vec2<f32>,
  _pad: vec2<f32>,
};
@group(0) @binding(0) var<uniform> B: BakeU;

struct VsIn {
  @builtin(vertex_index) vid: u32,
  // instance: a = (cx, cy, size, hardness), b = (strength, _, _, _)
  @location(0) a: vec4<f32>,
  @location(1) b: vec4<f32>,
};

struct VsOut {
  @builtin(position) pos: vec4<f32>,
  @location(0) local: vec2<f32>, // quad-local [-1,1]; circle edge at length 1
  @location(1) strength: f32,
  @location(2) hardness: f32,
};

@vertex
fn vs(in: VsIn) -> VsOut {
  // Two-triangle quad corners in [-1,1].
  var corners = array<vec2<f32>, 6>(
    vec2<f32>(-1.0, -1.0),
    vec2<f32>( 1.0, -1.0),
    vec2<f32>(-1.0,  1.0),
    vec2<f32>(-1.0,  1.0),
    vec2<f32>( 1.0, -1.0),
    vec2<f32>( 1.0,  1.0),
  );
  let corner = corners[in.vid];
  let center = in.a.xy;
  let size = in.a.z;
  let r = vec2<f32>(size * B.aspect.x, size * B.aspect.y); // uv radii
  let uv = center + corner * r;
  // uv (y down) -> clip (y up)
  let clip = vec2<f32>(uv.x * 2.0 - 1.0, 1.0 - 2.0 * uv.y);
  var out: VsOut;
  out.pos = vec4<f32>(clip, 0.0, 1.0);
  out.local = corner;
  out.strength = in.b.x;
  out.hardness = in.a.w;
  return out;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
  let d = length(in.local); // 0 center .. 1 circle edge
  if (d > 1.0) {
    discard;
  }
  let hard = clamp(in.hardness, 0.0, 0.999);
  let t = clamp((d - hard) / (1.0 - hard), 0.0, 1.0);
  let falloff = 1.0 - smoothstep(0.0, 1.0, t);
  let cov = clamp(in.strength, 0.0, 1.0) * falloff;
  return vec4<f32>(cov, 0.0, 0.0, 1.0);
}
