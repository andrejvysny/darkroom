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
  error: string | null;
  params: QueryParams;
}

export interface LibraryActions {
  refresh: (overrides?: Partial<QueryParams>) => Promise<void>;
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
      try {
        const [flds, cnt, kws, cols] = await Promise.all([
          libraryFolders(),
          libraryCount(DEFAULT_PARAMS),
          keywordsList(),
          collectionsList(),
        ]);
        setKeywords(kws);
        setCollections(cols);

        if (flds.length === 0 || cnt === 0) {
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

  const patchImage = useCallback((id: number, patch: Partial<ImageRow>) => {
    setImages((prev) =>
      prev.map((img) => (img.id === id ? { ...img, ...patch } : img)),
    );
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
    error,
    params,
    refresh,
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
