import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  suggestStatus,
  suggestTrain,
  type SuggestStatus,
  type SuggestTrainOutcome,
} from "./ipc";
import { useAppStore, type SuggestStoreState } from "../store/app";
import { log } from "./logger";

export interface SuggestActions {
  /** Re-read the live model's metrics + label census from the catalog. */
  reload: () => Promise<void>;
  /** Fit on the current labels in the background. Resolves once the job is *queued*; the outcome
   *  arrives as `suggest:done` / `suggest:error` (handled by the singleton listeners below). */
  train: () => Promise<void>;
}

async function reload(): Promise<void> {
  useAppStore.getState().setSuggest({ loading: true });
  try {
    const status = await suggestStatus();
    useAppStore.getState().setSuggest({ status });
  } catch (err) {
    log.debug("suggest", "status fetch failed", log.errorSummary(err));
  } finally {
    useAppStore.getState().setSuggest({ loading: false });
  }
}

async function train(): Promise<void> {
  const { setSuggest, setToast, suggest } = useAppStore.getState();
  // Optimistic: flip `running` immediately so the Train button dims without waiting for a round-trip.
  if (suggest.status) {
    setSuggest({ status: { ...suggest.status, running: true } });
  }
  try {
    await suggestTrain();
  } catch (err) {
    const msg = err instanceof Error ? err.message : String(err);
    setToast(`Training failed: ${msg}`);
    log.warn("suggest", "train failed", log.errorSummary(err));
    void reload(); // pull the real `running` back in — the job never started
  }
}

/** What a finished fit did, for the toast. A fit that lost the promote gate keeps the live model's
 *  badges, which is worth saying out loud — otherwise "trained" looks like nothing happened. */
function outcomeMessage(o: SuggestTrainOutcome): string {
  const pct = (v: number) => `${Math.round(v * 100)}%`;
  return o.promoted
    ? `Suggestions updated — ${pct(o.cvAuc)} accuracy on ${o.nPos + o.nNeg} labels`
    : "Kept the current suggestion model — the new fit scored worse";
}

// The `suggest:*` listeners (+ the first status fetch) are process-wide singletons: set up once
// however many components call `useSuggest()` (LibraryView, LeftNav, SettingsModal). Mirrors
// `usePanoDetect.ts` — same reason: duplicate listeners meant doubled toasts and round-trips.
let listenersBootstrapped = false;

function bootstrapListeners(): void {
  if (listenersBootstrapped) return;
  listenersBootstrapped = true;

  void reload();

  void listen<SuggestTrainOutcome>("suggest:done", (ev) => {
    // Bump the refetch token BEFORE the status lands: the badges are already written at this point,
    // so the grid should not wait on a second round-trip to show them.
    const { suggest, setSuggest, setToast } = useAppStore.getState();
    setSuggest({ doneVersion: suggest.doneVersion + 1 });
    void reload();
    setToast(outcomeMessage(ev.payload));
  });

  void listen<{ message: string }>("suggest:error", (ev) => {
    void reload();
    useAppStore
      .getState()
      .setToast(`Suggestion training failed: ${ev.payload.message}`);
  });
}

/**
 * Pick/reject suggester state + actions, backed by the shared `suggest` store slice. Listeners and
 * the initial fetch are module singletons; the hook just reads the store and hands back stable
 * actions, so LeftNav (the "Suggested picks" shelf), SettingsModal (metrics + Train), and
 * LibraryView (refetch on `doneVersion`) always agree.
 */
export function useSuggest(): SuggestStoreState & SuggestActions {
  const suggest = useAppStore((s) => s.suggest);

  useEffect(() => {
    bootstrapListeners();
  }, []);

  return { ...suggest, reload, train };
}

/** True once the model has badged at least one image — gates the "Suggested picks" shelf, which
 *  would otherwise be an always-empty row on a library that has never been trained. */
export function hasSuggestions(status: SuggestStatus | null): boolean {
  return status != null && status.modelId != null && status.scored > 0;
}
