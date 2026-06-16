import { useEffect, useCallback } from "react";
import { useAppStore } from "../store/app";
import { cullSetRating, cullSetFlag } from "../lib/ipc";
import type { ImageRow } from "../lib/ipc";

interface UseCullingParams {
  images: ImageRow[];
  patchImage: (id: number, patch: Partial<ImageRow>) => void;
}

export function useCulling({ images, patchImage }: UseCullingParams) {
  const selectedId = useAppStore((s) => s.selectedId);
  const setSelectedId = useAppStore((s) => s.setSelectedId);
  const view = useAppStore((s) => s.view);

  const advanceSelection = useCallback(
    (fromId: number | null) => {
      if (images.length === 0) return;
      const idx = images.findIndex((img) => img.id === fromId);
      if (idx === -1) return;
      // Clamp at last image instead of wrapping
      const nextIdx = Math.min(idx + 1, images.length - 1);
      setSelectedId(images[nextIdx].id);
    },
    [images, setSelectedId],
  );

  const moveSelection = useCallback(
    (dir: -1 | 1) => {
      if (images.length === 0) return;
      const idx = images.findIndex((img) => img.id === selectedId);
      if (idx === -1) return;
      const nextIdx = Math.max(0, Math.min(images.length - 1, idx + dir));
      setSelectedId(images[nextIdx].id);
    },
    [images, selectedId, setSelectedId],
  );

  useEffect(() => {
    if (view !== "library") return;

    function handler(e: KeyboardEvent) {
      const tag = (e.target as HTMLElement | null)?.tagName;
      const typing = tag === "INPUT" || tag === "TEXTAREA";
      if (typing) return;

      const id = useAppStore.getState().selectedId;
      if (id === null) return;

      // Arrow navigation / bracket navigation
      if (e.key === "ArrowLeft" || e.key === "[") {
        e.preventDefault();
        moveSelection(-1);
        return;
      }
      if (e.key === "ArrowRight" || e.key === "]") {
        e.preventDefault();
        moveSelection(1);
        return;
      }

      // Rating: digits 0–5
      if (/^[0-5]$/.test(e.key) && !e.metaKey && !e.ctrlKey) {
        const stars = parseInt(e.key, 10);
        patchImage(id, { stars });
        void cullSetRating(id, stars);
        advanceSelection(id);
        return;
      }

      // Flag: p=pick, x=reject, u=none
      if (e.key === "p" && !e.metaKey && !e.ctrlKey) {
        patchImage(id, { flag: "pick" });
        void cullSetFlag(id, "pick");
        advanceSelection(id);
        return;
      }
      if (e.key === "x" && !e.metaKey && !e.ctrlKey) {
        patchImage(id, { flag: "reject" });
        void cullSetFlag(id, "reject");
        advanceSelection(id);
        return;
      }
      if (e.key === "u" && !e.metaKey && !e.ctrlKey) {
        patchImage(id, { flag: "none" });
        void cullSetFlag(id, "none");
        advanceSelection(id);
        return;
      }
    }

    document.addEventListener("keydown", handler);
    return () => document.removeEventListener("keydown", handler);
  }, [view, moveSelection, advanceSelection, patchImage]);
}
