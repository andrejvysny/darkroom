import { useEffect, useCallback, useState, useRef } from "react";
import { useAppStore } from "../../store/app";
import { useLibrary } from "../../lib/useLibrary";
import {
  thumbUrl,
  cullSetRating,
  cullSetFlag,
  cullSetLabel,
  cullSetRatingMany,
  cullSetFlagMany,
  cullSetLabelMany,
  keywordsForImage,
  keywordAddToImage,
  keywordAddToImages,
  keywordRemoveFromImage,
  keywordDelete,
  collectionsForImage,
  collectionAddImages,
  collectionRemoveImages,
  collectionCreate,
  collectionDelete,
  collectionRename,
  smartQueryFromParams,
} from "../../lib/ipc";
import type {
  ImageRow,
  KeywordRow,
  CollectionRow,
  ImportMode,
} from "../../lib/ipc";
import { useCulling } from "../../hooks/useCulling";
import { runImport } from "../../lib/importFlow";
import { runBatchExport } from "../../lib/export";
import LeftNav from "./LeftNav";
import ThumbGrid, { GridImage, SelectMods } from "./ThumbGrid";
import RightInfo, { RightInfoHandlers } from "./RightInfo";
import BottomBar from "./BottomBar";
import SelectionBar from "./SelectionBar";
import Loupe from "./Loupe";
import DedupModal from "./DedupModal";
import ImportModal from "./ImportModal";
import SettingsModal from "./SettingsModal";

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
  const selectedIds = useAppStore((s) => s.selectedIds);
  const setSelection = useAppStore((s) => s.setSelection);
  const gridMode = useAppStore((s) => s.gridMode);
  const setGridMode = useAppStore((s) => s.setGridMode);
  const setLibraryImages = useAppStore((s) => s.setLibraryImages);
  const setOnImport = useAppStore((s) => s.setOnImport);
  const setOnOpenDedup = useAppStore((s) => s.setOnOpenDedup);
  const setOnOpenSettings = useAppStore((s) => s.setOnOpenSettings);
  const setOnSearch = useAppStore((s) => s.setOnSearch);
  const [dedupOpen, setDedupOpen] = useState(false);
  const [importOpen, setImportOpen] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [selectedKeywords, setSelectedKeywords] = useState<KeywordRow[]>([]);
  const [selectedCollections, setSelectedCollections] = useState<
    CollectionRow[]
  >([]);

  const lib = useLibrary();

  // Register library action callbacks so TopBar/CommandPalette can call them
  useEffect(() => {
    setOnImport(() => setImportOpen(true));
    setOnOpenDedup(() => setDedupOpen(true));
    setOnOpenSettings(() => setSettingsOpen(true));
    setOnSearch((q: string) => lib.setSearch(q.trim() ? q.trim() : null));
    return () => {
      setOnImport(null);
      setOnOpenDedup(null);
      setOnOpenSettings(null);
      setOnSearch(null);
    };
  }, [
    lib.setSearch,
    setOnImport,
    setOnOpenDedup,
    setOnOpenSettings,
    setOnSearch,
  ]);

  // Keep a valid primary selection: if it's unset or no longer in the current (filtered) set,
  // fall back to the first visible image.
  useEffect(() => {
    if (
      lib.images.length > 0 &&
      !lib.images.some((img) => img.id === selectedId)
    ) {
      setSelectedId(lib.images[0].id);
    }
  }, [lib.images, selectedId, setSelectedId]);

  // Share the current image set so Develop's filmstrip/chrome can read it after this view unmounts.
  useEffect(() => {
    setLibraryImages(lib.images);
  }, [lib.images, setLibraryImages]);

  useCulling({ images: lib.images, patchImage: lib.patchImage });

  // Load the selected image's keywords + collection membership when the selection changes.
  useEffect(() => {
    if (selectedId === null) {
      setSelectedKeywords([]);
      setSelectedCollections([]);
      return;
    }
    let cancelled = false;
    void Promise.all([
      keywordsForImage(selectedId),
      collectionsForImage(selectedId),
    ])
      .then(([ks, cs]) => {
        if (!cancelled) {
          setSelectedKeywords(ks);
          setSelectedCollections(cs);
        }
      })
      .catch(() => {
        if (!cancelled) {
          setSelectedKeywords([]);
          setSelectedCollections([]);
        }
      });
    return () => {
      cancelled = true;
    };
  }, [selectedId]);

  const selectedImage = lib.images.find((img) => img.id === selectedId) ?? null;

  const handleAddKeyword = useCallback(
    async (name: string) => {
      if (selectedId === null) return;
      try {
        const kw = await keywordAddToImage(selectedId, name);
        setSelectedKeywords((prev) =>
          prev.some((k) => k.id === kw.id)
            ? prev
            : [...prev, kw].sort((a, b) => a.name.localeCompare(b.name)),
        );
        void lib.reloadKeywords();
      } catch {
        /* ignore — duplicate/empty names are no-ops */
      }
    },
    [selectedId, lib.reloadKeywords],
  );

  const handleRemoveKeyword = useCallback(
    async (keywordId: number) => {
      if (selectedId === null) return;
      try {
        await keywordRemoveFromImage(selectedId, keywordId);
        setSelectedKeywords((prev) => prev.filter((k) => k.id !== keywordId));
        void lib.reloadKeywords();
        // Drop the image from the grid if it no longer matches a keyword filter.
        if (lib.params.keywordId === keywordId) void lib.refresh();
      } catch {
        /* ignore */
      }
    },
    [selectedId, lib.reloadKeywords, lib.refresh, lib.params.keywordId],
  );

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

  const handleAddToCollection = useCallback(
    async (collectionId: number) => {
      if (selectedId === null) return;
      try {
        await collectionAddImages(collectionId, [selectedId]);
        setSelectedCollections(await collectionsForImage(selectedId));
        void lib.reloadCollections();
      } catch {
        /* ignore */
      }
    },
    [selectedId, lib.reloadCollections],
  );

  const handleRemoveFromCollection = useCallback(
    async (collectionId: number) => {
      if (selectedId === null) return;
      try {
        await collectionRemoveImages(collectionId, [selectedId]);
        setSelectedCollections((prev) =>
          prev.filter((c) => c.id !== collectionId),
        );
        void lib.reloadCollections();
        if (lib.params.collectionId === collectionId) void lib.refresh();
      } catch {
        /* ignore */
      }
    },
    [selectedId, lib.reloadCollections, lib.refresh, lib.params.collectionId],
  );

  // LeftNav collection management
  const handleCreateCollection = useCallback(
    async (name: string) => {
      try {
        await collectionCreate(name, false, null);
        void lib.reloadCollections();
      } catch {
        /* ignore — empty names are rejected backend-side */
      }
    },
    [lib.reloadCollections],
  );

  const handleCreateSmartCollection = useCallback(
    async (name: string) => {
      try {
        await collectionCreate(name, true, smartQueryFromParams(lib.params));
        void lib.reloadCollections();
      } catch {
        /* ignore */
      }
    },
    [lib.reloadCollections, lib.params],
  );

  const handleDeleteCollection = useCallback(
    async (id: number) => {
      try {
        await collectionDelete(id);
        void lib.reloadCollections();
        if (lib.params.collectionId === id) lib.clearFilters();
      } catch {
        /* ignore */
      }
    },
    [lib.reloadCollections, lib.clearFilters, lib.params.collectionId],
  );

  const handleRenameCollection = useCallback(
    async (id: number, name: string) => {
      try {
        await collectionRename(id, name);
        void lib.reloadCollections();
      } catch {
        /* ignore — empty names are rejected backend-side */
      }
    },
    [lib.reloadCollections],
  );

  const handleDeleteKeyword = useCallback(
    async (id: number) => {
      try {
        await keywordDelete(id);
        void lib.reloadKeywords();
        // Clear a keyword filter that no longer exists, and drop its chip from the panel.
        if (lib.params.keywordId === id) lib.patchParams({ keywordId: null });
        setSelectedKeywords((prev) => prev.filter((k) => k.id !== id));
      } catch {
        /* ignore */
      }
    },
    [lib.reloadKeywords, lib.patchParams, lib.params.keywordId],
  );

  // ---- Multi-select ----
  // Fixed range anchor: set by plain/cmd click, NOT moved by shift-click, so a shift range can be
  // grown/shrunk from a stable pivot (Finder/Lightroom semantics).
  const anchorRef = useRef<number | null>(null);
  const handleSelect = useCallback(
    (id: number, mods: SelectMods) => {
      const order = lib.images.map((i) => i.id);
      if (mods.shift) {
        const anchor = anchorRef.current ?? selectedId;
        const a = anchor != null ? order.indexOf(anchor) : -1;
        const b = order.indexOf(id);
        if (a !== -1 && b !== -1) {
          const [lo, hi] = a <= b ? [a, b] : [b, a];
          setSelection(order.slice(lo, hi + 1), id);
          return; // anchor stays put
        }
      }
      if (mods.meta) {
        const set = new Set(selectedIds);
        if (set.has(id)) set.delete(id);
        else set.add(id);
        const next = order.filter((x) => set.has(x)); // preserve grid order
        const primary = set.has(id) ? id : (next[next.length - 1] ?? null);
        anchorRef.current = id;
        setSelection(next, primary);
        return;
      }
      anchorRef.current = id;
      setSelectedId(id);
    },
    [lib.images, selectedId, selectedIds, setSelection, setSelectedId],
  );

  // ---- Batch operations (act on the whole selection) ----
  const batchRate = useCallback(
    (stars: number) => {
      if (selectedIds.length === 0) return;
      selectedIds.forEach((id) => lib.patchImage(id, { stars }));
      void cullSetRatingMany(selectedIds, stars);
    },
    [selectedIds, lib.patchImage],
  );

  const batchFlag = useCallback(
    (flag: "none" | "pick" | "reject") => {
      if (selectedIds.length === 0) return;
      selectedIds.forEach((id) => lib.patchImage(id, { flag }));
      void cullSetFlagMany(selectedIds, flag);
    },
    [selectedIds, lib.patchImage],
  );

  const batchLabel = useCallback(
    (label: string | null) => {
      if (selectedIds.length === 0) return;
      selectedIds.forEach((id) => lib.patchImage(id, { colorLabel: label }));
      void cullSetLabelMany(selectedIds, label);
    },
    [selectedIds, lib.patchImage],
  );

  const batchAddKeyword = useCallback(
    async (name: string) => {
      if (selectedIds.length === 0) return;
      try {
        await keywordAddToImages(selectedIds, name);
        void lib.reloadKeywords();
        if (selectedId != null)
          setSelectedKeywords(await keywordsForImage(selectedId));
      } catch {
        /* ignore */
      }
    },
    [selectedIds, selectedId, lib.reloadKeywords],
  );

  const batchAddToCollection = useCallback(
    async (collectionId: number) => {
      if (selectedIds.length === 0) return;
      try {
        await collectionAddImages(collectionId, selectedIds);
        void lib.reloadCollections();
        if (selectedId != null)
          setSelectedCollections(await collectionsForImage(selectedId));
      } catch {
        /* ignore */
      }
    },
    [selectedIds, selectedId, lib.reloadCollections],
  );

  const batchExport = useCallback(() => {
    const items = lib.images
      .filter((i) => selectedIds.includes(i.id))
      .map((i) => ({ id: i.id, filename: i.filename }));
    void runBatchExport(items);
  }, [lib.images, selectedIds]);

  const collapseSelection = useCallback(() => {
    setSelectedId(selectedId);
  }, [selectedId, setSelectedId]);

  const rightInfoHandlers: RightInfoHandlers = {
    onSetRating: handleSetRating,
    onSetFlag: handleSetFlag,
    onSetLabel: handleSetLabel,
    onAddKeyword: handleAddKeyword,
    onRemoveKeyword: handleRemoveKeyword,
    onAddToCollection: handleAddToCollection,
    onRemoveFromCollection: handleRemoveFromCollection,
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
          keywords={lib.keywords}
          collections={lib.collections}
          grandTotal={lib.grandTotal}
          params={lib.params}
          clearFilters={lib.clearFilters}
          patchParams={lib.patchParams}
          setSort={lib.setSort}
          onCreateCollection={handleCreateCollection}
          onCreateSmartCollection={handleCreateSmartCollection}
          onDeleteCollection={handleDeleteCollection}
          onRenameCollection={handleRenameCollection}
          onDeleteKeyword={handleDeleteKeyword}
        />
      </div>

      {/* Center: optional selection bar + grid or loupe */}
      <div
        style={{
          gridRow: "1",
          gridColumn: "2",
          minHeight: 0,
          display: "flex",
          flexDirection: "column",
        }}
      >
        {selectedIds.length > 1 && (
          <SelectionBar
            count={selectedIds.length}
            collections={lib.collections}
            onRate={batchRate}
            onFlag={batchFlag}
            onLabel={batchLabel}
            onAddKeyword={batchAddKeyword}
            onAddToCollection={batchAddToCollection}
            onExport={batchExport}
            onClear={collapseSelection}
          />
        )}
        <div style={{ flex: 1, minHeight: 0, position: "relative" }}>
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
                selectedIds={selectedIds}
                onSelect={handleSelect}
                onActivate={(id) => {
                  setSelectedId(id);
                  setGridMode("loupe");
                }}
                onLoadMore={lib.loadMore}
              />
            </>
          )}
        </div>
      </div>

      {/* Right info spans both rows */}
      <div style={{ gridRow: "1 / 3", gridColumn: "3", minHeight: 0 }}>
        <RightInfo
          selectedImage={selectedImage}
          keywords={selectedKeywords}
          keywordSuggestions={lib.keywords}
          imageCollections={selectedCollections}
          allCollections={lib.collections}
          handlers={rightInfoHandlers}
        />
      </div>

      {/* Bottom bar under center only */}
      <div style={{ gridRow: "2", gridColumn: "2" }}>
        <BottomBar
          total={lib.total}
          params={lib.params}
          patchParams={lib.patchParams}
          clearFilters={lib.clearFilters}
          setSort={lib.setSort}
        />
      </div>

      <DedupModal
        open={dedupOpen}
        onClose={() => setDedupOpen(false)}
        onRefresh={() => void lib.refresh()}
      />

      <ImportModal
        open={importOpen}
        onClose={() => setImportOpen(false)}
        onChoose={(mode: ImportMode) => {
          setImportOpen(false);
          void runImport(mode, () => void lib.refresh());
        }}
      />

      <SettingsModal
        open={settingsOpen}
        onClose={() => setSettingsOpen(false)}
      />
    </div>
  );
}
