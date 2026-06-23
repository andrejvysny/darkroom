import { create } from "zustand";
import type { DupGroup, DupImage } from "../lib/ipc";

/** One photo marked for deletion but not yet trashed — held in the Bin until the user empties it.
 *  Emptying the bin calls `dedupResolve` per group, so detection/resolve IPCs stay unchanged. */
export interface StagedItem {
  image: DupImage;
  groupKey: string;
  category: string;
}

interface DedupState {
  groups: DupGroup[];
  /** Per-group chosen keeper (image id). */
  keepers: Record<string, number>;
  /** Groups the user accepted (non-keepers staged) — drives the "reviewed" status in the overview. */
  resolved: Record<string, boolean>;
  /** Photos staged for deletion, keyed by image id. Source of truth for "rejected". */
  staged: Record<number, StagedItem>;
  /** True after the first byte+capture scan completes — prevents auto-rescan on remount. */
  initialScanDone: boolean;

  setGroups: (groups: DupGroup[], keepers: Record<string, number>) => void;
  mergeGroups: (
    added: DupGroup[],
    addedKeepers: Record<string, number>,
  ) => void;
  setKeeper: (groupKey: string, imageId: number) => void;
  removeGroup: (key: string) => void;
  setInitialScanDone: (v: boolean) => void;

  // ── Staging / Bin ──
  setResolved: (groupKey: string, v: boolean) => void;
  stage: (items: StagedItem[]) => void;
  unstage: (imageIds: number[]) => void;
  clearStaged: () => void;
  /** After real trashing: drop image ids from every group, removing groups left with <2 images. */
  pruneTrashed: (imageIds: number[]) => void;
  /** Update one image's star rating in-place (after a `cullSetRating` IPC). */
  setStars: (imageId: number, stars: number) => void;
  /** Restore an undo snapshot of the mutable review state. */
  restore: (snap: {
    keepers: Record<string, number>;
    resolved: Record<string, boolean>;
    staged: Record<number, StagedItem>;
  }) => void;
}

export const useDedupStore = create<DedupState>((set) => ({
  groups: [],
  keepers: {},
  resolved: {},
  staged: {},
  initialScanDone: false,

  setGroups: (groups, keepers) =>
    set({ groups, keepers, resolved: {}, staged: {} }),

  mergeGroups: (added, addedKeepers) =>
    set((s) => {
      const seen = new Set(s.groups.map((g) => g.key));
      const fresh = added.filter((g) => !seen.has(g.key));
      if (fresh.length === 0) return {};
      return {
        groups: [...s.groups, ...fresh],
        keepers: { ...s.keepers, ...addedKeepers },
      };
    }),

  setKeeper: (groupKey, imageId) =>
    set((s) => ({ keepers: { ...s.keepers, [groupKey]: imageId } })),

  removeGroup: (key) =>
    set((s) => {
      const groups = s.groups.filter((g) => g.key !== key);
      const keepers = { ...s.keepers };
      const resolved = { ...s.resolved };
      delete keepers[key];
      delete resolved[key];
      const staged = { ...s.staged };
      for (const [id, item] of Object.entries(staged)) {
        if (item.groupKey === key) delete staged[Number(id)];
      }
      return { groups, keepers, resolved, staged };
    }),

  setInitialScanDone: (v) => set({ initialScanDone: v }),

  setResolved: (groupKey, v) =>
    set((s) => ({ resolved: { ...s.resolved, [groupKey]: v } })),

  stage: (items) =>
    set((s) => {
      const staged = { ...s.staged };
      for (const it of items) staged[it.image.id] = it;
      return { staged };
    }),

  unstage: (imageIds) =>
    set((s) => {
      const staged = { ...s.staged };
      for (const id of imageIds) delete staged[id];
      return { staged };
    }),

  clearStaged: () => set({ staged: {} }),

  pruneTrashed: (imageIds) =>
    set((s) => {
      const gone = new Set(imageIds);
      const groups: DupGroup[] = [];
      const keepers = { ...s.keepers };
      const resolved = { ...s.resolved };
      for (const g of s.groups) {
        const images = g.images.filter((i) => !gone.has(i.id));
        if (images.length < 2) {
          delete keepers[g.key];
          delete resolved[g.key];
          continue;
        }
        groups.push(images.length === g.images.length ? g : { ...g, images });
      }
      const staged = { ...s.staged };
      for (const id of imageIds) delete staged[id];
      return { groups, keepers, resolved, staged };
    }),

  setStars: (imageId, stars) =>
    set((s) => {
      const groups = s.groups.map((g) =>
        g.images.some((i) => i.id === imageId)
          ? {
              ...g,
              images: g.images.map((i) =>
                i.id === imageId ? { ...i, stars } : i,
              ),
            }
          : g,
      );
      const staged = { ...s.staged };
      if (staged[imageId]) {
        staged[imageId] = {
          ...staged[imageId],
          image: { ...staged[imageId].image, stars },
        };
      }
      return { groups, staged };
    }),

  restore: (snap) => set(snap),
}));
