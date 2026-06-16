import {
  DEFAULT_LOCAL_ADJUST,
  type LocalAdjust,
  type Mask,
  type MaskComponent,
} from "./ipc";

/** Mask geometry & factories. All coordinates are normalized [0,1], origin top-left, matching the
 *  GPU pre-pass (`mask_prepass.wgsl`) and develop sampling convention. */

export function clamp01(v: number): number {
  return v < 0 ? 0 : v > 1 ? 1 : v;
}

/** Map a client (DOM) point to normalized [0,1] within an element's bounding rect. */
export function clientToNorm(
  rect: DOMRect,
  clientX: number,
  clientY: number,
): [number, number] {
  return [
    clamp01((clientX - rect.left) / Math.max(rect.width, 1)),
    clamp01((clientY - rect.top) / Math.max(rect.height, 1)),
  ];
}

function freshAdjust(): LocalAdjust {
  return { ...DEFAULT_LOCAL_ADJUST };
}

/** A graduated/linear mask: full effect at the top line, fading to none lower down. */
export function newLinearMask(): Mask {
  const component: MaskComponent = {
    kind: { type: "linear", p0: [0.5, 0.25], p1: [0.5, 0.6] },
    op: "add",
    invert: false,
    feather: false,
  };
  return {
    name: "Linear",
    components: [component],
    adjust: freshAdjust(),
    opacity: 1,
    enabled: true,
  };
}

/** A radial mask centered on the frame with a soft feather. */
export function newRadialMask(): Mask {
  const component: MaskComponent = {
    kind: {
      type: "radial",
      center: [0.5, 0.5],
      radius: [0.3, 0.3],
      angle: 0,
      feather: 0.5,
    },
    op: "add",
    invert: false,
    feather: false,
  };
  return {
    name: "Radial",
    components: [component],
    adjust: freshAdjust(),
    opacity: 1,
    enabled: true,
  };
}

/** An empty brush mask (strokes are added by painting on the overlay). */
export function newBrushMask(): Mask {
  const component: MaskComponent = {
    kind: { type: "brush", strokes: [] },
    op: "add",
    invert: false,
    feather: false,
  };
  return {
    name: "Brush",
    components: [component],
    adjust: freshAdjust(),
    opacity: 1,
    enabled: true,
  };
}

/** A luminance-range mask selecting mid-to-bright tones (refined by sliders). */
export function newLuminanceMask(): Mask {
  const component: MaskComponent = {
    kind: { type: "luminanceRange", lo: 0.4, hi: 1.0, feather: 0.08 },
    op: "add",
    invert: false,
    feather: false,
  };
  return {
    name: "Luminance",
    components: [component],
    adjust: freshAdjust(),
    opacity: 1,
    enabled: true,
  };
}

/** A color-range mask; target hue/sat are set by the eyedropper. */
export function newColorMask(): Mask {
  const component: MaskComponent = {
    kind: { type: "colorRange", hue: 0.5, sat: 0.5, tol: 0.08, feather: 0.06 },
    op: "add",
    invert: false,
    feather: false,
  };
  return {
    name: "Color",
    components: [component],
    adjust: freshAdjust(),
    opacity: 1,
    enabled: true,
  };
}

/** sRGB [0,1] → HSV [0,1] (matches the GPU prepass hue/sat convention). */
export function rgbToHsv(
  r: number,
  g: number,
  b: number,
): [number, number, number] {
  const max = Math.max(r, g, b);
  const min = Math.min(r, g, b);
  const d = max - min;
  let h = 0;
  if (d > 1e-6) {
    if (max === r) h = ((g - b) / d) % 6;
    else if (max === g) h = (b - r) / d + 2;
    else h = (r - g) / d + 4;
    h /= 6;
    if (h < 0) h += 1;
  }
  const s = max <= 1e-6 ? 0 : d / max;
  return [h, s, max];
}

const imgCache = new Map<string, Promise<HTMLImageElement>>();
function loadImage(url: string): Promise<HTMLImageElement> {
  let p = imgCache.get(url);
  if (!p) {
    p = new Promise((resolve, reject) => {
      const im = new Image();
      im.onload = () => resolve(im);
      im.onerror = reject;
      im.src = url;
    });
    imgCache.set(url, p);
  }
  return p;
}

/** Sample the displayed image at normalized (nx,ny) and return its hue & saturation. */
export async function samplePixelHsv(
  url: string,
  nx: number,
  ny: number,
): Promise<{ hue: number; sat: number }> {
  const img = await loadImage(url);
  const canvas = document.createElement("canvas");
  canvas.width = img.naturalWidth;
  canvas.height = img.naturalHeight;
  const ctx = canvas.getContext("2d");
  if (!ctx) return { hue: 0.5, sat: 0.5 };
  ctx.drawImage(img, 0, 0);
  const x = Math.min(
    img.naturalWidth - 1,
    Math.max(0, Math.round(nx * (img.naturalWidth - 1))),
  );
  const y = Math.min(
    img.naturalHeight - 1,
    Math.max(0, Math.round(ny * (img.naturalHeight - 1))),
  );
  const d = ctx.getImageData(x, y, 1, 1).data;
  const [hue, sat] = rgbToHsv(d[0] / 255, d[1] / 255, d[2] / 255);
  return { hue, sat };
}

/** Short human label for a mask's primary component (for the mask list). */
export function maskKindLabel(mask: Mask): string {
  const k = mask.components[0]?.kind.type;
  switch (k) {
    case "linear":
      return "Linear";
    case "radial":
      return "Radial";
    case "brush":
      return "Brush";
    case "luminanceRange":
      return "Luminance";
    case "colorRange":
      return "Color";
    case "ai":
      return "AI";
    default:
      return "Mask";
  }
}
