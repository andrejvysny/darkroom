import { execFileSync } from "node:child_process";
import { realpath } from "node:fs/promises";
import { test as base, expect } from "@playwright/test";
import { PluginClient, TauriPage } from "@srsholmes/tauri-playwright";
type TP = TauriPage;

export { expect };
export const test = base.extend<{ tauriPage: TauriPage }>({
  tauriPage: async ({}, use) => {
    const client = new PluginClient("/tmp/tauri-playwright.sock");
    await client.connect();
    try {
      const ping = await client.send({ type: "ping" });
      if (!ping.ok) throw new Error("Plugin ping failed");
      await use(new TauriPage(client));
    } finally {
      client.disconnect();
    }
  },
});

export const PROFILE = "/private/tmp/darkroom-native-baseline-profile-20261030";
type Request = { token: number; requestId: number; imageId: number; exposure: number; masks: number[] };
export type Frame = { at: number; width: number; height: number; checksum: number; request: Request };
export type Capture = { frames: Frame[]; start: number; exposure: number };

export async function verifyProfile(page: TP): Promise<void> {
  const actual = await page.evaluate<{ identifier: string; path: string; mock: string }>(`(async () => ({
    identifier: await window.__TAURI__.app.getIdentifier(),
    path: await window.__TAURI__.path.appDataDir(), mock: typeof window.__darkroomThumbMock
  }))()`);
  expect(actual.identifier).toBe("com.andrejvysny.darkroom.baseline.20261030");
  expect(actual.mock).toBe("undefined");
  expect(await realpath(actual.path)).toBe(await realpath(PROFILE));
}

export function rss(): number | null {
  const pid = process.env.DARKROOM_BASELINE_PID;
  if (!pid || !/^\d+$/.test(pid)) return null;
  try {
    return Number(execFileSync("ps", ["-p", pid, "-o", "rss="], { encoding: "utf8" }).trim());
  } catch {
    return null;
  }
}

export async function installCapture(page: TP): Promise<void> {
  await page.evaluate(`(async () => {
    const original = CanvasRenderingContext2D.prototype.putImageData;
    const { setDevelopRenderObserver } = await import('/src/lib/developRenderObservation.ts');
    const responses = [];
    const state = window.__baseline = { frames: [], original, start: 0, token: 0 };
    setDevelopRenderObserver(args => {
      const request = { token: state.token, requestId: args.requestId, imageId: args.imageId,
        exposure: args.params.exposure, masks: args.params.masks.map(mask => mask.adjust.exposure) };
      return buffer => {
        if (buffer.byteLength > 9) {
          const header = new DataView(buffer);
          responses.push({ request, width: header.getUint32(0, true), height: header.getUint32(4, true),
            data: new Uint8Array(buffer, 9) });
          if (responses.length > 8) responses.shift();
        }
      };
    });
    CanvasRenderingContext2D.prototype.putImageData = function(image, ...args) {
      const result = original.call(this, image, ...args);
      const paintedAt = performance.now();
      if (this.canvas.isConnected && this.canvas.closest('section')?.style.background === 'var(--color-stage-dev)') {
        const matches = responses.filter(response => {
          if (response.width !== image.width || response.height !== image.height || response.data.length !== image.data.length) return false;
          for (let i = 0; i < image.data.length; i++) if (response.data[i] !== image.data[i]) return false;
          return true;
        });
        // Ambiguous identical responses cannot establish which request was painted.
        if (matches.length !== 1 || matches[0].request.token !== state.token) return result;
        const request = matches[0].request;
        responses.splice(responses.indexOf(matches[0]), 1);
        let checksum = 0;
        for (let i = 0; i < image.data.length; i += 4096) checksum = (checksum * 31 + image.data[i]) >>> 0;
        state.frames.push({ at: paintedAt, width: image.width, height: image.height, checksum, request });
      }
      return result;
    };
  })()`);
}

export async function mark(page: TP): Promise<void> {
  await page.evaluate("(() => { window.__baseline.token++; window.__baseline.frames = []; window.__baseline.start = performance.now(); })()");
}

export async function resetCopiedEdit(page: TP): Promise<void> {
  await mark(page);
  await page.evaluate(`(() => {
    const button = Array.from(document.querySelectorAll('button')).find(element => element.textContent.trim() === 'Reset');
    if (!button) throw new Error('Develop Reset action missing');
    button.click();
  })()`);
  await waitFrame(page, 30000, { exposure: 0, masks: [] });
}

export async function waitFrame(page: TP, timeout = 30000, expected?: { imageId?: number; exposure?: number; masks?: number[] }): Promise<Capture> {
  const end = Date.now() + timeout;
  while (Date.now() < end) {
    const capture = await page.evaluate<Capture>(`(async () => ({
      frames: window.__baseline.frames, start: window.__baseline.start,
      exposure: (await import('/src/store/develop.ts')).useDevelopStore.getState().params.exposure
    }))()`);
    const frames = capture.frames.filter(({ request }) =>
      (expected?.imageId === undefined || request.imageId === expected.imageId) &&
      (expected?.exposure === undefined || Math.abs(request.exposure - expected.exposure) < 0.0001) &&
      (expected?.masks === undefined || JSON.stringify(request.masks) === JSON.stringify(expected.masks)));
    if (frames.length) return { ...capture, frames };
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new Error("Timed out waiting for Stage putImageData");
}

export async function setExposure(page: TP, value: number): Promise<void> {
  await page.evaluate(`(() => {
    const target = document.querySelector('[data-testid="slider-exposure"]');
    if (!target) throw new Error('Exposure slider missing');
    const rect = target.getBoundingClientRect();
    const x = rect.left + rect.width * (${value} + 5) / 10;
    const original = target.setPointerCapture;
    target.setPointerCapture = () => {};
    try {
      target.dispatchEvent(new PointerEvent('pointerdown', { bubbles: true, pointerId: 1, clientX: x, clientY: rect.top + 7 }));
      target.dispatchEvent(new PointerEvent('pointerup', { bubbles: true, pointerId: 1, clientX: x, clientY: rect.top + 7 }));
    } finally { target.setPointerCapture = original; }
  })()`);
}

export async function uninstallCapture(page: TP): Promise<void> {
  await page.evaluate("(async () => { if (window.__baseline) CanvasRenderingContext2D.prototype.putImageData = window.__baseline.original; (await import('/src/lib/developRenderObservation.ts')).setDevelopRenderObserver(undefined); delete window.__baseline; })()");
}
