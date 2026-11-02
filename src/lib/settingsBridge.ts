import { log } from "./logger";
import { settingsBridgeWrite, type WebSnapshot } from "./ipc";

/**
 * The legacy app's preferences that the native (Rust) settings store imports. Values are the
 * RAW localStorage strings — never re-encoded — so the native parser sees exactly what the
 * browser stored.
 */
export const BRIDGED_KEYS = [
  "darkroom.bootstrapped",
  "darkroom.autoUpdateCheck",
  "darkroom.suggestions.show",
  "darkroom.export.dir",
  "darkroom.export.presets",
  "darkroom.export.last",
] as const;

export type BridgedKey = (typeof BRIDGED_KEYS)[number];

/** Debounce for bridge writes: slider-like toggles fire in bursts; one snapshot per burst. */
const BRIDGE_DEBOUNCE_MS = 500;

/** Capture the current bridged preferences as the envelope the native store ingests. */
export function buildSnapshot(
  storage: Pick<Storage, "getItem">,
  now: number,
): WebSnapshot {
  const values: Record<string, string | null> = {};
  for (const key of BRIDGED_KEYS) values[key] = storage.getItem(key);
  return {
    format: "darkroom-web-settings",
    version: 1,
    capturedAtMs: now,
    values,
  };
}

let pendingTimer: number | undefined;

/**
 * Debounced push of the current snapshot to the native bridge file. Fire-and-forget: a failed
 * write is logged (the next change or launch retries) and never surfaces to the caller — a
 * broken bridge must not break the legacy UI.
 */
export function scheduleBridgeWrite(): void {
  if (pendingTimer !== undefined) {
    window.clearTimeout(pendingTimer);
    pendingTimer = undefined;
  }
  pendingTimer = window.setTimeout(() => {
    pendingTimer = undefined;
    const snapshot = buildSnapshot(localStorage, Date.now());
    settingsBridgeWrite(snapshot).catch((err) => {
      log.warn("settingsBridge", "bridge write failed", log.errorSummary(err));
    });
  }, BRIDGE_DEBOUNCE_MS);
}

// Each bridged key gets an explicit literal write call, so every persisted key stays greppable
// at its localStorage call site (the migration parity scanner reads `src/` for exactly these).
const WRITE_PERSISTED: Record<BridgedKey, (value: string) => void> = {
  "darkroom.bootstrapped": (v) => localStorage.setItem("darkroom.bootstrapped", v),
  "darkroom.autoUpdateCheck": (v) => localStorage.setItem("darkroom.autoUpdateCheck", v),
  "darkroom.suggestions.show": (v) => localStorage.setItem("darkroom.suggestions.show", v),
  "darkroom.export.dir": (v) => localStorage.setItem("darkroom.export.dir", v),
  "darkroom.export.presets": (v) => localStorage.setItem("darkroom.export.presets", v),
  "darkroom.export.last": (v) => localStorage.setItem("darkroom.export.last", v),
};

/**
 * Write a bridged preference to localStorage and mirror it into the native settings bridge.
 * Drop-in replacement for the old direct `localStorage` writes of a bridged key; reads stay direct.
 */
export function setPref(key: BridgedKey, value: string): void {
  try {
    WRITE_PERSISTED[key](value);
  } catch {
    /* private mode / storage disabled — the preference just won't persist locally */
  }
  scheduleBridgeWrite();
}
