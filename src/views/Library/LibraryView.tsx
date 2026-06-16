import { useEffect, useCallback, useState } from "react";
import { useAppStore } from "../../store/app";
import { useLibrary } from "../../lib/useLibrary";
import {
  thumbUrl,
  cullSetRating,
  cullSetFlag,
  cullSetLabel,
} from "../../lib/ipc";
import type { ImageRow } from "../../lib/ipc";
import { useCulling } from "../../hooks/useCulling";
import { runImport } from "../../lib/importFlow";
import LeftNav from "./LeftNav";
import ThumbGrid, { GridImage } from "./ThumbGrid";
import RightInfo, { RightInfoHandlers } from "./RightInfo";
import BottomBar from "./BottomBar";
import Loupe from "./Loupe";
import DedupModal from "./DedupModal";

// Map color label name to CSS var for the dot color in ThumbGrid
const LABEL_COLOR_MAP: Record<string, string> = {
  red: "var(--color-lab-red)",
  yellow: "var(--color-lab-yellow)",
  green: "var(--color-lab-green)",
  blue: "var(--color-lab-blue)",
  purple: "var(--color-lab-purple)",
};

function toGridImage(r: ImageRow): GridImage {
  return {
    id: r.id,
    filename: r.filename,
    thumbUrl: thumbUrl(r.contentHash),
    stars: r.stars,
    flag: r.flag === "none" ? null : r.flag,
    label: r.colorLabel
      ? (LABEL_COLOR_MAP[r.colorLabel] ?? r.colorLabel)
      : undefined,
  };
}

export default function LibraryView() {
  const thumbSize = useAppStore((s) => s.thumbSize);
  const selectedId = useAppStore((s) => s.selectedId);
  const setSelectedId = useAppStore((s) => s.setSelectedId);
  const gridMode = useAppStore((s) => s.gridMode);
  const setOnImport = useAppStore((s) => s.setOnImport);
  const setOnOpenDedup = useAppStore((s) => s.setOnOpenDedup);
  const setOnSearch = useAppStore((s) => s.setOnSearch);
  const [dedupOpen, setDedupOpen] = useState(false);

  const lib = useLibrary();

  // Register library action callbacks so TopBar/CommandPalette can call them
  useEffect(() => {
    const handleImport = () => void runImport("copy", () => void lib.refresh());
    setOnImport(handleImport);
    setOnOpenDedup(() => setDedupOpen(true));
    setOnSearch((q: string) => lib.setSearch(q.trim() ? q.trim() : null));
    return () => {
      setOnImport(null);
      setOnOpenDedup(null);
      setOnSearch(null);
    };
  }, [lib.refresh, lib.setSearch, setOnImport, setOnOpenDedup, setOnSearch]);

  // Default selection to first image after load
  useEffect(() => {
    if (selectedId === null && lib.images.length > 0) {
      setSelectedId(lib.images[0].id);
    }
  }, [lib.images, selectedId, setSelectedId]);

  useCulling({ images: lib.images, patchImage: lib.patchImage });

  const selectedImage = lib.images.find((img) => img.id === selectedId) ?? null;

  const handleSetRating = useCallback(
    (stars: number) => {
      if (selectedId === null) return;
      lib.patchImage(selectedId, { stars });
      void cullSetRating(selectedId, stars);
    },
    [selectedId, lib.patchImage],
  );

  const handleSetFlag = useCallback(
    (flag: "none" | "pick" | "reject") => {
      if (selectedId === null) return;
      lib.patchImage(selectedId, { flag });
      void cullSetFlag(selectedId, flag);
    },
    [selectedId, lib.patchImage],
  );

  const handleSetLabel = useCallback(
    (label: string | null) => {
      if (selectedId === null) return;
      lib.patchImage(selectedId, { colorLabel: label });
      void cullSetLabel(selectedId, label);
    },
    [selectedId, lib.patchImage],
  );

  const rightInfoHandlers: RightInfoHandlers = {
    onSetRating: handleSetRating,
    onSetFlag: handleSetFlag,
    onSetLabel: handleSetLabel,
  };

  const gridImages = lib.images.map(toGridImage);

  return (
    <div
      style={{
        display: "grid",
        gridTemplateRows: "1fr 40px",
        gridTemplateColumns: "208px 1fr 264px",
        minHeight: 0,
        flex: 1,
      }}
    >
      {/* Left nav spans both rows */}
      <div style={{ gridRow: "1 / 3", gridColumn: "1", minHeight: 0 }}>
        <LeftNav
          folders={lib.folders}
          total={lib.total}
          params={lib.params}
          setFolderId={lib.setFolderId}
        />
      </div>

      {/* Center: grid or loupe */}
      <div
        style={{
          gridRow: "1",
          gridColumn: "2",
          minHeight: 0,
          position: "relative",
        }}
      >
        {lib.indexing && (
          <div
            style={{
              position: "absolute",
              top: 10,
              left: "50%",
              transform: "translateX(-50%)",
              zIndex: 10,
              background: "var(--color-elev)",
              border: "1px solid var(--color-line)",
              borderRadius: "var(--radius-sm)",
              padding: "6px 14px",
              fontSize: 12,
              color: "var(--color-t2)",
            }}
          >
            {lib.indexing.total > 0
              ? `Indexing ${lib.indexing.done} / ${lib.indexing.total}…`
              : "Indexing…"}
          </div>
        )}

        {gridMode === "loupe" && selectedImage !== null ? (
          <Loupe image={selectedImage} />
        ) : (
          <>
            {!lib.loading && !lib.indexing && gridImages.length === 0 && (
              <div
                style={{
                  display: "flex",
                  alignItems: "center",
                  justifyContent: "center",
                  height: "100%",
                  color: "var(--color-t3)",
                  fontSize: 13,
                }}
              >
                No photos found
              </div>
            )}
            <ThumbGrid
              images={gridImages}
              thumbSize={thumbSize}
              selectedId={selectedId}
              onSelect={setSelectedId}
            />
          </>
        )}
      </div>

      {/* Right info spans both rows */}
      <div style={{ gridRow: "1 / 3", gridColumn: "3", minHeight: 0 }}>
        <RightInfo selectedImage={selectedImage} handlers={rightInfoHandlers} />
      </div>

      {/* Bottom bar under center only */}
      <div style={{ gridRow: "2", gridColumn: "2" }}>
        <BottomBar
          total={lib.total}
          params={lib.params}
          setSort={lib.setSort}
          setMinStars={lib.setMinStars}
          setFlag={lib.setFlag}
        />
      </div>

      <DedupModal
        open={dedupOpen}
        onClose={() => setDedupOpen(false)}
        onRefresh={() => void lib.refresh()}
      />
    </div>
  );
}
