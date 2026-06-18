import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { useAppStore } from "../store/app";

interface EditChanged {
  imageId: number;
  /** Edit version (`updated_at`) for cache-busting previews, or null when the edit was cleared. */
  editedAt: number | null;
}

/**
 * Keeps edit-aware previews live: when an edit is regenerated (Develop edit settle), the backend
 * emits `develop:edit-changed`; we patch the image's edit version in the shared store so the
 * filmstrip (and any chrome reading `libraryImages`) swaps to the new versioned `thumb://` URL.
 * The library grid refreshes on its own when LibraryView re-mounts (its query returns `editedAt`).
 */
export function useEditSync() {
  const setImageEdited = useAppStore((s) => s.setImageEdited);
  useEffect(() => {
    let active = true;
    const un = listen<EditChanged>("develop:edit-changed", (ev) => {
      if (active) setImageEdited(ev.payload.imageId, ev.payload.editedAt);
    });
    return () => {
      active = false;
      void un.then((fn) => fn());
    };
  }, [setImageEdited]);
}
