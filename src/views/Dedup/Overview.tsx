import { thumbUrl, type DupGroup } from "../../lib/ipc";
import {
  CATEGORY_BADGE,
  CATEGORY_DOT,
  CATEGORY_LABELS,
  fmtBytes,
  groupTitle,
  reclaimableBytes,
  SORT_LABELS,
  suggestKeeper,
  type SortDir,
  type SortKey,
} from "./helpers";

const PREVIEW_CAP = 5;
const SORT_KEYS: SortKey[] = ["reclaim", "files", "name"];

interface OverviewProps {
  groups: DupGroup[];
  keepers: Record<string, number>;
  resolved: Record<string, boolean>;
  stagedCount: number;
  loading: boolean;
  scanningSimilar: boolean;
  fillProgress: { done: number; total: number } | null;
  threshold: number;
  hasByteGroups: boolean;
  sortKey: SortKey;
  sortDir: SortDir;
  setSortKey: (k: SortKey) => void;
  setSortDir: (d: SortDir) => void;
  setThreshold: (n: number) => void;
  onRescan: () => void;
  onFindSimilar: () => void;
  onAutoResolve: () => void;
  onOpenGroup: (idx: number) => void;
  onOpenBin: () => void;
}

function GroupRow({
  group,
  keeperId,
  reviewed,
  onOpen,
}: {
  group: DupGroup;
  keeperId: number;
  reviewed: boolean;
  onOpen: () => void;
}) {
  const preview = group.images.slice(0, PREVIEW_CAP);
  const overflow = group.images.length - PREVIEW_CAP;
  const reclaim = reclaimableBytes(group, keeperId);

  return (
    <div
      onClick={onOpen}
      style={{
        display: "flex",
        alignItems: "center",
        gap: 16,
        background: "var(--color-panel)",
        border: `1px solid ${reviewed ? "var(--color-pick)" : "var(--color-line)"}`,
        borderRadius: "var(--radius-lg)",
        padding: "12px 14px",
        cursor: "pointer",
      }}
    >
      <div style={{ display: "flex", gap: 5, flex: "none" }}>
        {preview.map((img) => (
          <div
            key={img.id}
            style={{
              position: "relative",
              width: 62,
              height: 42,
              borderRadius: "var(--radius-sm)",
              overflow: "hidden",
              background: "var(--color-stage)",
            }}
          >
            <img
              src={thumbUrl(img.contentHash, 256)}
              alt={img.filename}
              style={{
                width: "100%",
                height: "100%",
                objectFit: "cover",
                display: "block",
              }}
              loading="lazy"
            />
            <div
              style={{
                position: "absolute",
                inset: 0,
                border: `1.5px solid ${
                  img.id === keeperId ? "var(--color-pick)" : "transparent"
                }`,
                borderRadius: "var(--radius-sm)",
              }}
            />
          </div>
        ))}
        {overflow > 0 && (
          <div
            style={{
              width: 62,
              height: 42,
              borderRadius: "var(--radius-sm)",
              border: "1px dashed var(--color-line-2)",
              display: "flex",
              alignItems: "center",
              justifyContent: "center",
              fontSize: 12,
              color: "var(--color-t3)",
            }}
          >
            +{overflow}
          </div>
        )}
      </div>

      <div style={{ flex: 1, minWidth: 0 }}>
        <div style={{ display: "flex", alignItems: "center", gap: 9 }}>
          <span
            style={{
              display: "inline-flex",
              alignItems: "center",
              gap: 5,
              fontSize: 10,
              fontWeight: 700,
              letterSpacing: ".07em",
              color: "var(--color-t1)",
              background: "var(--color-elev)",
              borderRadius: 4,
              padding: "3px 7px",
            }}
          >
            <span
              style={{
                width: 6,
                height: 6,
                borderRadius: "50%",
                background: CATEGORY_DOT[group.category] ?? "var(--color-t3)",
              }}
            />
            {CATEGORY_BADGE[group.category] ?? group.category.toUpperCase()}
          </span>
          <span
            style={{
              fontSize: 13,
              color: "var(--color-t1)",
              fontWeight: 500,
              overflow: "hidden",
              textOverflow: "ellipsis",
              whiteSpace: "nowrap",
            }}
          >
            {groupTitle(group)}
          </span>
        </div>
        <div style={{ fontSize: 11.5, color: "var(--color-t2)", marginTop: 5 }}>
          {group.images.length} files ·{" "}
          {CATEGORY_LABELS[group.category] ?? group.category} ·{" "}
          {fmtBytes(reclaim)} reclaimable
        </div>
      </div>

      <div
        style={{ display: "flex", alignItems: "center", gap: 14, flex: "none" }}
      >
        <span
          style={{
            fontSize: 11,
            fontWeight: 600,
            color: reviewed ? "var(--color-pick)" : "var(--color-t3)",
          }}
        >
          {reviewed ? "Reviewed" : "Pending"}
        </span>
        <span
          style={{
            fontSize: 12.5,
            color: "var(--color-accent)",
            fontWeight: 500,
          }}
        >
          Review →
        </span>
      </div>
    </div>
  );
}

export default function Overview({
  groups,
  keepers,
  resolved,
  stagedCount,
  loading,
  scanningSimilar,
  fillProgress,
  threshold,
  hasByteGroups,
  sortKey,
  sortDir,
  setSortKey,
  setSortDir,
  setThreshold,
  onRescan,
  onFindSimilar,
  onAutoResolve,
  onOpenGroup,
  onOpenBin,
}: OverviewProps) {
  const redundant = groups.reduce((s, g) => s + g.images.length - 1, 0);
  const reclaim = groups.reduce(
    (s, g) =>
      s + reclaimableBytes(g, keepers[g.key] ?? suggestKeeper(g.images)),
    0,
  );
  const reviewedCount = groups.filter((g) => resolved[g.key]).length;
  const summary =
    groups.length === 0
      ? "No duplicate groups detected."
      : `${groups.length} group${groups.length === 1 ? "" : "s"} · ${redundant} redundant photo${redundant === 1 ? "" : "s"} · ${fmtBytes(reclaim)} reclaimable`;
  const progress =
    groups.length === 0 ? 0 : (reviewedCount / groups.length) * 100;

  return (
    <div
      style={{
        flex: 1,
        minHeight: 0,
        overflowY: "auto",
        background: "var(--color-app)",
      }}
    >
      <div
        style={{ maxWidth: 1100, margin: "0 auto", padding: "28px 24px 60px" }}
      >
        <div
          style={{
            display: "flex",
            justifyContent: "space-between",
            alignItems: "flex-end",
            gap: 16,
            marginBottom: 6,
          }}
        >
          <div>
            <div
              style={{ fontSize: 22, fontWeight: 700, letterSpacing: "-.01em" }}
            >
              Duplicate review
            </div>
            <div
              style={{ fontSize: 13, color: "var(--color-t2)", marginTop: 5 }}
            >
              {summary}
            </div>
          </div>
          <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
            <label
              style={{
                display: "flex",
                alignItems: "center",
                gap: 7,
                fontSize: 12.5,
                color: "var(--color-t2)",
              }}
              title="Max differing dHash bits — lower = stricter"
            >
              Similarity
              <input
                type="range"
                min={2}
                max={20}
                value={threshold}
                disabled={scanningSimilar}
                onChange={(e) => setThreshold(Number(e.target.value))}
                style={{ width: 90, accentColor: "var(--color-accent)" }}
              />
              <span style={{ fontFamily: "var(--font-mono)", width: 18 }}>
                {threshold}
              </span>
            </label>
            <button
              className="tbtn ghost"
              onClick={onFindSimilar}
              disabled={scanningSimilar || loading}
              style={{ fontSize: 12.5 }}
            >
              {scanningSimilar
                ? fillProgress
                  ? `Hashing ${fillProgress.done}/${fillProgress.total}…`
                  : "Scanning…"
                : "Find similar"}
            </button>
            <button
              className="tbtn ghost"
              onClick={onRescan}
              disabled={loading || scanningSimilar}
              style={{ fontSize: 12.5 }}
              title="Re-run byte + capture duplicate scan"
            >
              {loading ? "Scanning…" : "↻ Rescan"}
            </button>
            <button
              className="tbtn ghost"
              onClick={onOpenBin}
              style={{ fontSize: 12.5 }}
            >
              Bin ·{" "}
              <span style={{ fontFamily: "var(--font-mono)" }}>
                {stagedCount}
              </span>
            </button>
            {hasByteGroups && (
              <button
                className="tbtn ghost"
                onClick={onAutoResolve}
                style={{ fontSize: 12.5 }}
                title="Keep largest copy of each exact duplicate and trash the rest"
              >
                Auto-resolve exact
              </button>
            )}
          </div>
        </div>

        <div
          style={{
            height: 8,
            background: "var(--color-panel)",
            borderRadius: 5,
            overflow: "hidden",
            margin: "16px 0 24px",
          }}
        >
          <div
            style={{
              height: "100%",
              width: `${progress}%`,
              background: "var(--color-pick)",
              transition: "width .2s",
            }}
          />
        </div>

        {loading ? (
          <div
            style={{
              padding: 60,
              textAlign: "center",
              color: "var(--color-t3)",
            }}
          >
            Scanning library…
          </div>
        ) : groups.length === 0 ? (
          <div
            style={{
              padding: 60,
              textAlign: "center",
              color: "var(--color-t3)",
              fontSize: 13,
              lineHeight: 1.7,
            }}
          >
            No exact or same-capture duplicates found.
            <br />
            Use{" "}
            <strong style={{ color: "var(--color-t2)" }}>
              Find similar
            </strong>{" "}
            to scan for near-duplicates by appearance.
          </div>
        ) : (
          <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
            <div
              style={{
                display: "flex",
                justifyContent: "flex-end",
                alignItems: "center",
                gap: 8,
                marginBottom: 2,
              }}
            >
              <span style={{ fontSize: 12, color: "var(--color-t3)" }}>
                Sort by
              </span>
              <select
                value={sortKey}
                onChange={(e) => setSortKey(e.target.value as SortKey)}
                style={{
                  background: "var(--color-elev)",
                  color: "var(--color-t1)",
                  border: "1px solid var(--color-line)",
                  borderRadius: "var(--radius-sm)",
                  padding: "5px 8px",
                  fontSize: 12.5,
                  cursor: "pointer",
                  outline: "none",
                }}
              >
                {SORT_KEYS.map((k) => (
                  <option key={k} value={k}>
                    {SORT_LABELS[k]}
                  </option>
                ))}
              </select>
              <button
                className="tbtn ghost"
                onClick={() => setSortDir(sortDir === "desc" ? "asc" : "desc")}
                style={{ fontSize: 12.5 }}
                title="Toggle sort direction"
              >
                {sortKey === "name"
                  ? sortDir === "asc"
                    ? "A–Z ↓"
                    : "Z–A ↑"
                  : sortDir === "desc"
                    ? "Most first ↓"
                    : "Least first ↑"}
              </button>
            </div>
            {groups.map((group, idx) => (
              <GroupRow
                key={group.key}
                group={group}
                keeperId={keepers[group.key] ?? suggestKeeper(group.images)}
                reviewed={!!resolved[group.key]}
                onOpen={() => onOpenGroup(idx)}
              />
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
