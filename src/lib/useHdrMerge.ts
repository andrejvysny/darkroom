import { useState, useEffect, useCallback, useRef } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { hdrMerge, type ImageRow } from "./ipc";
import { useAppStore } from "../store/app";
import { log } from "./logger";

/** `hdr:done` payload: the new catalog row plus any per-frame merge warnings (e.g. a frame that
 *  couldn't be aligned and was merged unaligned). */
interface HdrDone {
  warnings?: string[];
}

export interface HdrProgress {
  done: number;
  total: number;
  stage: string;
}

export interface HdrMergeState {
  /** Live progress of a running merge, or null when idle. */
  progress: HdrProgress | null;
  merging: boolean;
}

export interface HdrMergeResult {
  row: ImageRow | null;
  /** Set when the merge failed — a user-presentable message for the caller's toast. */
  error: string | null;
}

export interface HdrMergeActions {
  /** Run a merge over the selected image ids. Never rejects. */
  merge: (imageIds: number[]) => Promise<HdrMergeResult>;
}

/** Merge-to-HDR progress + trigger, mirroring the `useAnalysis` event-hook pattern:
 *  backend emits `hdr:progress {done,total,stage}` and `hdr:done {image}`. */
export function useHdrMerge(): HdrMergeState & HdrMergeActions {
  const [progress, setProgress] = useState<HdrProgress | null>(null);
  const [merging, setMerging] = useState(false);
  const bootstrappedRef = useRef(false);

  const merge = useCallback(
    async (imageIds: number[]): Promise<HdrMergeResult> => {
      setMerging(true);
      setProgress({ done: 0, total: imageIds.length + 2, stage: "starting" });
      try {
        return { row: await hdrMerge(imageIds), error: null };
      } catch (err) {
        const msg = typeof err === "string" ? err : String(err);
        log.warn("hdr", "merge failed", { imageIds, ...log.errorSummary(err) });
        return { row: null, error: msg };
      } finally {
        setMerging(false);
        setProgress(null);
      }
    },
    [],
  );

  useEffect(() => {
    if (bootstrappedRef.current) return;
    bootstrappedRef.current = true;
    const unlisteners: UnlistenFn[] = [];

    async function setup() {
      unlisteners.push(
        await listen<HdrProgress>("hdr:progress", (ev) =>
          setProgress(ev.payload),
        ),
      );
      unlisteners.push(
        await listen<HdrDone>("hdr:done", (ev) => {
          setProgress(null);
          const warnings = ev.payload?.warnings ?? [];
          if (warnings.length > 0) {
            const extra =
              warnings.length > 1 ? ` (+${warnings.length - 1} more)` : "";
            useAppStore.getState().setToast(`${warnings[0]}${extra}`);
          }
        }),
      );
    }
    void setup();

    return () => {
      unlisteners.forEach((fn) => fn());
    };
  }, []);

  return { progress, merging, merge };
}
