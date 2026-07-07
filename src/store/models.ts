import { create } from "zustand";
import type { ModelGroup, ModelGroupId } from "../lib/ipc";

/** Live state of one capability's in-flight (or just-finished) download. */
export type DownloadState = {
  phase: "downloading" | "done";
  /** Currently-downloading file (relative path); "" between files / on the final tick. */
  fileName: string;
  fileIndex: number;
  fileCount: number;
  bytesDone: number;
  bytesTotal: number;
  /** Smoothed transfer rate (bytes/sec); 0 until two samples seen. */
  bytesPerSec: number;
  /** Estimated seconds remaining; null when unknown. */
  etaSec: number | null;
  /** Non-cancel failure message to surface on the card; null while healthy. */
  error: string | null;
};

interface ModelsStore {
  /** Snapshot from `models_overview`; refreshed after each download/remove + on `:done`. */
  overview: ModelGroup[];
  /** Per-capability live download state; entry present only while downloading / erroring. */
  downloads: Partial<Record<ModelGroupId, DownloadState>>;
  setOverview: (o: ModelGroup[]) => void;
  setDownload: (id: ModelGroupId, d: DownloadState | null) => void;
}

export const useModelsStore = create<ModelsStore>((set) => ({
  overview: [],
  downloads: {},
  setOverview: (o) => set({ overview: o }),
  setDownload: (id, d) =>
    set((s) => {
      const next = { ...s.downloads };
      if (d === null) delete next[id];
      else next[id] = d;
      return { downloads: next };
    }),
}));
