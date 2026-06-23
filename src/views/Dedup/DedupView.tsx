import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { useAppStore } from "../../store/app";
import { useDedupStore, type StagedItem } from "../../store/dedup";
import {
  cullSetRating,
  dedupResolve,
  dedupResolveBulk,
  dedupScan,
  dedupScanPerceptual,
  effectivePreviewEdge,
  thumbPrioritize,
  type DupGroup,
  type DupImage,
} from "../../lib/ipc";
import {
  defaultKeepers,
  keeperScore,
  sortGroups,
  suggestKeeper,
  type SortDir,
  type SortKey,
} from "./helpers";
import Overview from "./Overview";
import Review, { type ReviewMode } from "./Review";
import BinDrawer from "./BinDrawer";
import PreviewOverlay from "./PreviewOverlay";

/** Best non-keeper to challenge with: highest keeper-fit, preferring photos not staged for the bin. */
function nextChallenger(
  group: DupGroup,
  keeperId: number,
  isStaged: (id: number) => boolean,
): number {
  const others = group.images.filter((i) => i.id !== keeperId);
  if (others.length === 0) return keeperId;
  const live = others.filter((i) => !isStaged(i.id));
  const pool = live.length > 0 ? live : others;
  return pool.reduce((best, img) =>
    keeperScore(img, group) > keeperScore(best, group) ? img : best,
  ).id;
}

export default function DedupView() {
  const setToast = useAppStore((s) => s.setToast);
  // Cache-bust tokens bumped by `useEditSync` when a background preview/thumb render lands — appended
  // to `thumb://` URLs so the immutable-cached <img> refetches the now-sharp preview.
  const thumbVersions = useAppStore((s) => s.thumbVersions);
  const tokenOf = useCallback(
    (id: number) => thumbVersions[id],
    [thumbVersions],
  );

  const groups = useDedupStore((s) => s.groups);
  const keepers = useDedupStore((s) => s.keepers);
  const resolved = useDedupStore((s) => s.resolved);
  const staged = useDedupStore((s) => s.staged);
  const storeSetGroups = useDedupStore((s) => s.setGroups);
  const storeMergeGroups = useDedupStore((s) => s.mergeGroups);
  const storeSetKeeper = useDedupStore((s) => s.setKeeper);
  const storeSetResolved = useDedupStore((s) => s.setResolved);
  const storeStage = useDedupStore((s) => s.stage);
  const storeUnstage = useDedupStore((s) => s.unstage);
  const storePrune = useDedupStore((s) => s.pruneTrashed);
  const storeSetStars = useDedupStore((s) => s.setStars);
  const storeRestore = useDedupStore((s) => s.restore);
  const storeSetInitialScanDone = useDedupStore((s) => s.setInitialScanDone);

  const [loading, setLoading] = useState(false);
  const [threshold, setThreshold] = useState(10);
  const [scanningSimilar, setScanningSimilar] = useState(false);
  const [fillProgress, setFillProgress] = useState<{
    done: number;
    total: number;
  } | null>(null);

  const [mode, setMode] = useState<"groups" | "review">("groups");
  const [focusIdx, setFocusIdx] = useState(0);
  const [focusId, setFocusId] = useState(-1);
  const [reviewMode, setReviewMode] = useState<ReviewMode>("compare");
  const [previewImg, setPreviewImg] = useState<DupImage | null>(null);
  const [previewEdge, setPreviewEdge] = useState(2560);
  const [binOpen, setBinOpen] = useState(false);
  const [binBusy, setBinBusy] = useState(false);
  const [sortKey, setSortKey] = useState<SortKey>("reclaim");
  const [sortDir, setSortDir] = useState<SortDir>("desc");

  // Display/navigation order (store stays in scan order). Both the overview and review nav use this.
  const orderedGroups = useMemo(
    () => sortGroups(groups, keepers, sortKey, sortDir),
    [groups, keepers, sortKey, sortDir],
  );

  // Undo stack of mutable-state snapshots.
  const undoStack = useRef<
    {
      keepers: Record<string, number>;
      resolved: Record<string, boolean>;
      staged: Record<number, StagedItem>;
    }[]
  >([]);
  const [canUndo, setCanUndo] = useState(false);

  const isStaged = useCallback(
    (id: number) => !!useDedupStore.getState().staged[id],
    [],
  );

  const keeperOf = useCallback(
    (g: DupGroup) =>
      useDedupStore.getState().keepers[g.key] ?? suggestKeeper(g.images),
    [],
  );

  const pushUndo = useCallback(() => {
    const s = useDedupStore.getState();
    undoStack.current.push({
      keepers: { ...s.keepers },
      resolved: { ...s.resolved },
      staged: { ...s.staged },
    });
    if (undoStack.current.length > 40) undoStack.current.shift();
    setCanUndo(true);
  }, []);

  const undo = useCallback(() => {
    const snap = undoStack.current.pop();
    if (!snap) return;
    storeRestore(snap);
    setCanUndo(undoStack.current.length > 0);
  }, [storeRestore]);

  // ── Scanning (detection IPCs unchanged) ──
  const scan = useCallback(async () => {
    setLoading(true);
    try {
      const [byByte, byCapture] = await Promise.all([
        dedupScan("byte"),
        dedupScan("capture"),
      ]);
      const seen = new Set<string>();
      const merged: DupGroup[] = [];
      for (const g of [...byByte, ...byCapture]) {
        if (!seen.has(g.key)) {
          seen.add(g.key);
          merged.push(g);
        }
      }
      storeSetGroups(merged, defaultKeepers(merged));
      storeSetInitialScanDone(true);
      undoStack.current = [];
      setCanUndo(false);
    } catch (err) {
      setToast(`Scan failed: ${String(err)}`);
    } finally {
      setLoading(false);
    }
  }, [setToast, storeSetGroups, storeSetInitialScanDone]);

  const handleSimilarScan = useCallback(async () => {
    setScanningSimilar(true);
    setFillProgress(null);
    const unlisten = await listen<{ done: number; total: number }>(
      "dedup:progress",
      (ev) => setFillProgress(ev.payload),
    );
    try {
      const found = await dedupScanPerceptual(threshold);
      storeMergeGroups(found, defaultKeepers(found));
      if (found.length === 0) setToast("No similar photos found");
    } catch (err) {
      setToast(`Similar scan failed: ${String(err)}`);
    } finally {
      unlisten();
      setScanningSimilar(false);
      setFillProgress(null);
    }
  }, [threshold, storeMergeGroups, setToast]);

  const handleAutoResolve = useCallback(async () => {
    try {
      const count = await dedupResolveBulk();
      setToast(`Trashed ${count} exact duplicate${count === 1 ? "" : "s"}`);
      await scan();
    } catch (err) {
      setToast(`Auto-resolve failed: ${String(err)}`);
    }
  }, [setToast, scan]);

  // ── Mutations (staging layer over dedupResolve) ──
  const setKeeper = useCallback(
    (groupKey: string, id: number) => {
      pushUndo();
      storeSetKeeper(groupKey, id);
      storeUnstage([id]);
      const g = useDedupStore.getState().groups.find((x) => x.key === groupKey);
      if (g) setFocusId(nextChallenger(g, id, isStaged));
    },
    [pushUndo, storeSetKeeper, storeUnstage, isStaged],
  );

  const keep = useCallback(
    (id: number) => {
      if (!useDedupStore.getState().staged[id]) return;
      pushUndo();
      storeUnstage([id]);
    },
    [pushUndo, storeUnstage],
  );

  const reject = useCallback(
    (group: DupGroup, id: number) => {
      const st = useDedupStore.getState();
      const keeperId = st.keepers[group.key] ?? suggestKeeper(group.images);
      const liveAfter = group.images.filter(
        (i) => i.id !== id && !st.staged[i.id],
      );
      if (liveAfter.length === 0) {
        setToast("Keep at least one photo in this group");
        return;
      }
      pushUndo();
      const item: StagedItem = {
        image: group.images.find((i) => i.id === id)!,
        groupKey: group.key,
        category: group.category,
      };
      storeStage([item]);
      if (id === keeperId) {
        const promote = liveAfter.reduce((best, img) =>
          keeperScore(img, group) > keeperScore(best, group) ? img : best,
        );
        storeSetKeeper(group.key, promote.id);
        setToast(`Keeper moved to ${promote.filename}`);
      }
      const fresh = useDedupStore.getState();
      const newKeeper = fresh.keepers[group.key] ?? keeperId;
      setFocusId(nextChallenger(group, newKeeper, (x) => !!fresh.staged[x]));
    },
    [pushUndo, storeStage, storeSetKeeper, setToast],
  );

  const rate = useCallback(
    async (img: DupImage, n: number) => {
      const next = img.stars === n ? 0 : n;
      storeSetStars(img.id, next); // optimistic
      try {
        await cullSetRating(img.id, next);
      } catch (err) {
        storeSetStars(img.id, img.stars); // revert
        setToast(`Rating failed: ${String(err)}`);
      }
    },
    [storeSetStars, setToast],
  );

  const acceptBest = useCallback(
    (group: DupGroup) => {
      const st = useDedupStore.getState();
      const keeperId = st.keepers[group.key] ?? suggestKeeper(group.images);
      const toStage = group.images.filter(
        (i) => i.id !== keeperId && !st.staged[i.id],
      );
      pushUndo();
      storeStage(
        toStage.map((image) => ({
          image,
          groupKey: group.key,
          category: group.category,
        })),
      );
      storeSetResolved(group.key, true);
      const kept = group.images.find((i) => i.id === keeperId);
      setToast(
        `Kept ${kept?.filename ?? "best"} · ${toStage.length} staged for bin`,
      );
      // Advance to the next unreviewed group (in display order), else return to the overview.
      const gs = orderedGroups;
      const rs = useDedupStore.getState().resolved;
      const idx = gs.findIndex((g) => g.key === group.key);
      for (let k = 1; k <= gs.length; k++) {
        const j = (idx + k) % gs.length;
        if (!rs[gs[j].key]) {
          setFocusIdx(j);
          setFocusId(nextChallenger(gs[j], keeperOf(gs[j]), isStaged));
          return;
        }
      }
      setMode("groups");
    },
    [
      pushUndo,
      storeStage,
      storeSetResolved,
      setToast,
      keeperOf,
      isStaged,
      orderedGroups,
    ],
  );

  const emptyBin = useCallback(async () => {
    const st = useDedupStore.getState();
    const items = Object.values(st.staged);
    if (items.length === 0) return;
    setBinBusy(true);
    // Group staged photos by their source group, then resolve each group for real.
    const byGroup = new Map<string, StagedItem[]>();
    for (const it of items) {
      const arr = byGroup.get(it.groupKey) ?? [];
      arr.push(it);
      byGroup.set(it.groupKey, arr);
    }
    let trashed = 0;
    const trashedIds: number[] = [];
    try {
      for (const [groupKey, list] of byGroup) {
        const group = st.groups.find((g) => g.key === groupKey);
        const keepId = st.keepers[groupKey] ?? group?.images[0]?.id ?? -1;
        const trashIds = list.map((it) => it.image.id);
        const count = await dedupResolve(keepId, trashIds, {
          groupId: groupKey,
          candidateIds: group?.images.map((i) => i.id),
          autoKeeperId: group ? suggestKeeper(group.images) : undefined,
        });
        trashed += count;
        trashedIds.push(...trashIds);
      }
      storePrune(trashedIds);
      setToast(
        `Deleted ${trashed} photo${trashed === 1 ? "" : "s"} to OS trash`,
      );
      undoStack.current = [];
      setCanUndo(false);
      setBinOpen(false);
    } catch (err) {
      setToast(`Delete failed: ${String(err)}`);
    } finally {
      setBinBusy(false);
    }
  }, [setToast, storePrune]);

  // ── Navigation (indices are into `orderedGroups`, the display order) ──
  const openGroup = useCallback(
    (idx: number) => {
      const g = orderedGroups[idx];
      if (!g) return;
      setFocusIdx(idx);
      setFocusId(nextChallenger(g, keeperOf(g), isStaged));
      setMode("review");
    },
    [orderedGroups, keeperOf, isStaged],
  );

  const gotoGroup = useCallback(
    (idx: number) => {
      const clamped = Math.max(0, Math.min(idx, orderedGroups.length - 1));
      const g = orderedGroups[clamped];
      if (!g) return;
      setFocusIdx(clamped);
      setFocusId(nextChallenger(g, keeperOf(g), isStaged));
    },
    [orderedGroups, keeperOf, isStaged],
  );

  // ── Effects ──
  useEffect(() => {
    if (!useDedupStore.getState().initialScanDone) void scan();
    void effectivePreviewEdge().then(setPreviewEdge);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Clip focusIdx if the group list shrank (after emptying the bin).
  useEffect(() => {
    if (mode === "review" && focusIdx >= groups.length) {
      if (groups.length === 0) setMode("groups");
      else setFocusIdx(groups.length - 1);
    }
  }, [groups.length, focusIdx, mode]);

  // Close preview overlay on Escape regardless of mode.
  useEffect(() => {
    if (!previewImg) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        setPreviewImg(null);
      }
    };
    window.addEventListener("keydown", handler, true);
    return () => window.removeEventListener("keydown", handler, true);
  }, [previewImg]);

  const focusGroup = orderedGroups[focusIdx] ?? null;

  // Push the reviewed group's images (focus first) to the front of the background render queue so
  // their display-sharp preview tier renders now; the token bump then swaps the soft thumb for it.
  useEffect(() => {
    if (mode !== "review" || !focusGroup) return;
    const ids = [
      focusId,
      ...focusGroup.images.map((i) => i.id).filter((id) => id !== focusId),
    ].filter((id) => id >= 0);
    void thumbPrioritize(ids);
  }, [mode, focusGroup, focusId]);

  // Review-mode keyboard (actions + navigation; flicker/zoom are handled inside Review).
  useEffect(() => {
    if (mode !== "review" || !focusGroup || previewImg) return;
    const handler = (e: KeyboardEvent) => {
      const tag = (e.target as HTMLElement)?.tagName;
      if (tag === "INPUT" || tag === "TEXTAREA") return;
      const g = focusGroup;
      if (!g) return;
      const imgs = g.images;
      const i = imgs.findIndex((x) => x.id === focusId);
      const k = e.key;
      if (k === "Escape") {
        e.preventDefault();
        if (binOpen) setBinOpen(false);
        else setMode("groups");
      } else if (k === "ArrowLeft") {
        e.preventDefault();
        setFocusId(imgs[Math.max(0, (i < 0 ? 0 : i) - 1)].id);
      } else if (k === "ArrowRight") {
        e.preventDefault();
        setFocusId(imgs[Math.min(imgs.length - 1, (i < 0 ? 0 : i) + 1)].id);
      } else if (k === "[") {
        e.preventDefault();
        gotoGroup(focusIdx - 1);
      } else if (k === "]") {
        e.preventDefault();
        gotoGroup(focusIdx + 1);
      } else if (k === " ") {
        e.preventDefault();
        keep(focusId);
      } else if (k === "x" || k === "X") {
        reject(g, focusId);
      } else if (k === "s" || k === "S") {
        setKeeper(g.key, focusId);
      } else if (k === "Enter") {
        e.preventDefault();
        acceptBest(g);
      } else if (k === "c" || k === "C") {
        setReviewMode("compare");
      } else if (k === "l" || k === "L") {
        setReviewMode("loupe");
      } else if (k === "g" || k === "G") {
        setReviewMode("grid");
      } else if (k === "b" || k === "B") {
        setBinOpen((o) => !o);
      } else if (k === "u" || (k === "z" && (e.metaKey || e.ctrlKey))) {
        undo();
      } else if (/^[1-5]$/.test(k) && !e.metaKey && !e.ctrlKey) {
        const img = imgs[i < 0 ? 0 : i];
        if (img) void rate(img, parseInt(k, 10));
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [
    mode,
    focusGroup,
    focusIdx,
    focusId,
    binOpen,
    previewImg,
    keep,
    reject,
    setKeeper,
    acceptBest,
    gotoGroup,
    undo,
    rate,
  ]);

  const stagedList = Object.values(staged);
  const hasByteGroups = groups.some((g) => g.category === "byte");

  return (
    <div
      style={{
        display: "flex",
        flexDirection: "column",
        flex: 1,
        minHeight: 0,
        background: "var(--color-app)",
      }}
    >
      {mode === "groups" || !focusGroup ? (
        <Overview
          groups={orderedGroups}
          keepers={keepers}
          resolved={resolved}
          stagedCount={stagedList.length}
          loading={loading}
          scanningSimilar={scanningSimilar}
          fillProgress={fillProgress}
          threshold={threshold}
          hasByteGroups={hasByteGroups}
          sortKey={sortKey}
          sortDir={sortDir}
          setSortKey={setSortKey}
          setSortDir={setSortDir}
          setThreshold={setThreshold}
          onRescan={() => void scan()}
          onFindSimilar={() => void handleSimilarScan()}
          onAutoResolve={() => void handleAutoResolve()}
          onOpenGroup={openGroup}
          onOpenBin={() => setBinOpen(true)}
        />
      ) : (
        <Review
          group={focusGroup}
          groupIndex={focusIdx}
          groupCount={groups.length}
          keeperId={keeperOf(focusGroup)}
          focusId={focusId}
          mode={reviewMode}
          canUndo={canUndo}
          previewEdge={previewEdge}
          isStaged={isStaged}
          tokenOf={tokenOf}
          setMode={setReviewMode}
          onBack={() => setMode("groups")}
          onPrevGroup={() => gotoGroup(focusIdx - 1)}
          onNextGroup={() => gotoGroup(focusIdx + 1)}
          onUndo={undo}
          onAcceptBest={() => acceptBest(focusGroup)}
          onFocus={setFocusId}
          onSetKeeper={(id) => setKeeper(focusGroup.key, id)}
          onKeep={keep}
          onReject={(id) => reject(focusGroup, id)}
          onRate={rate}
          onPreview={setPreviewImg}
        />
      )}

      <BinDrawer
        open={binOpen}
        staged={stagedList}
        busy={binBusy}
        onClose={() => setBinOpen(false)}
        onRestore={(id) => storeUnstage([id])}
        onEmpty={() => void emptyBin()}
      />

      {previewImg && (
        <PreviewOverlay
          img={previewImg}
          previewEdge={previewEdge}
          onClose={() => setPreviewImg(null)}
        />
      )}
    </div>
  );
}
