/**
 * Pure view-rect math for the canvas viewport model.
 *
 * The view lives in CROP-LOCAL UV space [0,1]×[0,1]:
 *   (ox,oy) = top-left corner of the visible window
 *   (sx,sy) = size of the visible window
 *
 * "crop-local" means the unit square represents the crop rectangle (whatever
 * crop/straighten is active). For the default full-frame crop the crop-local
 * space equals the source image uv space.
 *
 * The aspect constraint (plan Workstream C / A3) guarantees undistorted pixels:
 *   sx / sy == displayAspect / cropAspect
 * where displayAspect = outW / outH, cropAspect = (naturalW / naturalH) * (crop.hw / crop.hh).
 * For the default full-frame crop cropAspect == naturalW / naturalH.
 */

export interface ViewRect {
  ox: number; // top-left x in crop-local uv
  oy: number; // top-left y in crop-local uv
  sx: number; // width in crop-local uv
  sy: number; // height in crop-local uv
}

/** Zoom and pan state. panNorm is the center of the view in crop-local uv (default 0.5,0.5). */
export interface ViewState {
  zoom: number; // 1.0 = fit (whole crop visible)
  panNorm: { x: number; y: number }; // center of view in crop-local uv, [0,1]
}

/**
 * Result of deriving the full rendering geometry from view state.
 *
 * view       — what the backend needs (crop-local UV window)
 * outW/outH  — canvas backing store size (device px)
 * imgCssW/H  — how many CSS px the entire fit image occupies (used for overlay math)
 * visCss     — visible CSS size of the image (after letterbox subtraction)
 */
export interface DerivedView {
  view: ViewRect;
  outW: number;
  outH: number;
  imgCssW: number;
  imgCssH: number;
  visCssW: number;
  visCssH: number;
}

const MAX_DPR = 2;
const MIN_ZOOM = 0.5;
const MAX_ZOOM = 8;

/**
 * The view-rect math (from the plan's Workstream C "the exact view-rect math"):
 *
 * 1. fitScale = min(containerCss.w / imgAspect, containerCss.h) in "img height px"
 *    More precisely: the fit image occupies imgCssW = containerW * (imgAspect / max(imgAspect, containerAspect))
 *    and analogously for height. The "fit" view is {0,0,1,1} only when the container has the same
 *    aspect as the image; otherwise the letterbox area gets no pixels.
 *
 * 2. At zoom > 1 the visible crop-local window shrinks by 1/zoom:
 *    sx = 1/zoom,  sy = sx * (containerAspect / cropAspect)   [aspect constraint]
 *    But we also clamp sy ≤ 1 (cannot see outside the crop), which can force sx > 1/zoom —
 *    this only happens when the container is wider than the image and zoom is near 1.
 *
 * 3. Pan clamping: center stays at least sx/2 from the edges (so the window never exceeds [0,1]).
 */
export function deriveViewRect(
  zoom: number,
  panNorm: { x: number; y: number },
  natural: { w: number; h: number },
  containerCss: { w: number; h: number },
  dpr: number,
): DerivedView {
  const effectiveDpr = Math.min(dpr, MAX_DPR);
  const cw = Math.max(1, containerCss.w);
  const ch = Math.max(1, containerCss.h);
  const nw = Math.max(1, natural.w);
  const nh = Math.max(1, natural.h);

  // `natural` is the RENDERED FRAME's pixel dims (crop-adjusted by the caller), so its aspect is the
  // crop-local image aspect.
  const cropAspect = nw / nh;

  // Fit: CSS px the whole image occupies, letterboxed/pillarboxed in the container (zoom = 1).
  let imgFitW: number;
  let imgFitH: number;
  if (cw / ch > cropAspect) {
    imgFitH = ch; // pillarbox — height fills
    imgFitW = ch * cropAspect;
  } else {
    imgFitW = cw; // letterbox — width fills
    imgFitH = cw / cropAspect;
  }

  // On-screen image size at this zoom (zoom = 1 ⇒ fit), and the visible area = image ∩ container.
  const imgCssW = imgFitW * zoom;
  const imgCssH = imgFitH * zoom;
  const visCssW = Math.min(cw, imgCssW);
  const visCssH = Math.min(ch, imgCssH);

  // The crop-local view window = the visible fraction of the image on each axis. The per-axis `min`
  // handles letterbox/pillarbox correctly for ANY container/image aspect (at fit this gives the
  // whole image {0,0,1,1}); the output buffer is sized to the visible area so its aspect matches the
  // window's — no distortion. (This replaces an earlier aspect-ratio constraint that cropped the
  // image at fit whenever container aspect ≠ image aspect.)
  const sx = Math.min(1, visCssW / imgCssW);
  const sy = Math.min(1, visCssH / imgCssH);

  // Pan clamp: center at least half-window from each edge so the window stays within [0,1].
  const halfSx = sx / 2;
  const halfSy = sy / 2;
  const cx = Math.max(halfSx, Math.min(1 - halfSx, panNorm.x));
  const cy = Math.max(halfSy, Math.min(1 - halfSy, panNorm.y));

  const view: ViewRect = { ox: cx - halfSx, oy: cy - halfSy, sx, sy };

  const outW = Math.max(1, Math.round(visCssW * effectiveDpr));
  const outH = Math.max(1, Math.round(visCssH * effectiveDpr));

  return { view, outW, outH, imgCssW, imgCssH, visCssW, visCssH };
}

/**
 * Zoom anchored at a point (clientX/Y relative to the container's top-left corner).
 * Keeps the image pixel under the cursor at the same screen position.
 */
export function zoomAtPoint(
  state: ViewState,
  nextZoom: number,
  anchorCss: { x: number; y: number },
  natural: { w: number; h: number },
  containerCss: { w: number; h: number },
  dpr: number,
): ViewState {
  const clampedZoom = Math.max(MIN_ZOOM, Math.min(MAX_ZOOM, nextZoom));
  const before = deriveViewRect(
    state.zoom,
    state.panNorm,
    natural,
    containerCss,
    dpr,
  );

  // What crop-local uv point is under the cursor?
  const normPoint = canvasPxToNorm(
    before.view,
    { w: before.visCssW, h: before.visCssH },
    anchorCss.x,
    anchorCss.y,
  );

  // After zoom, the same uv point should remain under the cursor.
  // The cursor is at (anchorCss.x / visCssW) of the visible area.
  const after = deriveViewRect(
    clampedZoom,
    { x: 0.5, y: 0.5 }, // temporary center
    natural,
    containerCss,
    dpr,
  );

  // New center such that normPoint lands at the same screen fraction
  const fracX = anchorCss.x / Math.max(1, before.visCssW);
  const fracY = anchorCss.y / Math.max(1, before.visCssH);
  const newCx = normPoint.x - (fracX - 0.5) * after.view.sx;
  const newCy = normPoint.y - (fracY - 0.5) * after.view.sy;

  // Clamp pan
  const halfSx = after.view.sx / 2;
  const halfSy = after.view.sy / 2;
  const panX = Math.max(halfSx, Math.min(1 - halfSx, newCx));
  const panY = Math.max(halfSy, Math.min(1 - halfSy, newCy));

  return { zoom: clampedZoom, panNorm: { x: panX, y: panY } };
}

/**
 * Map a CSS pixel position inside the visible image rect to crop-local UV.
 * anchorCss is measured from the top-left of the visible image area (not the container).
 */
export function canvasPxToNorm(
  view: ViewRect,
  visCss: { w: number; h: number },
  clientX: number,
  clientY: number,
): { x: number; y: number } {
  const fracX = clientX / Math.max(1, visCss.w);
  const fracY = clientY / Math.max(1, visCss.h);
  return {
    x: view.ox + fracX * view.sx,
    y: view.oy + fracY * view.sy,
  };
}

/**
 * Map a crop-local UV point to a CSS pixel position within the visible image rect.
 */
export function viewToCanvasPx(
  view: ViewRect,
  p: { x: number; y: number },
  visCss: { w: number; h: number },
): { x: number; y: number } {
  return {
    x: ((p.x - view.ox) / view.sx) * visCss.w,
    y: ((p.y - view.oy) / view.sy) * visCss.h,
  };
}

/** The zoom level at which one source pixel == one display pixel (device pixel). */
export function zoom1to1(
  natural: { w: number; h: number },
  containerCss: { w: number; h: number },
  dpr: number,
): number {
  const effectiveDpr = Math.min(dpr, MAX_DPR);
  const fitZoom = Math.min(
    containerCss.w / Math.max(1, natural.w),
    containerCss.h / Math.max(1, natural.h),
  );
  // 1:1 means one display pixel per source pixel; at fitZoom=1 CSS px covers fitZoom source px.
  // To get to 1 source px per device px we need zoom = 1/(fitZoom * effectiveDpr).
  // But "zoom" is relative to fit (zoom=1 = fit), so:
  return 1 / (fitZoom * effectiveDpr);
}

/** Fit view state (zoom=1, centered). */
export function fitViewState(): ViewState {
  return { zoom: 1, panNorm: { x: 0.5, y: 0.5 } };
}

export { MIN_ZOOM, MAX_ZOOM };
