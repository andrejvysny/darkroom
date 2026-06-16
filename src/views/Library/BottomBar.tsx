import Icon from "../../components/Icon";
import { useAppStore } from "../../store/app";
import type { QueryParams } from "../../lib/ipc";

interface BottomBarProps {
  total: number;
  params: QueryParams;
  setSort: (sort: QueryParams["sort"]) => void;
  setMinStars: (minStars: number | null) => void;
  setFlag: (flag: string | null) => void;
}

const SORT_LABELS: Record<NonNullable<QueryParams["sort"]>, string> = {
  capture_desc: "Capture date ↓",
  capture_asc: "Capture date ↑",
  filename: "Filename",
};

const SORT_CYCLE: NonNullable<QueryParams["sort"]>[] = [
  "capture_desc",
  "capture_asc",
  "filename",
];

export default function BottomBar({
  total,
  params,
  setSort,
  setMinStars,
  setFlag,
}: BottomBarProps) {
  const thumbSize = useAppStore((s) => s.thumbSize);
  const setThumbSize = useAppStore((s) => s.setThumbSize);
  const gridMode = useAppStore((s) => s.gridMode);
  const setGridMode = useAppStore((s) => s.setGridMode);

  const currentSort = params.sort ?? "capture_desc";
  const starsActive = params.minStars === 3;
  const picksActive = params.flag === "pick";
  const unratedActive = params.minStars === 0 && params.flag === null;

  function cycleSort() {
    const idx = SORT_CYCLE.indexOf(currentSort);
    const next = SORT_CYCLE[(idx + 1) % SORT_CYCLE.length];
    setSort(next);
  }

  function toggleStars() {
    setMinStars(starsActive ? null : 3);
    if (!starsActive) setFlag(null);
  }

  function togglePicks() {
    setFlag(picksActive ? null : "pick");
    if (!picksActive) setMinStars(null);
  }

  function toggleUnrated() {
    if (unratedActive) {
      setMinStars(null);
    } else {
      setMinStars(0);
      setFlag(null);
    }
  }

  const filters: {
    key: string;
    icon?: React.ReactNode;
    label: string;
    active: boolean;
    onToggle: () => void;
  }[] = [
    {
      key: "stars",
      icon: <Icon name="star" size={12} />,
      label: "≥ 3",
      active: starsActive,
      onToggle: toggleStars,
    },
    {
      key: "picks",
      icon: <Icon name="flag" size={12} />,
      label: "Picks",
      active: picksActive,
      onToggle: togglePicks,
    },
    {
      key: "unrated",
      label: "Unrated",
      active: unratedActive,
      onToggle: toggleUnrated,
    },
  ];

  return (
    <footer
      style={{
        display: "flex",
        alignItems: "center",
        gap: 16,
        padding: "0 14px",
        background: "var(--color-app)",
        borderTop: "1px solid var(--color-line)",
        fontSize: 12,
        color: "var(--color-t2)",
        height: 40,
        flexShrink: 0,
      }}
    >
      <span
        style={{
          fontFamily: "var(--font-mono)",
          fontSize: 11.5,
          color: "var(--color-t3)",
        }}
      >
        {total.toLocaleString()} photos
      </span>

      <div style={{ display: "flex", alignItems: "center", gap: 6 }}>
        {filters.map(({ key, icon, label, active, onToggle }) => (
          <button
            key={key}
            onClick={onToggle}
            style={{
              display: "flex",
              alignItems: "center",
              gap: 5,
              padding: "4px 9px",
              borderRadius: 20,
              border: "1px solid",
              borderColor: active
                ? "var(--color-accent-line)"
                : "var(--color-line)",
              background: active ? "var(--color-accent-dim)" : "transparent",
              color: active ? "var(--color-t1)" : "var(--color-t2)",
              fontSize: 12,
              cursor: "pointer",
            }}
          >
            {icon}
            {label}
          </button>
        ))}
      </div>

      <div
        style={{
          display: "flex",
          alignItems: "center",
          gap: 8,
          marginLeft: "auto",
        }}
      >
        <button
          onClick={cycleSort}
          style={{
            color: "var(--color-t3)",
            fontSize: 12,
            cursor: "pointer",
            background: "none",
            border: "none",
          }}
        >
          Sort: {SORT_LABELS[currentSort]}
        </button>
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
          style={{ width: 96 }}
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
