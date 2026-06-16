import Icon, { IconName } from "../../components/Icon";
import type { FolderRow, QueryParams } from "../../lib/ipc";

interface StaticNavItem {
  icon: IconName;
  label: string;
  count: string;
  active?: boolean;
}

interface StaticSection {
  heading: string;
  items: StaticNavItem[];
}

const STATIC_SECTIONS: StaticSection[] = [
  {
    heading: "Collections",
    items: [
      { icon: "stack", label: "Portfolio", count: "96" },
      { icon: "stack", label: "Print queue", count: "12" },
    ],
  },
  {
    heading: "Smart",
    items: [
      { icon: "bolt", label: "★★★★+", count: "2,051" },
      { icon: "bolt", label: "Untagged", count: "7,330" },
    ],
  },
];

interface LeftNavProps {
  folders: FolderRow[];
  total: number;
  params: QueryParams;
  setFolderId: (id: number | null) => void;
}

function basename(p: string): string {
  return p.replace(/\/$/, "").split("/").pop() ?? p;
}

export default function LeftNav({
  folders,
  total,
  params,
  setFolderId,
}: LeftNavProps) {
  const activeFolderId = params.folderId ?? null;

  return (
    <aside
      style={{
        background: "var(--color-app)",
        borderRight: "1px solid var(--color-line)",
        padding: "10px 8px",
        overflowY: "auto",
        minHeight: 0,
        height: "100%",
      }}
    >
      {/* Catalog section */}
      <div>
        <div
          style={{
            fontSize: 10.5,
            letterSpacing: ".06em",
            textTransform: "uppercase",
            color: "var(--color-t3)",
            fontWeight: 600,
            padding: "2px 8px 6px",
          }}
        >
          Catalog
        </div>
        <NavRow
          icon="photos"
          label="All photos"
          count={total.toLocaleString()}
          active={activeFolderId === null}
          onClick={() => setFolderId(null)}
        />
        <NavRow icon="flag" label="Picks" count="" />
        <NavRow icon="clock" label="Recent import" count="" />
      </div>

      {/* Folders section */}
      <div>
        <div
          style={{
            fontSize: 10.5,
            letterSpacing: ".06em",
            textTransform: "uppercase",
            color: "var(--color-t3)",
            fontWeight: 600,
            padding: "12px 8px 6px",
          }}
        >
          Folders
        </div>
        {folders.length === 0 ? (
          <div
            style={{
              fontSize: 12,
              color: "var(--color-t3)",
              padding: "4px 8px",
            }}
          >
            No folders indexed
          </div>
        ) : (
          folders.map((f) => (
            <NavRow
              key={f.id}
              icon="folder"
              label={basename(f.path)}
              count={f.count.toLocaleString()}
              active={activeFolderId === f.id}
              onClick={() => setFolderId(f.id)}
            />
          ))
        )}
      </div>

      {/* Static sections */}
      {STATIC_SECTIONS.map((section) => (
        <div key={section.heading}>
          <div
            style={{
              fontSize: 10.5,
              letterSpacing: ".06em",
              textTransform: "uppercase",
              color: "var(--color-t3)",
              fontWeight: 600,
              padding: "12px 8px 6px",
            }}
          >
            {section.heading}
          </div>
          {section.items.map((item) => (
            <NavRow
              key={item.label}
              icon={item.icon}
              label={item.label}
              count={item.count}
              active={item.active}
            />
          ))}
        </div>
      ))}
    </aside>
  );
}

interface NavRowProps {
  icon: IconName;
  label: string;
  count: string;
  active?: boolean;
  child?: boolean;
  onClick?: () => void;
}

function NavRow({ icon, label, count, active, child, onClick }: NavRowProps) {
  return (
    <div
      onClick={onClick}
      style={{
        display: "flex",
        alignItems: "center",
        gap: 9,
        padding: child ? "6px 8px 6px 24px" : "6px 8px",
        borderRadius: "var(--radius-sm)",
        color: active ? "var(--color-t1)" : "var(--color-t2)",
        fontSize: child ? 12 : 12.5,
        cursor: onClick ? "pointer" : "default",
        position: "relative",
        background: active ? "var(--color-accent-dim)" : "transparent",
      }}
    >
      {active && (
        <span
          style={{
            position: "absolute",
            left: 0,
            top: 6,
            bottom: 6,
            width: 2,
            borderRadius: 2,
            background: "var(--color-accent)",
          }}
        />
      )}
      <Icon
        name={icon}
        style={
          {
            color: active ? "var(--color-t2)" : "var(--color-t3)",
            width: 14,
            height: 14,
            flexShrink: 0,
          } as React.CSSProperties
        }
      />
      {label}
      {count && (
        <span
          style={{
            marginLeft: "auto",
            fontFamily: "var(--font-mono)",
            fontSize: 11,
            color: "var(--color-t3)",
          }}
        >
          {count}
        </span>
      )}
    </div>
  );
}
