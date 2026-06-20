import { useState, useEffect, useCallback, useRef } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  libraryQuery,
  libraryCount,
  libraryFolders,
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
  type KeywordRow,
  type CollectionRow,
} from "./ipc";

export type IndexingState = { done: number; total: number };

export interface LibraryState {
  images: ImageRow[];
  folders: FolderRow[];
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
      const [imgs, cnt, flds, grand, kws, cols] = await Promise.all([
        libraryQuery(merged),
        libraryCount(merged),
        libraryFolders(),
        libraryCount({}),
        keywordsList(),
        collectionsList(),
      ]);
      setImages(imgs);
      setTotal(cnt);
      setGrandTotal(grand);
      setFolders(flds);
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
          libraryQuery(merged),
          libraryCount(merged),
        ]);
        setImages(imgs);
        setTotal(cnt);
      } catch (e) {
        setError(String(e));
      } finally {
        setLoading(false);
      }
    },
    [],
  );

  // Append the next page. Reads current length/total/params from refs so its identity is stable.
  const loadMore = useCallback(async () => {
    if (loadingMoreRef.current) return;
    const offset = imagesRef.current.length;
    if (offset >= totalRef.current) return; // everything loaded
    loadingMoreRef.current = true;
    setLoadingMore(true);
    try {
      const page = await libraryQuery({
        ...paramsRef.current,
        limit: PAGE_SIZE,
        offset,
      });
      setImages((prev) => {
        // A refresh may have reset the list while this page was in flight — drop the stale page.
        if (prev.length !== offset) return prev;
        return [...prev, ...page];
      });
    } catch (e) {
      setError(String(e));
    } finally {
      loadingMoreRef.current = false;
      setLoadingMore(false);
    }
  }, []);

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
        const [flds, cnt, kws, cols] = await Promise.all([
          libraryFolders(),
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
          const imgs = await libraryQuery(DEFAULT_PARAMS);
          setFolders(flds);
          setImages(imgs);
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

  // Live-append photos as they're imported. The backend streams each freshly-catalogued row in the
  // `import:progress` event; we append them to the grid immediately so the user sees photos land as
  // they import. The counter/toast is owned by the import flow / `startIndexing` listeners, and the
  // `import:done` refresh wired there reconciles ordering & counts afterward. Only append on the
  // unfiltered "All photos" view — a filtered/sorted/searched view would mis-place fresh rows, so it
  // waits for that reconcile instead.
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
        const incoming = ev.payload.images;
        if (!incoming || incoming.length === 0) return;
        const p = paramsRef.current;
        if (hasActiveFilters(p) || p.search) return;
        const have = new Set(imagesRef.current.map((i) => i.id));
        const fresh = incoming.filter((r) => !have.has(r.id));
        if (fresh.length === 0) return;
        setImages((prev) => [...prev, ...fresh]);
        setTotal((t) => t + fresh.length);
        setGrandTotal((g) => g + fresh.length);
      },
    ).then((f) => {
      unProgress = f;
    });
    void listen("import:done", () => setImporting(false)).then((f) => {
      unDone = f;
    });
    return () => {
      unProgress?.();
      unDone?.();
    };
  }, []);

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
