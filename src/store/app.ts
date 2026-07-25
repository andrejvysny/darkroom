import { create } from "zustand";
import type { ImageRow, PanoGroupRow } from "../lib/ipc";
import type { ExportTarget } from "../lib/export";

/** Live phase/progress of a running panorama-detection scan (`pano_detect:progress` payload), or
 *  `null` when idle. */
export type PanoDetectProgress = {
  phase: string;
  done: number;
  total: number;
} | null;

/** Panorama-detection scan state, shared by every consumer (see `panoDetect` below). */
export interface PanoDetectStoreState {
  running: boolean;
  suggested: number;
  groups: PanoGroupRow[];
  /** True while the initial (or a manual) status+groups fetch is in flight. */
  loading: boolean;
  progress: PanoDetectProgress;
}

interface AppState {
  view: "library" | "develop" | "dedup";
  setView: (v: "library" | "develop" | "dedup") => void;
  /** The current (filtered) library image set — shared so Develop's filmstrip/chrome can read it
   *  even while the LibraryView is unmounted. */
  libraryImages: ImageRow[];
  setLibraryImages: (rows: ImageRow[]) => void;
  /** Update one image's edit version (drives live edit-aware previews in the filmstrip/chrome). */
  setImageEdited: (id: number, editedAt: number | null) => void;
  /** Per-image thumbnail cache-bust counter, bumped when the backend renders a fresh canonical/edited
   *  thumbnail (`thumb:rendered`). Appended to `thumb://` URLs so the immutable-cached `<img>`
   *  refetches when the placeholder is replaced by the canonical render. */
  thumbVersions: Record<number, number>;
  /** Increment the cache-bust counter for each id (batched from coalesced `thumb:rendered` events). */
  bumpThumbVersions: (ids: number[]) => void;
  /** Primary/active selection (drives metadata panel + develop). */
  selectedId: number | null;
  setSelectedId: (id: number | null) => void;
  /** Full multi-selection (always includes selectedId when non-null). */
  selectedIds: number[];
  /** Set the whole selection + primary in one update (multi-select clicks). */
  setSelection: (ids: number[], primary: number | null) => void;
  thumbSize: number;
  setThumbSize: (n: number) => void;
  paletteOpen: boolean;
  setPaletteOpen: (b: boolean) => void;
  toast: string | null;
  setToast: (t: string | null) => void;
  gridMode: "grid" | "loupe";
  setGridMode: (m: "grid" | "loupe") => void;
  /** Images queued in the Export modal; null = modal closed. */
  exportTargets: ExportTarget[] | null;
  setExportTargets: (t: ExportTarget[] | null) => void;
  /** Image ids (+ originating detect-group, if any) queued for the Panorama merge modal; null =
   *  modal closed. `detectGroupId` rides along here (rather than a separate ambient store field) so
   *  it's threaded through the actual `panorama_merge` IPC call and echoed back on `panorama:done` —
   *  no stale field to mis-attribute a later manual merge to an earlier detected group. */
  panoramaSources: { ids: number[]; detectGroupId: number | null } | null;
  setPanoramaSources: (
    v: { ids: number[]; detectGroupId: number | null } | null,
  ) => void;
  /** Active panorama merge job (current backend phase); null = no merge in flight. Drives the
   *  PanoramaPill; fed by the `panorama:progress` event, cleared on `panorama:done`/`panorama:error`. */
  panoramaJob: { phase: string } | null;
  setPanoramaJob: (job: { phase: string } | null) => void;
  /** Panorama-suggestions review overlay open state (see `views/Panorama/PanoSuggestions.tsx`). */
  panoSuggestOpen: boolean;
  setPanoSuggestOpen: (b: boolean) => void;
  /** Panorama-detection scan state: running flag, suggested-group count, the group list, and a
   *  fetch-in-flight flag. Lifted here (rather than per-hook-instance state) so `LibraryView`,
   *  `LeftNav`, and `PanoSuggestions` — every `usePanoDetect()` call site — see one consistent
   *  picture regardless of mount order; the module-level singleton listeners in
   *  `lib/usePanoDetect.ts` are the sole writers besides the actions it exposes. */
  panoDetect: PanoDetectStoreState;
  setPanoDetect: (patch: Partial<PanoDetectStoreState>) => void;
  /** AI Models manager modal open state. */
  modelManagerOpen: boolean;
  setModelManagerOpen: (b: boolean) => void;
  // Library action callbacks registered by LibraryView
  onImport: (() => void) | null;
  setOnImport: (fn: (() => void) | null) => void;
  onMergeHdr: (() => void) | null;
  setOnMergeHdr: (fn: (() => void) | null) => void;
  onOpenSettings: (() => void) | null;
  setOnOpenSettings: (fn: (() => void) | null) => void;
  onSearch: ((query: string) => void) | null;
  setOnSearch: (fn: ((query: string) => void) | null) => void;
  onDevelopReset: (() => void) | null;
  setOnDevelopReset: (fn: (() => void) | null) => void;
  // Develop preset / settings callbacks registered by DevelopView (for the command palette).
  onSavePreset: (() => void) | null;
  setOnSavePreset: (fn: (() => void) | null) => void;
  onCopySettings: (() => void) | null;
  setOnCopySettings: (fn: (() => void) | null) => void;
  onPasteSettings: (() => void) | null;
  setOnPasteSettings: (fn: (() => void) | null) => void;
}

export const useAppStore = create<AppState>((set) => ({
  view: "library",
  setView: (v) => set({ view: v }),
  libraryImages: [],
  setLibraryImages: (rows) => set({ libraryImages: rows }),
  setImageEdited: (id, editedAt) =>
    set((s) => ({
      libraryImages: s.libraryImages.map((r) =>
        r.id === id ? { ...r, editedAt } : r,
      ),
    })),
  thumbVersions: {},
  bumpThumbVersions: (ids) =>
    set((s) => {
      if (ids.length === 0) return {};
      const next = { ...s.thumbVersions };
      for (const id of ids) next[id] = (next[id] ?? 0) + 1;
      return { thumbVersions: next };
    }),
  selectedId: null,
  setSelectedId: (id) =>
    set({ selectedId: id, selectedIds: id == null ? [] : [id] }),
  selectedIds: [],
  setSelection: (ids, primary) =>
    set({ selectedIds: ids, selectedId: primary }),
  thumbSize: 150,
  setThumbSize: (n) => set({ thumbSize: n }),
  paletteOpen: false,
  setPaletteOpen: (b) => set({ paletteOpen: b }),
  toast: null,
  setToast: (t) => set({ toast: t }),
  gridMode: "grid",
  setGridMode: (m) => set({ gridMode: m }),
  exportTargets: null,
  setExportTargets: (t) => set({ exportTargets: t }),
  panoramaSources: null,
  setPanoramaSources: (v) => set({ panoramaSources: v }),
  panoramaJob: null,
  setPanoramaJob: (job) => set({ panoramaJob: job }),
  panoSuggestOpen: false,
  setPanoSuggestOpen: (b) => set({ panoSuggestOpen: b }),
  panoDetect: { running: false, suggested: 0, groups: [], loading: false, progress: null },
  setPanoDetect: (patch) =>
    set((s) => ({ panoDetect: { ...s.panoDetect, ...patch } })),
  modelManagerOpen: false,
  setModelManagerOpen: (b) => set({ modelManagerOpen: b }),
  onImport: null,
  setOnImport: (fn) => set({ onImport: fn }),
  onMergeHdr: null,
  setOnMergeHdr: (fn) => set({ onMergeHdr: fn }),
  onOpenSettings: null,
  setOnOpenSettings: (fn) => set({ onOpenSettings: fn }),
  onSearch: null,
  setOnSearch: (fn) => set({ onSearch: fn }),
  onDevelopReset: null,
  setOnDevelopReset: (fn) => set({ onDevelopReset: fn }),
  onSavePreset: null,
  setOnSavePreset: (fn) => set({ onSavePreset: fn }),
  onCopySettings: null,
  setOnCopySettings: (fn) => set({ onCopySettings: fn }),
  onPasteSettings: null,
  setOnPasteSettings: (fn) => set({ onPasteSettings: fn }),
}));
