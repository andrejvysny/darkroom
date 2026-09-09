import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { developPersistence } from "../lib/developPersistence";

/**
 * Keeps a normal quit from dropping an edit that is still only in memory.
 *
 * The backend blocks the exit, emits `app:flush-edits`, and waits for `develop_flush_ack` — so the
 * ack is ALWAYS sent, including on failure: a missing one only makes quit wait out the backend's
 * 1500 ms timeout.
 *
 * `ok` is a real dirty check, not just "the promise resolved": a failed save is swallowed into the
 * retry schedule rather than rejecting, so `flushAll()` resolving proves nothing on its own —
 * `hasPending()` is what says whether anything is still unsaved. `ok: false` cancels this quit
 * attempt and leaves the error banner up; quitting again within 30 s exits regardless.
 *
 * Registered once app-wide — never torn down on view changes.
 */
export function useQuitFlush() {
  useEffect(() => {
    let active = true;
    const unFlush = listen("app:flush-edits", async () => {
      if (!active) return;
      let ok = false;
      try {
        await developPersistence.flushAll();
        ok = !developPersistence.hasPending();
      } catch {
        ok = false;
      }
      // Never let a failed ack become an unhandled rejection — the backend would just wait out its
      // timeout, but the console noise would hide the real failure.
      try {
        await invoke("develop_flush_ack", { ok });
      } catch {
        /* the barrier's timeout is the backstop */
      }
    });
    return () => {
      active = false;
      void unFlush.then((fn) => fn());
    };
  }, []);
}
