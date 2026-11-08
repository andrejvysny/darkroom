import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import {
  developPersistence,
  type SavedRevision,
} from "../lib/developPersistence";
import { useAppStore } from "../store/app";

/**
 * Keeps a normal quit from dropping an edit that is still only in memory.
 *
 * The backend blocks the exit, emits `app:flush-edits`, and waits for `develop_flush_ack` — so the
 * ack is ALWAYS sent, including on failure: a missing one only makes quit wait out the backend's
 * 5 s timeout. The ack echoes the request's `barrierId` (a reply to any other barrier is ignored)
 * and names the committed revision of each image saved, which the backend checks against the
 * catalog.
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
    const unFlush = listen<{ barrierId: number }>(
      "app:flush-edits",
      async (ev) => {
        if (!active) return;
        const barrierId = ev.payload?.barrierId;
        if (typeof barrierId !== "number") return;
        let ok = false;
        let saved: SavedRevision[] = [];
        try {
          saved = await developPersistence.flushAllWithRevisions();
          ok = !developPersistence.hasPending();
        } catch {
          ok = false;
        }
        // Never let a failed ack become an unhandled rejection — the backend would just wait out its
        // timeout, but the console noise would hide the real failure.
        try {
          await invoke("develop_flush_ack", {
            barrierId,
            ok,
            saved: ok ? saved : [],
          });
        } catch {
          /* the barrier's timeout is the backstop */
        }
      },
    );
    // A barrier that didn't reach disk (quit or updater-restart cancelled) must say why —
    // the user just pressed quit and nothing happened. (Payload mirrors Rust `SaveBarrierReport`.)
    const unBarrier = listen<{ reason: string; message: string }>(
      "app:save-barrier",
      (ev) => {
        if (active) useAppStore.getState().setToast(ev.payload.message);
      },
    );
    return () => {
      active = false;
      void unFlush.then((fn) => fn());
      void unBarrier.then((fn) => fn());
    };
  }, []);
}
