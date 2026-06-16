import Icon from "../../components/Icon";
import { useAppStore } from "../../store/app";
import {
  LABEL_NONE,
  hasActiveFilters,
  type QueryParams,
  type SortKey,
} from "../../lib/ipc";

interface BottomBarProps {
  total: number;
  params: QueryParams;
  patchParams: (patch: Partial<QueryParams>) => void;
  clearFilters: () => void;
  setSort: (sort: SortKey) => void;
}

const SORT_LABELS: Record<SortKey, string> = {
  capture_desc: "Capture date ↓",
  capture_asc: "Capture date ↑",
  filename: "Filename A→Z",
  filename_desc: "Filename Z→A",
  rating_desc: "Rating ↓",
  rating_asc: "Rating ↑",
  imported_desc: "Recently added",
  imported_asc: "Oldest added",
};

const LABEL_SWATCHES: { key: string; bg: string }[] = [
  { key: "red", bg: "var(--color-lab-red)" },
  { key: "yellow", bg: "var(--color-lab-yellow)" },
  { key: "green", bg: "var(--color-lab-green)" },
  { key: "blue", bg: "var(--color-lab-blue)" },
  { key: "purple", bg: "var(--color-lab-purple)" },
];

function Divider() {
  return (
    <span
      style={{
        width: 1,
        height: 18,
        background: "var(--color-line)",
        flexShrink: 0,
      }}
    />
  );
}

export default function BottomBar({
  total,
  params,
  patchParams,
  clearFilters,
  setSort,
}: BottomBarProps) {
  const thumbSize = useAppStore((s) => s.thumbSize);
  const setThumbSize = useAppStore((s) => s.setThumbSize);
  const gridMode = useAppStore((s) => s.gridMode);
  const setGridMode = useAppStore((s) => s.setGridMode);

  const currentSort = params.sort ?? "capture_desc";
  const minStars = params.minStars ?? null;
  const flag = params.flag ?? null;
  const label = params.colorLabel ?? null;

  const anyFilter = hasActiveFilters(params);

  function toggleStar(n: number) {
    patchParams({ minStars: minStars === n ? null : n });
  }
  function toggleFlag(f: "pick" | "reject") {
    patchParams({ flag: flag === f ? null : f });
  }
  function toggleLabel(key: string) {
    patchParams({ colorLabel: label === key ? null : key });
  }

  return (
    <footer
      style={{
        display: "flex",
        alignItems: "center",
        gap: 12,
        padding: "0 14px",
        background: "var(--color-app)",
        borderTop: "1px solid var(--color-line)",
        fontSize: 12,
        color: "var(--color-t2)",
        height: 40,
        flexShrink: 0,
        overflow: "hidden",
      }}
    >
      <span
        style={{
          fontFamily: "var(--font-mono)",
          fontSize: 11.5,
          color: "var(--color-t3)",
          whiteSpace: "nowrap",
        }}
      >
        {total.toLocaleString()} photos
      </span>

      <Divider />

      {/* Star threshold filter */}
      <div
        style={{ display: "flex", alignItems: "center", gap: 1 }}
        title="Filter by minimum rating"
      >
        {[1, 2, 3, 4, 5].map((n) => (
          <svg
            key={n}
            viewBox="0 0 16 16"
            width={13}
            height={13}
            fill={minStars != null && n <= minStars ? "var(--color-star)" : "none"}
            stroke={
              minStars != null && n <= minStars
                ? "var(--color-star)"
                : "var(--color-t3)"
            }
            strokeWidth="1.2"
            style={{ cursor: "pointer", display: "block" }}
            onClick={() => toggleStar(n)}
          >
            <path d="M8 2.2l1.7 3.6 3.9.5-2.9 2.7.8 3.9L8 11l-3.5 1.9.8-3.9L2.4 6.3l3.9-.5z" />
          </svg>
        ))}
      </div>

      <Divider />

      {/* Flag filters */}
      <div style={{ display: "flex", alignItems: "center", gap: 6 }}>
        <button
          onClick={() => toggleFlag("pick")}
          title="Show picks"
          style={pillStyle(flag === "pick")}
        >
          <Icon name="flag" size={12} />
          Picks
        </button>
        <button
          onClick={() => toggleFlag("reject")}
          title="Show rejects"
          style={pillStyle(flag === "reject")}
        >
          Rejects
        </button>
      </div>

      <Divider />

      {/* Color label filters */}
      <div style={{ display: "flex", alignItems: "center", gap: 6 }}>
        {LABEL_SWATCHES.map(({ key, bg }) => (
          <span
            key={key}
            onClick={() => toggleLabel(key)}
            title={`Filter: ${key}`}
            style={{
              width: 11,
              height: 11,
              borderRadius: "50%",
              background: bg,
              boxShadow:
                label === key
                  ? "0 0 0 2px var(--color-accent-line)"
                  : "0 0 0 1.5px rgba(0,0,0,.35)",
              cursor: "pointer",
              display: "block",
              flexShrink: 0,
            }}
          />
        ))}
        <span
          onClick={() => toggleLabel(LABEL_NONE)}
          title="Filter: unlabeled"
          style={{
            width: 11,
            height: 11,
            borderRadius: "50%",
            background: "transparent",
            border: "1px solid var(--color-t3)",
            boxShadow:
              label === LABEL_NONE ? "0 0 0 2px var(--color-accent-line)" : "none",
            cursor: "pointer",
            display: "block",
            flexShrink: 0,
          }}
        />
      </div>

      {anyFilter && (
        <button
          onClick={clearFilters}
          title="Clear all filters"
          style={{
            ...pillStyle(false),
            color: "var(--color-t3)",
            borderColor: "transparent",
          }}
        >
          Clear
        </button>
      )}

      <div
        style={{
          display: "flex",
          alignItems: "center",
          gap: 8,
          marginLeft: "auto",
        }}
      >
        <label
          style={{ display: "flex", alignItems: "center", gap: 5 }}
          title="Sort order"
        >
          <span style={{ color: "var(--color-t3)", whiteSpace: "nowrap" }}>
            Sort
          </span>
          <select
            value={currentSort}
            onChange={(e) => setSort(e.target.value as SortKey)}
            style={{
              background: "var(--color-elev)",
              border: "1px solid var(--color-line)",
              borderRadius: "var(--radius-sm)",
              color: "var(--color-t1)",
              fontSize: 12,
              padding: "3px 6px",
              cursor: "pointer",
            }}
          >
            {(Object.keys(SORT_LABELS) as SortKey[]).map((k) => (
              <option key={k} value={k}>
                {SORT_LABELS[k]}
              </option>
            ))}
          </select>
        </label>
        <Icon
          name="grid-sm"
          style={{ color: "var(--color-t3)" } as React.CSSProperties}
        />
        <input
          type="range"
          min={110}
          max={240}
          value={thumbSize}
          onChange={(e) => setThumbSize(Number(e.target.value))}
          style={{ width: 80 }}
        />
        <Icon
          name="grid-lg"
          style={{ color: "var(--color-t3)" } as React.CSSProperties}
        />
        <div
          style={{
            display: "flex",
            background: "var(--color-elev)",
            border: "1px solid var(--color-line)",
            borderRadius: "var(--radius-sm)",
            padding: 2,
          }}
        >
          {(["grid", "loupe"] as const).map((v) => (
            <button
              key={v}
              onClick={() => setGridMode(v)}
              title={v === "grid" ? "Grid" : "Loupe"}
              style={{
                padding: "3px 8px",
                borderRadius: 4,
                color: gridMode === v ? "var(--color-t1)" : "var(--color-t3)",
                background:
                  gridMode === v ? "var(--color-hover)" : "transparent",
              }}
            >
              <Icon name={v === "grid" ? "grid" : "square"} size={14} />
            </button>
          ))}
        </div>
      </div>
    </footer>
  );
}

function pillStyle(active: boolean): React.CSSProperties {
  return {
    display: "flex",
    alignItems: "center",
    gap: 5,
    padding: "4px 9px",
    borderRadius: 20,
    border: "1px solid",
    borderColor: active ? "var(--color-accent-line)" : "var(--color-line)",
    background: active ? "var(--color-accent-dim)" : "transparent",
    color: active ? "var(--color-t1)" : "var(--color-t2)",
    fontSize: 12,
    cursor: "pointer",
    whiteSpace: "nowrap",
  };
}
