import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { useAppStore } from "../store/app";

interface EditChanged {
  imageId: number;
  /** Edit version (`updated_at`) for cache-busting previews, or null when the edit was cleared. */
  editedAt: number | null;
}

interface ThumbRendered {
  imageId: number;
  hash: string;
}

/** Coalesce window for `thumb:rendered` bursts (the startup backfill renders many in a row) so the
 *  grid re-renders once per batch instead of once per thumbnail. */
const COALESCE_MS = 150;

/**
 * Keeps thumbnails live across the app:
 * - `develop:edit-changed` (Develop edit settle) → patch the image's edit version so the
 *   filmstrip/chrome swap to the new versioned `thumb://` URL.
 * - `thumb:rendered` (background canonical/edited render landed) → bump the image's cache-bust token
 *   so the immutable-cached `<img>` refetches the fresh render (placeholder → canonical swap).
 *
 * Both bump `thumbVersions`, which the grid/filmstrip/loupe append to their `thumb://` URLs.
 */
export function useEditSync() {
  const setImageEdited = useAppStore((s) => s.setImageEdited);
  const bumpThumbVersions = useAppStore((s) => s.bumpThumbVersions);
  useEffect(() => {
    let active = true;
    const pending = new Set<number>();
    let timer: ReturnType<typeof setTimeout> | null = null;
    const flush = () => {
      timer = null;
      if (!active || pending.size === 0) return;
      const ids = [...pending];
      pending.clear();
      bumpThumbVersions(ids);
    };
    const schedule = (id: number) => {
      pending.add(id);
      if (timer === null) timer = setTimeout(flush, COALESCE_MS);
    };

    const unEdit = listen<EditChanged>("develop:edit-changed", (ev) => {
      if (!active) return;
      setImageEdited(ev.payload.imageId, ev.payload.editedAt);
      schedule(ev.payload.imageId);
    });
    const unThumb = listen<ThumbRendered>("thumb:rendered", (ev) => {
      if (active) schedule(ev.payload.imageId);
    });
    return () => {
      active = false;
      if (timer !== null) clearTimeout(timer);
      void unEdit.then((fn) => fn());
      void unThumb.then((fn) => fn());
    };
  }, [setImageEdited, bumpThumbVersions]);
}
