import type { Border } from "./ipc";

/** Framed-canvas geometry for a `w×h` image under a border. Mirrors the Rust `frame_rgba8` math
 * exactly so the live develop-stage preview matches the exported file. `ow/oh` = output dims, the
 * image is placed at `offX/offY`. With an inactive border this returns the image's own dims. */
export function computeFrame(
  w: number,
  h: number,
  border: Border,
): { ow: number; oh: number; offX: number; offY: number } {
  const margin = Math.round((Math.max(0, border.size) / 100) * Math.max(w, h));
  const baseW = w + 2 * margin;
  const baseH = h + 2 * margin;
  let ow = baseW;
  let oh = baseH;
  if (border.aspectW > 0 && border.aspectH > 0) {
    const r = border.aspectW / border.aspectH;
    if (baseW / baseH < r) {
      ow = Math.round(baseH * r);
      oh = baseH;
    } else {
      ow = baseW;
      oh = Math.round(baseW / r);
    }
  }
  return { ow, oh, offX: Math.floor((ow - w) / 2), offY: Math.floor((oh - h) / 2) };
}

/** `Rgb3` (0..1) → CSS `rgb(...)` string. */
export function rgb3ToCss(c: readonly [number, number, number]): string {
  const q = (v: number) => Math.round(Math.min(1, Math.max(0, v)) * 255);
  return `rgb(${q(c[0])}, ${q(c[1])}, ${q(c[2])})`;
}
