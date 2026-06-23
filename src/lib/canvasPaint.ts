import type { RenderedFrame } from "./ipc";

/**
 * Blit a rendered frame onto a canvas: resize the backing store to the frame dims (if needed) then
 * `putImageData`. Sizing + paint happen together so there's no transparent-canvas flash between a
 * resize and the paint. (The identical path previously inlined in Stage + Loupe.)
 */
export function paintFrame(
  canvas: HTMLCanvasElement,
  frame: RenderedFrame,
): void {
  if (canvas.width !== frame.w || canvas.height !== frame.h) {
    canvas.width = frame.w;
    canvas.height = frame.h;
  }
  const ctx = canvas.getContext("2d");
  if (!ctx) return;
  ctx.putImageData(new ImageData(frame.data, frame.w, frame.h), 0, 0);
}
