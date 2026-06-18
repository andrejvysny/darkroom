import { create } from "zustand";
import type { ImageRow } from "../lib/ipc";

interface AppState {
  view: "library" | "develop";
  setView: (v: "library" | "develop") => void;
  /** The current (filtered) library image set — shared so Develop's filmstrip/chrome can read it
   *  even while the LibraryView is unmounted. */
  libraryImages: ImageRow[];
  setLibraryImages: (rows: ImageRow[]) => void;
  /** Update one image's edit version (drives live edit-aware previews in the filmstrip/chrome). */
  setImageEdited: (id: number, editedAt: number | null) => void;
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
  // Library action callbacks registered by LibraryView
  onImport: (() => void) | null;
  setOnImport: (fn: (() => void) | null) => void;
  onOpenDedup: (() => void) | null;
  setOnOpenDedup: (fn: (() => void) | null) => void;
  onOpenSettings: (() => void) | null;
  setOnOpenSettings: (fn: (() => void) | null) => void;
  onSearch: ((query: string) => void) | null;
  setOnSearch: (fn: ((query: string) => void) | null) => void;
  onDevelopReset: (() => void) | null;
  setOnDevelopReset: (fn: (() => void) | null) => void;
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
  selectedId: 6,
  setSelectedId: (id) =>
    set({ selectedId: id, selectedIds: id == null ? [] : [id] }),
  selectedIds: [6],
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
  onImport: null,
  setOnImport: (fn) => set({ onImport: fn }),
  onOpenDedup: null,
  setOnOpenDedup: (fn) => set({ onOpenDedup: fn }),
  onOpenSettings: null,
  setOnOpenSettings: (fn) => set({ onOpenSettings: fn }),
  onSearch: null,
  setOnSearch: (fn) => set({ onSearch: fn }),
  onDevelopReset: null,
  setOnDevelopReset: (fn) => set({ onDevelopReset: fn }),
}));
