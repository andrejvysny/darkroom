import { useEffect, useCallback, useRef } from "react";
import { useAppStore } from "../store/app";
import {
  cullSetRating,
  cullSetFlag,
  cullSetRatingMany,
  cullSetFlagMany,
} from "../lib/ipc";
import type { ImageRow } from "../lib/ipc";

interface UseCullingParams {
  images: ImageRow[];
  patchImage: (id: number, patch: Partial<ImageRow>) => void;
}

export function useCulling({ images, patchImage }: UseCullingParams) {
  const selectedId = useAppStore((s) => s.selectedId);
  const setSelectedId = useAppStore((s) => s.setSelectedId);
  const view = useAppStore((s) => s.view);

  // When the current image was selected — decision latency = time on image before a cull action
  // (a cheap implicit confidence weight for the behavioral log).
  const selectedAt = useRef<number>(Date.now());
  useEffect(() => {
    selectedAt.current = Date.now();
  }, [selectedId]);

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
      const ids = useAppStore.getState().selectedIds;
      const multi = ids.length > 1;

      // Arrow navigation / bracket navigation (collapses any multi-selection to one).
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
        if (multi) {
          ids.forEach((i) => patchImage(i, { stars }));
          void cullSetRatingMany(ids, stars);
        } else {
          patchImage(id, { stars });
          void cullSetRating(id, stars, {
            latencyMs: Date.now() - selectedAt.current,
          });
          advanceSelection(id);
        }
        return;
      }

      // Flag: p=pick, x/r=reject, u=none
      if (
        (e.key === "p" || e.key === "x" || e.key === "r" || e.key === "u") &&
        !e.metaKey &&
        !e.ctrlKey
      ) {
        const flag =
          e.key === "p"
            ? "pick"
            : e.key === "x" || e.key === "r"
              ? "reject"
              : "none";
        if (multi) {
          ids.forEach((i) => patchImage(i, { flag }));
          void cullSetFlagMany(ids, flag);
        } else {
          patchImage(id, { flag });
          void cullSetFlag(id, flag, {
            latencyMs: Date.now() - selectedAt.current,
          });
          advanceSelection(id);
        }
        return;
      }
    }

    document.addEventListener("keydown", handler);
    return () => document.removeEventListener("keydown", handler);
  }, [view, moveSelection, advanceSelection, patchImage]);
}
