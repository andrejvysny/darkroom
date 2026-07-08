import { useEffect } from "react";
import {
  check,
  type Update,
  type DownloadEvent,
} from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import { useUpdateStore } from "../store/update";
import { useAppStore } from "../store/app";
import { log } from "./logger";

const AUTO_CHECK_KEY = "darkroom.autoUpdateCheck";
/** Delay the startup check so it never competes with first-paint / initial indexing. */
const STARTUP_CHECK_DELAY_MS = 4000;

// The `Update` handle from the last successful check. Non-serializable (holds a native resource id),
// so it is kept module-side rather than in the store.
let pending: Update | null = null;

/** Auto-check defaults ON; persisted to localStorage so it survives restarts. */
export function autoCheckEnabled(): boolean {
  return localStorage.getItem(AUTO_CHECK_KEY) !== "0";
}
export function setAutoCheckEnabled(on: boolean): void {
  localStorage.setItem(AUTO_CHECK_KEY, on ? "1" : "0");
}

function inTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

function msg(err: unknown): string {
  if (typeof err === "string") return err;
  if (err instanceof Error) return err.message;
  return String(err);
}

/** Query the configured GitHub endpoint for a newer release. `silent` suppresses the
 *  up-to-date / failure toasts (used by the startup check). */
export async function checkForUpdate(opts?: { silent?: boolean }): Promise<void> {
  const silent = opts?.silent ?? false;
  const { patch } = useUpdateStore.getState();
  const { setToast } = useAppStore.getState();
  if (!inTauri()) {
    if (!silent) setToast("Update checks require the desktop app.");
    return;
  }
  patch({ status: "checking", error: null });
  try {
    const update = await check();
    if (update) {
      pending = update;
      patch({
        status: "available",
        version: update.version,
        notes: update.body ?? null,
        date: update.date ?? null,
        bytesDone: 0,
        bytesTotal: 0,
      });
    } else {
      pending = null;
      patch({ status: "uptodate" });
      if (!silent) setToast("You're on the latest version.");
    }
  } catch (err) {
    patch({ status: "error", error: msg(err) });
    log.warn("updater", "check failed", log.errorSummary(err));
    if (!silent) setToast("Update check failed.");
  }
}

/** Download + stage the pending update, streaming byte progress into the store. On success the
 *  status becomes `ready` (awaiting relaunch). No-op if no update is pending. */
export async function downloadAndInstall(): Promise<void> {
  const { patch } = useUpdateStore.getState();
  const update = pending;
  if (!update) return;
  patch({ status: "downloading", bytesDone: 0, bytesTotal: 0, error: null });
  let done = 0;
  let total = 0;
  try {
    await update.downloadAndInstall((e: DownloadEvent) => {
      switch (e.event) {
        case "Started":
          total = e.data.contentLength ?? 0;
          patch({ bytesTotal: total, bytesDone: 0 });
          break;
        case "Progress":
          done += e.data.chunkLength;
          patch({ bytesDone: done });
          break;
        case "Finished":
          patch({ bytesDone: total || done });
          break;
      }
    });
    patch({ status: "ready" });
  } catch (err) {
    patch({ status: "error", error: msg(err) });
    log.warn("updater", "download failed", log.errorSummary(err));
  }
}

/** Relaunch into the freshly-installed version. */
export async function restartApp(): Promise<void> {
  try {
    await relaunch();
  } catch (err) {
    log.warn("updater", "relaunch failed", log.errorSummary(err));
    useAppStore.getState().setToast("Restart failed — please reopen the app.");
  }
}

/** Hide the banner without acting (re-prompts next launch / next manual check). */
export function dismissUpdate(): void {
  useUpdateStore.getState().patch({ status: "idle" });
}

/**
 * Run one silent update check shortly after launch (if auto-check is enabled). Mount EXACTLY ONCE
 * (App.tsx). The manual "Check for updates" button and command-palette row call
 * {@link checkForUpdate} directly and must NOT call this hook.
 */
export function useUpdaterStartup(): void {
  useEffect(() => {
    if (!autoCheckEnabled()) return;
    // Plain schedule/cleanup (no ref guard) so it survives React StrictMode's mount→cleanup→remount:
    // the final mount's timer is the one that fires, exactly once.
    const t = window.setTimeout(
      () => void checkForUpdate({ silent: true }),
      STARTUP_CHECK_DELAY_MS,
    );
    return () => window.clearTimeout(t);
  }, []);
}
