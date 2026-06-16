import { useState, useEffect, useCallback, useRef } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  libraryQuery,
  libraryCount,
  libraryFolders,
  libraryIndexRoot,
  appDefaultLibrary,
  keywordsList,
  clearedFilters,
  type QueryParams,
  type ImageRow,
  type FolderRow,
  type KeywordRow,
} from "./ipc";

export type IndexingState = { done: number; total: number };

export interface LibraryState {
  images: ImageRow[];
  folders: FolderRow[];
  keywords: KeywordRow[];
  /** Count of the current (filtered) query. */
  total: number;
  /** Count of all present images, ignoring filters (for the "All photos" nav). */
  grandTotal: number;
  loading: boolean;
  indexing: IndexingState | null;
  error: string | null;
  params: QueryParams;
}

export interface LibraryActions {
  refresh: (overrides?: Partial<QueryParams>) => Promise<void>;
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
}

const DEFAULT_PARAMS: QueryParams = {
  sort: "capture_desc",
  limit: 500,
  offset: 0,
};

export function useLibrary(): LibraryState & LibraryActions {
  const [images, setImages] = useState<ImageRow[]>([]);
  const [folders, setFolders] = useState<FolderRow[]>([]);
  const [keywords, setKeywords] = useState<KeywordRow[]>([]);
  const [total, setTotal] = useState(0);
  const [grandTotal, setGrandTotal] = useState(0);
  const [loading, setLoading] = useState(false);
  const [indexing, setIndexing] = useState<IndexingState | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [params, setParams] = useState<QueryParams>(DEFAULT_PARAMS);

  // Stable ref so callbacks always see latest params without being recreated
  const paramsRef = useRef<QueryParams>(DEFAULT_PARAMS);
  paramsRef.current = params;

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
      const [imgs, cnt, flds, grand, kws] = await Promise.all([
        libraryQuery(merged),
        libraryCount(merged),
        libraryFolders(),
        libraryCount({}),
        keywordsList(),
      ]);
      setImages(imgs);
      setTotal(cnt);
      setGrandTotal(grand);
      setFolders(flds);
      setKeywords(kws);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
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
        const [flds, cnt, kws] = await Promise.all([
          libraryFolders(),
          libraryCount(DEFAULT_PARAMS),
          keywordsList(),
        ]);
        setKeywords(kws);

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

  // Re-fetch when params change (skip the initial render)
  const isFirstParamsRun = useRef(true);
  useEffect(() => {
    if (isFirstParamsRun.current) {
      isFirstParamsRun.current = false;
      return;
    }
    void refresh();
  }, [params, refresh]);

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

  return {
    images,
    folders,
    keywords,
    total,
    grandTotal,
    loading,
    indexing,
    error,
    params,
    refresh,
    patchParams,
    clearFilters,
    setSort,
    setSearch,
    reindex,
    patchImage,
    reloadKeywords,
  };
}
