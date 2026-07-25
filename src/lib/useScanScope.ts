import { useEffect, useMemo, useState } from "react";
import {
  scanScopeCounts,
  scopeFromParams,
  type CollectionRow,
  type KeywordRow,
  type QueryParams,
  type ScanScope,
  type ScopeCounts,
} from "./ipc";
import { log } from "./logger";

export interface ScanScopeState {
  /** The container the grid is showing, or `null` for the whole library. */
  scope: ScanScope | null;
  /** Human name for `scope` ("2026-06-22", "denmark", "RAW"); `null` when unscoped. */
  label: string | null;
  /** Sizing for the scan buttons; `null` until the first fetch resolves. */
  counts: ScopeCounts | null;
}

/** Human-readable name for each scope dimension the user can actually be inside. */
function describe(
  scope: ScanScope,
  collections: CollectionRow[],
  keywords: KeywordRow[],
): string {
  const parts: string[] = [];
  // Day is more specific than year — when both are set, the day alone reads better.
  if (scope.captureDate) parts.push(scope.captureDate);
  else if (scope.captureYear) parts.push(scope.captureYear);
  if (scope.collectionId != null) {
    const c = collections.find((x) => x.id === scope.collectionId);
    parts.push(c ? c.name : "collection");
  }
  if (scope.keywordId != null) {
    const k = keywords.find((x) => x.id === scope.keywordId);
    parts.push(k ? k.name : "keyword");
  }
  if (scope.importSessionId != null) parts.push("last import");
  if (scope.format) parts.push(scope.format.toUpperCase());
  if (scope.folderId != null) parts.push("folder");
  return parts.join(" · ");
}

/**
 * Derives the AI-scan scope from the live library filter and prices it.
 *
 * The counts query is read-only and loads no models, so it is safe to re-run on every filter
 * change. `refreshKey` (bump it on `analysis:done`) forces a re-price after a scan lands, since
 * finishing work shrinks the pending numbers.
 */
export function useScanScope(
  params: QueryParams,
  collections: CollectionRow[],
  keywords: KeywordRow[],
  refreshKey: number,
): ScanScopeState {
  const scope = useMemo(() => scopeFromParams(params), [params]);
  const [counts, setCounts] = useState<ScopeCounts | null>(null);
  // Structural key: re-fetch when the scope's *values* change, not when a new object is allocated.
  const scopeKey = scope ? JSON.stringify(scope) : "";

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const c = await scanScopeCounts(scope);
        if (!cancelled) setCounts(c);
      } catch (err) {
        // A failed price is not worth surfacing — the buttons just fall back to unlabeled counts.
        if (!cancelled) setCounts(null);
        log.debug("analysis", "scope counts failed", log.errorSummary(err));
      }
    })();
    return () => {
      cancelled = true;
    };
    // `scope` is intentionally not a dep: `scopeKey` is its structural identity.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [scopeKey, refreshKey]);

  return {
    scope,
    label: scope ? describe(scope, collections, keywords) : null,
    counts,
  };
}
