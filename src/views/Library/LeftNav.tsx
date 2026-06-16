import Icon, { IconName } from "../../components/Icon";
import {
  hasActiveFilters,
  type FolderRow,
  type KeywordRow,
  type QueryParams,
  type SortKey,
} from "../../lib/ipc";

interface LeftNavProps {
  folders: FolderRow[];
  keywords: KeywordRow[];
  grandTotal: number;
  params: QueryParams;
  clearFilters: () => void;
  patchParams: (patch: Partial<QueryParams>) => void;
  setSort: (sort: SortKey) => void;
}

function basename(p: string): string {
  return p.replace(/\/$/, "").split("/").pop() ?? p;
}

function SectionHeading({ children }: { children: React.ReactNode }) {
  return (
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
      {children}
    </div>
  );
}

export default function LeftNav({
  folders,
  keywords,
  grandTotal,
  params,
  clearFilters,
  patchParams,
  setSort,
}: LeftNavProps) {
  const activeFolderId = params.folderId ?? null;
  const noFilters = !hasActiveFilters(params);
  const picksActive = params.flag === "pick";
  const recentActive = params.sort === "imported_desc";

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
          count={grandTotal.toLocaleString()}
          active={noFilters}
          onClick={clearFilters}
        />
        <NavRow
          icon="flag"
          label="Picks"
          count=""
          active={picksActive}
          onClick={() =>
            patchParams({ flag: picksActive ? null : "pick" })
          }
        />
        <NavRow
          icon="clock"
          label="Recent import"
          count=""
          active={recentActive}
          onClick={() =>
            setSort(recentActive ? "capture_desc" : "imported_desc")
          }
        />
      </div>

      {/* Folders section */}
      <div>
        <SectionHeading>Folders</SectionHeading>
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
              onClick={() =>
                patchParams({
                  folderId: activeFolderId === f.id ? null : f.id,
                })
              }
            />
          ))
        )}
      </div>

      {/* Keywords section */}
      {keywords.length > 0 && (
        <div>
          <SectionHeading>Keywords</SectionHeading>
          {keywords.map((kw) => (
            <NavRow
              key={kw.id}
              icon="tag"
              label={kw.name}
              count={kw.count.toLocaleString()}
              active={params.keywordId === kw.id}
              onClick={() =>
                patchParams({
                  keywordId: params.keywordId === kw.id ? null : kw.id,
                })
              }
            />
          ))}
        </div>
      )}
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
