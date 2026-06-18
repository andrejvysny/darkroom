import type { TauriPage } from "@srsholmes/tauri-playwright";

// Call a REAL backend command over the socket bridge and return its (JSON-serializable) result.
// evaluate() takes an EXPRESSION → use a concise-body async IIFE (no top-level return/await).
export function call<T = unknown>(
  page: { evaluate<R>(s: string): Promise<R> },
  cmd: string,
  args: Record<string, unknown> = {},
): Promise<T> {
  const expr =
    `(async () => (await window.__TAURI_INTERNALS__.invoke(` +
    `${JSON.stringify(cmd)}, ${JSON.stringify(args)})))()`;
  return page.evaluate<T>(expr);
}

// Same, but for commands that return binary (ArrayBuffer) — return byteLength (ArrayBuffer is not
// JSON-serializable over the bridge, so we measure it inside the webview).
export function callLen(
  page: { evaluate<R>(s: string): Promise<R> },
  cmd: string,
  args: Record<string, unknown> = {},
): Promise<number> {
  const expr =
    `(async () => ((await window.__TAURI_INTERNALS__.invoke(` +
    `${JSON.stringify(cmd)}, ${JSON.stringify(args)}))?.byteLength ?? -1))()`;
  return page.evaluate<number>(expr);
}

export type TP = TauriPage;
