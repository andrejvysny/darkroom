import { useState, useEffect, useCallback, useRef } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  libraryQuery,
  libraryCount,
  libraryFolders,
  libraryDateTree,
  libraryIndexRoot,
  appDefaultLibrary,
  keywordsList,
  collectionsList,
  clearedFilters,
  hasActiveFilters,
  LABEL_NONE,
  type QueryParams,
  type ImageRow,
  type FolderRow,
  type DateTreeYear,
  type KeywordRow,
  type CollectionRow,
} from "./ipc";

export type IndexingState = { done: number; total: number };

export interface LibraryState {
  images: ImageRow[];
  folders: FolderRow[];
  /** Year → Date capture-date tree for the left-nav Folders section. */
  dateTree: DateTreeYear[];
  keywords: KeywordRow[];
  collections: CollectionRow[];
  /** Count of the current (filtered) query. */
  total: number;
  /** Count of all present images, ignoring filters (for the "All photos" nav). */
  grandTotal: number;
  loading: boolean;
  /** True while a `loadMore` page fetch is in flight (initial load uses `loading`). */
  loadingMore: boolean;
  indexing: IndexingState | null;
  /** True while an import (Import button) is in flight — suppresses the empty-state during it. */
  importing: boolean;
  error: string | null;
  params: QueryParams;
}

export interface LibraryActions {
  refresh: (overrides?: Partial<QueryParams>) => Promise<void>;
  /** Re-query just the image page + its count for the current filter (no folder/keyword reload). */
  refreshImages: (overrides?: Partial<QueryParams>) => Promise<void>;
  /** Append the next page of results (infinite scroll). No-op when all rows are loaded or a page
   *  fetch is already in flight. */
  loadMore: () => void;
  /** Merge a partial set of params in one update (single refresh). */
  patchParams: (patch: Partial<QueryParams>) => void;
  /** Clear every filter dimension (keeps sort & search). */
  clearFilters: () => void;
  setSort: (sort: QueryParams["sort"]) => void;
  setSearch: (search: string | null) => void;
  reindex: () => Promise<void>;
  patchImage: (id: number, patch: Partial<ImageRow>) => void;
  /** Refetch only the keyword list + counts (after tagging changes). */
  reloadKeywords: () => Promise<void>;
  /** Refetch only the collection list + counts (after membership/CRUD changes). */
  reloadCollections: () => Promise<void>;
}

/** Rows fetched per page (initial load + each infinite-scroll append). */
const PAGE_SIZE = 500;

const DEFAULT_PARAMS: QueryParams = {
  sort: "capture_desc",
  limit: PAGE_SIZE,
  offset: 0,
};

/** Sorts paginated by keyset/seek (stable under concurrent import inserts, O(log n) at depth). The
 *  rest (filename/rating) stay on LIMIT/OFFSET — rarely imported into, and the sorted-merge keeps the
 *  loaded list a true prefix so offset stays correct. */
const SEEK_SORTS = new Set<QueryParams["sort"]>([
  "capture_desc",
  "capture_asc",
  "imported_desc",
  "imported_asc",
]);
function isSeekSort(sort: QueryParams["sort"] | undefined): boolean {
  return SEEK_SORTS.has(sort ?? "capture_desc");
}

/** The keyset cursor value for `row` under `sort` (capture sorts → captureDate, import sorts →
 *  importedAt). May be null for capture sorts (a row in the NULL capture_date block). */
function cursorValueOf(
  row: ImageRow,
  sort: QueryParams["sort"] | undefined,
): number | null {
  return sort === "imported_desc" || sort === "imported_asc"
    ? row.importedAt
    : row.captureDate;
}

/** Params for a FIRST page (initial load / refresh / sort change): seek-aware, cursor cleared. */
function firstPageParams(p: QueryParams): QueryParams {
  return {
    ...p,
    limit: PAGE_SIZE,
    offset: 0,
    seek: isSeekSort(p.sort),
    cursorValue: null,
    cursorId: null,
  };
}

/** Comparator mirroring the backend ORDER BY for `sort` — same NULL placement (DESC ⇒ NULLs last,
 *  ASC ⇒ NULLs first) and id tie-break direction. Returns <0 when `a` sorts before `b`. */
function makeCmp(
  sort: QueryParams["sort"] | undefined,
): (a: ImageRow, b: ImageRow) => number {
  const capDesc = (a: ImageRow, b: ImageRow) => {
    const ax = a.captureDate;
    const bx = b.captureDate;
    if (ax == null && bx == null) return b.id - a.id;
    if (ax == null) return 1; // NULLs last
    if (bx == null) return -1;
    return ax !== bx ? bx - ax : b.id - a.id;
  };
  const capAsc = (a: ImageRow, b: ImageRow) => {
    const ax = a.captureDate;
    const bx = b.captureDate;
    if (ax == null && bx == null) return a.id - b.id;
    if (ax == null) return -1; // NULLs first
    if (bx == null) return 1;
    return ax !== bx ? ax - bx : a.id - b.id;
  };
  switch (sort) {
    case "capture_asc":
      return capAsc;
    case "filename":
      return (a, b) =>
        a.filename < b.filename
          ? -1
          : a.filename > b.filename
            ? 1
            : a.id - b.id;
    case "filename_desc":
      return (a, b) =>
        a.filename > b.filename
          ? -1
          : a.filename < b.filename
            ? 1
            : b.id - a.id;
    case "rating_desc":
      return (a, b) =>
        b.stars !== a.stars ? b.stars - a.stars : capDesc(a, b);
    case "rating_asc":
      return (a, b) =>
        a.stars !== b.stars ? a.stars - b.stars : capDesc(a, b);
    case "imported_desc":
      return (a, b) =>
        a.importedAt !== b.importedAt
          ? b.importedAt - a.importedAt
          : b.id - a.id;
    case "imported_asc":
      return (a, b) =>
        a.importedAt !== b.importedAt
          ? a.importedAt - b.importedAt
          : a.id - b.id;
    default:
      return capDesc; // capture_desc
  }
}

/** Merge two `cmp`-sorted arrays into one. O(n+m), stable on ties (left wins). */
function mergeSorted(
  a: ImageRow[],
  b: ImageRow[],
  cmp: (x: ImageRow, y: ImageRow) => number,
): ImageRow[] {
  const out: ImageRow[] = new Array(a.length + b.length);
  let i = 0;
  let j = 0;
  let k = 0;
  while (i < a.length && j < b.length) {
    out[k++] = cmp(a[i], b[j]) <= 0 ? a[i++] : b[j++];
  }
  while (i < a.length) out[k++] = a[i++];
  while (j < b.length) out[k++] = b[j++];
  return out;
}

/** Does a row still satisfy the active filter on the dimensions evaluable from the row alone
 *  (flag / min stars / color label)? Server-only dimensions are treated as matching here. */
function matchesRowFilter(row: ImageRow, p: QueryParams): boolean {
  if (p.flag != null && row.flag !== p.flag) return false;
  if (p.minStars != null && row.stars < p.minStars) return false;
  if (p.colorLabel != null) {
    if (p.colorLabel === LABEL_NONE) {
      if (row.colorLabel != null) return false;
    } else if (row.colorLabel !== p.colorLabel) {
      return false;
    }
  }
  return true;
}

export function useLibrary(): LibraryState & LibraryActions {
  const [images, setImages] = useState<ImageRow[]>([]);
  const [folders, setFolders] = useState<FolderRow[]>([]);
  const [dateTree, setDateTree] = useState<DateTreeYear[]>([]);
  const [keywords, setKeywords] = useState<KeywordRow[]>([]);
  const [collections, setCollections] = useState<CollectionRow[]>([]);
  const [total, setTotal] = useState(0);
  const [grandTotal, setGrandTotal] = useState(0);
  const [loading, setLoading] = useState(false);
  const [loadingMore, setLoadingMore] = useState(false);
  const [indexing, setIndexing] = useState<IndexingState | null>(null);
  const [importing, setImporting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [params, setParams] = useState<QueryParams>(DEFAULT_PARAMS);

  // Stable ref so callbacks always see latest params without being recreated
  const paramsRef = useRef<QueryParams>(DEFAULT_PARAMS);
  paramsRef.current = params;

  // Refs mirror the latest images/total so the stable `loadMore` callback can read them without
  // being recreated (and without racing on stale closures).
  const imagesRef = useRef<ImageRow[]>([]);
  imagesRef.current = images;
  const totalRef = useRef(0);
  totalRef.current = total;
  const loadingMoreRef = useRef(false);
  // True once every present row for the current filter is loaded (no more pages). Lets the live merge
  // insert ALL fresh rows (the cursor is the global last) vs. only within-window rows otherwise.
  const endReachedRef = useRef(false);
  // Import rows that arrived while a `loadMore` seek was in flight — re-evaluated against the advanced
  // cursor once the page lands, so a row committed mid-seek is never dropped-and-lost (keyset race).
  const bufferRef = useRef<ImageRow[]>([]);
  // Trailing-throttle timer for the live sidebar (Folders tree + counts) refresh during import.
  const sidebarTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Guard against double-run in React 19 StrictMode
  const bootstrappedRef = useRef(false);

  // Stable refresh — reads params from ref so identity never changes
  const refresh = useCallback(async (overrides?: Partial<QueryParams>) => {
    setLoading(true);
    setError(null);
    try {
      const merged = overrides
        ? { ...paramsRef.current, ...overrides }
        : paramsRef.current;
      const [imgs, cnt, flds, tree, grand, kws, cols] = await Promise.all([
        libraryQuery(firstPageParams(merged)),
        libraryCount(merged),
        libraryFolders(),
        libraryDateTree(),
        libraryCount({}),
        keywordsList(),
        collectionsList(),
      ]);
      setImages(imgs);
      endReachedRef.current = imgs.length >= cnt;
      setTotal(cnt);
      setGrandTotal(grand);
      setFolders(flds);
      setDateTree(tree);
      setKeywords(kws);
      setCollections(cols);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  // Lean refresh for the filter/sort/search hot path: refetches only the image page + its count.
  // Folders/keywords/collections/grandTotal are independent of the active filter (their counts are
  // global present-image counts), so they stay put — only `refresh` (full) reloads them.
  const refreshImages = useCallback(
    async (overrides?: Partial<QueryParams>) => {
      setLoading(true);
      setError(null);
      try {
        const merged = overrides
          ? { ...paramsRef.current, ...overrides }
          : paramsRef.current;
        const [imgs, cnt] = await Promise.all([
          libraryQuery(firstPageParams(merged)),
          libraryCount(merged),
        ]);
        setImages(imgs);
        endReachedRef.current = imgs.length >= cnt;
        setTotal(cnt);
      } catch (e) {
        setError(String(e));
      } finally {
        setLoading(false);
      }
    },
    [],
  );

  // Insert fresh import rows into the grid, keeping `images` an exact sorted prefix. Reads params/refs
  // so its identity is stable. Inserts only rows that sort within the loaded window (strictly before
  // the last loaded row) — unless the whole library is loaded, where every fresh row belongs. Rows
  // past the cursor are dropped here and re-fetched by `loadMore`'s seek: no gap, no duplicate.
  const mergeFreshIntoGrid = useCallback((fresh: ImageRow[]) => {
    if (fresh.length === 0) return;
    const cmp = makeCmp(paramsRef.current.sort);
    setImages((prev) => {
      const have = new Set(prev.map((i) => i.id));
      const add = fresh.filter((r) => !have.has(r.id));
      if (add.length === 0) return prev;
      const cursor = prev[prev.length - 1];
      const within =
        endReachedRef.current || !cursor
          ? add
          : add.filter((r) => cmp(r, cursor) < 0);
      if (within.length === 0) return prev;
      within.sort(cmp);
      return mergeSorted(prev, within, cmp);
    });
  }, []);

  // Append the next page. Reads current length/total/params from refs so its identity is stable.
  // Time-based sorts page by keyset cursor (the last loaded row); filename/rating use OFFSET.
  const loadMore = useCallback(async () => {
    if (loadingMoreRef.current) return;
    const cur = imagesRef.current;
    const last = cur[cur.length - 1];
    if (!last) return;
    if (cur.length >= totalRef.current) return; // everything loaded
    loadingMoreRef.current = true;
    setLoadingMore(true);
    const sort = paramsRef.current.sort;
    try {
      const page = await libraryQuery(
        isSeekSort(sort)
          ? {
              ...paramsRef.current,
              limit: PAGE_SIZE,
              seek: true,
              cursorValue: cursorValueOf(last, sort),
              cursorId: last.id,
            }
          : { ...paramsRef.current, limit: PAGE_SIZE, offset: cur.length },
      );
      setImages((prev) => {
        // A wholesale refresh replaced the list while this page was in flight — its tail no longer
        // matches our cursor → drop the stale page. (Live merges are buffered during a loadMore, so
        // the tail only changes via refresh, never mid-flight insertion.)
        if (prev.length === 0 || prev[prev.length - 1].id !== last.id)
          return prev;
        const have = new Set(prev.map((i) => i.id));
        const add = page.filter((r) => !have.has(r.id));
        endReachedRef.current = page.length < PAGE_SIZE;
        return add.length === 0 ? prev : [...prev, ...add];
      });
    } catch (e) {
      setError(String(e));
    } finally {
      loadingMoreRef.current = false;
      setLoadingMore(false);
      // Cursor advanced — re-evaluate import rows buffered during this in-flight page.
      if (bufferRef.current.length > 0) {
        const buffered = bufferRef.current;
        bufferRef.current = [];
        mergeFreshIntoGrid(buffered);
      }
    }
  }, [mergeFreshIntoGrid]);

  // Stable startIndexing — depends only on stable refresh
  const startIndexing = useCallback(
    async (path: string): Promise<UnlistenFn[]> => {
      setIndexing({ done: 0, total: 0 });

      const unlisteners: UnlistenFn[] = [];

      const unProgress = await listen<{ done: number; total: number }>(
        "import:progress",
        (ev) => setIndexing({ done: ev.payload.done, total: ev.payload.total }),
      );
      unlisteners.push(unProgress);

      const unDone = await listen<{
        scanned: number;
        added: number;
        skipped: number;
        failed: number;
      }>("import:done", (_ev) => {
        setIndexing(null);
        void refresh();
      });
      unlisteners.push(unDone);

      // Fire-and-forget; events drive the UI
      void libraryIndexRoot(path).catch((e) => {
        setIndexing(null);
        setError(String(e));
      });

      return unlisteners;
    },
    [refresh],
  );

  const reindex = useCallback(async () => {
    const path = await appDefaultLibrary();
    if (!path) return;
    await startIndexing(path);
  }, [startIndexing]);

  // Bootstrap once on mount
  useEffect(() => {
    if (bootstrappedRef.current) return;
    bootstrappedRef.current = true;

    let unlisteners: UnlistenFn[] = [];

    async function bootstrap() {
      setLoading(true);
      // Auto-import the bundled validation library only on the very first launch ever. Once the
      // user has opened the app (or explicitly reset to empty), they own their library — never
      // silently re-import on an empty catalog. This keeps "Reset catalog" leaving a truly empty
      // app that stays empty across reloads/restarts.
      const firstLaunch = localStorage.getItem("darkroom.bootstrapped") !== "1";
      try {
        const [flds, tree, cnt, kws, cols] = await Promise.all([
          libraryFolders(),
          libraryDateTree(),
          libraryCount(DEFAULT_PARAMS),
          keywordsList(),
          collectionsList(),
        ]);
        setKeywords(kws);
        setCollections(cols);
        localStorage.setItem("darkroom.bootstrapped", "1");

        if (firstLaunch && (flds.length === 0 || cnt === 0)) {
          const defaultPath = await appDefaultLibrary();
          if (defaultPath) {
            unlisteners = await startIndexing(defaultPath);
          } else {
            // No default path, just load whatever is there
            await refresh();
          }
        } else {
          // Library already has data — load directly
          const imgs = await libraryQuery(firstPageParams(DEFAULT_PARAMS));
          setFolders(flds);
          setDateTree(tree);
          setImages(imgs);
          endReachedRef.current = imgs.length >= cnt;
          // DEFAULT_PARAMS carries no filter dimensions, so cnt is the unfiltered total.
          setTotal(cnt);
          setGrandTotal(cnt);
        }
      } catch (e) {
        setError(String(e));
      } finally {
        setLoading(false);
      }
    }

    void bootstrap();

    return () => {
      unlisteners.forEach((fn) => fn());
    };
  }, [refresh, startIndexing]);

  // Refresh when the FS watcher reports on-disk changes (reconcile / new files).
  useEffect(() => {
    let un: UnlistenFn | undefined;
    void listen("library:changed", () => void refresh()).then((f) => {
      un = f;
    });
    return () => un?.();
  }, [refresh]);

  // Live sidebar during import: refetch the Folders date-tree + folder counts + grand total on a
  // trailing ~500ms throttle so a burst of import events triggers at most ~2 GROUP BY queries/sec.
  // These are global (filter-independent), so they update even on a filtered view.
  const scheduleSidebarRefresh = useCallback(() => {
    if (sidebarTimerRef.current != null) return; // already pending — coalesce the burst
    sidebarTimerRef.current = setTimeout(() => {
      sidebarTimerRef.current = null;
      void Promise.all([libraryDateTree(), libraryFolders(), libraryCount({})])
        .then(([tree, flds, grand]) => {
          setDateTree(tree);
          setFolders(flds);
          setGrandTotal(grand);
          const p = paramsRef.current;
          // On the unfiltered view the filtered count == grand total; keep it exact (corrects any
          // drift from the optimistic per-event bumps below).
          if (!hasActiveFilters(p) && !p.search) setTotal(grand);
        })
        .catch(() => {});
    }, 500);
  }, []);

  // Live grid during import: the backend streams each freshly-catalogued row in `import:progress`.
  // We sorted-merge them into their correct position under the active sort (keeping `images` an exact
  // sorted prefix), so photos land in capture-date order with no duplicates. Only the unfiltered "All
  // photos" view merges into the grid; a filtered/searched view waits for the `import:done` refresh
  // (wired in the import flow / `startIndexing`) — but its sidebar still updates live.
  useEffect(() => {
    let unProgress: UnlistenFn | undefined;
    let unDone: UnlistenFn | undefined;
    void listen<{ done: number; total: number; images?: ImageRow[] }>(
      "import:progress",
      (ev) => {
        // Active while files remain. The final event (done === total) always fires after the import
        // loop — even if a post-loop DB step then errors — so this self-clears without depending on
        // `import:done` (which is skipped on a wholesale import failure).
        setImporting(ev.payload.done < ev.payload.total);
        scheduleSidebarRefresh();
        const incoming = ev.payload.images;
        if (!incoming || incoming.length === 0) return;
        const p = paramsRef.current;
        if (hasActiveFilters(p) || p.search) return;
        const have = new Set(imagesRef.current.map((i) => i.id));
        const fresh = incoming.filter((r) => !have.has(r.id));
        if (fresh.length === 0) return;
        // Optimistic count bump for immediate feedback; the throttled sidebar refresh reconciles it.
        setTotal((t) => t + fresh.length);
        setGrandTotal((g) => g + fresh.length);
        // Serialize with an in-flight loadMore (keyset race): buffer now, re-evaluate against the
        // advanced cursor once the page lands so a row committed mid-seek is never dropped-and-lost.
        if (loadingMoreRef.current) {
          bufferRef.current.push(...fresh);
          return;
        }
        mergeFreshIntoGrid(fresh);
      },
    ).then((f) => {
      unProgress = f;
    });
    void listen("import:done", () => {
      setImporting(false);
      bufferRef.current = [];
      if (sidebarTimerRef.current != null) {
        clearTimeout(sidebarTimerRef.current);
        sidebarTimerRef.current = null;
      }
    }).then((f) => {
      unDone = f;
    });
    return () => {
      unProgress?.();
      unDone?.();
      if (sidebarTimerRef.current != null) {
        clearTimeout(sidebarTimerRef.current);
        sidebarTimerRef.current = null;
      }
    };
  }, [mergeFreshIntoGrid, scheduleSidebarRefresh]);

  // Re-fetch when params change (skip the initial render)
  const isFirstParamsRun = useRef(true);
  useEffect(() => {
    if (isFirstParamsRun.current) {
      isFirstParamsRun.current = false;
      return;
    }
    void refreshImages();
  }, [params, refreshImages]);

  const patchParams = useCallback((patch: Partial<QueryParams>) => {
    setParams((p) => ({ ...p, ...patch }));
  }, []);

  const clearFilters = useCallback(() => {
    setParams((p) => ({ ...p, ...clearedFilters() }));
  }, []);

  const setSort = useCallback((sort: QueryParams["sort"]) => {
    setParams((p) => ({ ...p, sort }));
  }, []);

  const setSearch = useCallback((search: string | null) => {
    setParams((p) => ({ ...p, search }));
  }, []);

  // Update one image's row in place. If the change makes it no longer match the active filter on a
  // dimension we can evaluate from the row itself (flag / rating / color label), drop it from the
  // grid and decrement the count immediately — so e.g. rejecting a photo in the Picks view removes
  // it live without a re-query. Filters needing server data (keyword/collection/detected) are left
  // to the caller to reconcile.
  const patchImage = useCallback((id: number, patch: Partial<ImageRow>) => {
    const p = paramsRef.current;
    const cur = imagesRef.current.find((i) => i.id === id);
    const updated = cur ? { ...cur, ...patch } : null;
    const remove = updated != null && !matchesRowFilter(updated, p);
    setImages((prev) =>
      remove
        ? prev.filter((i) => i.id !== id)
        : prev.map((img) => (img.id === id ? { ...img, ...patch } : img)),
    );
    if (remove) setTotal((t) => Math.max(0, t - 1));
  }, []);

  const reloadKeywords = useCallback(async () => {
    try {
      setKeywords(await keywordsList());
    } catch (e) {
      setError(String(e));
    }
  }, []);

  const reloadCollections = useCallback(async () => {
    try {
      setCollections(await collectionsList());
    } catch (e) {
      setError(String(e));
    }
  }, []);

  return {
    images,
    folders,
    dateTree,
    keywords,
    collections,
    total,
    grandTotal,
    loading,
    loadingMore,
    indexing,
    importing,
    error,
    params,
    refresh,
    refreshImages,
    loadMore,
    patchParams,
    clearFilters,
    setSort,
    setSearch,
    reindex,
    patchImage,
    reloadKeywords,
    reloadCollections,
  };
}
