import { useState } from "react";
import Icon, { IconName } from "../../components/Icon";
import {
  hasActiveFilters,
  clearedFilters,
  parseSmartQuery,
  smartQueryFromParams,
  type FolderRow,
  type KeywordRow,
  type CollectionRow,
  type QueryParams,
  type SortKey,
} from "../../lib/ipc";

interface LeftNavProps {
  folders: FolderRow[];
  keywords: KeywordRow[];
  collections: CollectionRow[];
  grandTotal: number;
  params: QueryParams;
  clearFilters: () => void;
  patchParams: (patch: Partial<QueryParams>) => void;
  setSort: (sort: SortKey) => void;
  onCreateCollection: (name: string) => void;
  onCreateSmartCollection: (name: string) => void;
  onDeleteCollection: (id: number) => void;
}

function basename(p: string): string {
  return p.replace(/\/$/, "").split("/").pop() ?? p;
}

function SectionHeading({
  children,
  action,
}: {
  children: React.ReactNode;
  action?: React.ReactNode;
}) {
  return (
    <div
      style={{
        display: "flex",
        alignItems: "center",
        justifyContent: "space-between",
        fontSize: 10.5,
        letterSpacing: ".06em",
        textTransform: "uppercase",
        color: "var(--color-t3)",
        fontWeight: 600,
        padding: "12px 8px 6px",
      }}
    >
      <span>{children}</span>
      {action}
    </div>
  );
}

export default function LeftNav({
  folders,
  keywords,
  collections,
  grandTotal,
  params,
  clearFilters,
  patchParams,
  setSort,
  onCreateCollection,
  onCreateSmartCollection,
  onDeleteCollection,
}: LeftNavProps) {
  const activeFolderId = params.folderId ?? null;
  const noFilters = !hasActiveFilters(params);
  const picksActive = params.flag === "pick";
  const recentActive = params.sort === "imported_desc";

  const staticCollections = collections.filter((c) => !c.isSmart);
  const smartCollections = collections.filter((c) => c.isSmart);
  const currentPredicate = smartQueryFromParams(params);

  function enterCollection(id: number) {
    if (params.collectionId === id) {
      clearFilters();
    } else {
      patchParams({ ...clearedFilters(), collectionId: id });
    }
  }

  function applySmart(c: CollectionRow) {
    if (c.query === currentPredicate) {
      clearFilters();
    } else {
      patchParams({ ...clearedFilters(), ...parseSmartQuery(c.query) });
    }
  }

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
          onClick={() => patchParams({ flag: picksActive ? null : "pick" })}
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
          <Empty>No folders indexed</Empty>
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

      {/* Collections section */}
      <div>
        <SectionHeading>Collections</SectionHeading>
        {staticCollections.map((c) => (
          <NavRow
            key={c.id}
            icon="stack"
            label={c.name}
            count={c.count.toLocaleString()}
            active={params.collectionId === c.id}
            onClick={() => enterCollection(c.id)}
            onDelete={() => onDeleteCollection(c.id)}
          />
        ))}
        <CreateRow placeholder="New collection…" onSubmit={onCreateCollection} />
      </div>

      {/* Smart collections section */}
      <div>
        <SectionHeading>Smart</SectionHeading>
        {smartCollections.map((c) => (
          <NavRow
            key={c.id}
            icon="bolt"
            label={c.name}
            count={c.count.toLocaleString()}
            active={c.query !== null && c.query === currentPredicate}
            onClick={() => applySmart(c)}
            onDelete={() => onDeleteCollection(c.id)}
          />
        ))}
        {hasActiveFilters(params) ? (
          <CreateRow
            placeholder="Save filters as…"
            onSubmit={onCreateSmartCollection}
          />
        ) : (
          smartCollections.length === 0 && (
            <Empty>Filter, then save as smart</Empty>
          )
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

function Empty({ children }: { children: React.ReactNode }) {
  return (
    <div style={{ fontSize: 12, color: "var(--color-t3)", padding: "4px 8px" }}>
      {children}
    </div>
  );
}

/** Inline "+ create" affordance: a trigger that expands into a text input. */
function CreateRow({
  placeholder,
  onSubmit,
}: {
  placeholder: string;
  onSubmit: (name: string) => void;
}) {
  const [editing, setEditing] = useState(false);
  const [value, setValue] = useState("");

  function commit() {
    const name = value.trim();
    if (name) onSubmit(name);
    setValue("");
    setEditing(false);
  }

  if (!editing) {
    return (
      <div
        onClick={() => setEditing(true)}
        style={{
          display: "flex",
          alignItems: "center",
          gap: 9,
          padding: "6px 8px",
          borderRadius: "var(--radius-sm)",
          color: "var(--color-t3)",
          fontSize: 12.5,
          cursor: "pointer",
        }}
      >
        <span style={{ width: 14, textAlign: "center" }}>+</span>
        {placeholder}
      </div>
    );
  }

  return (
    <input
      autoFocus
      value={value}
      onChange={(e) => setValue(e.target.value)}
      onKeyDown={(e) => {
        if (e.key === "Enter") {
          e.preventDefault();
          commit();
        } else if (e.key === "Escape") {
          setValue("");
          setEditing(false);
        }
      }}
      onBlur={commit}
      placeholder={placeholder}
      style={{
        width: "100%",
        background: "var(--color-panel)",
        border: "1px solid var(--color-accent-line)",
        borderRadius: "var(--radius-sm)",
        color: "var(--color-t1)",
        fontSize: 12.5,
        padding: "5px 8px",
        outline: "none",
      }}
    />
  );
}

interface NavRowProps {
  icon: IconName;
  label: string;
  count: string;
  active?: boolean;
  child?: boolean;
  onClick?: () => void;
  onDelete?: () => void;
}

function NavRow({
  icon,
  label,
  count,
  active,
  child,
  onClick,
  onDelete,
}: NavRowProps) {
  const [hover, setHover] = useState(false);
  return (
    <div
      onClick={onClick}
      onMouseEnter={() => setHover(true)}
      onMouseLeave={() => setHover(false)}
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
      <span
        style={{
          overflow: "hidden",
          textOverflow: "ellipsis",
          whiteSpace: "nowrap",
        }}
      >
        {label}
      </span>
      {onDelete && hover ? (
        <button
          onClick={(e) => {
            e.stopPropagation();
            onDelete();
          }}
          title={`Delete ${label}`}
          aria-label={`Delete ${label}`}
          style={{
            marginLeft: "auto",
            border: "none",
            background: "transparent",
            color: "var(--color-t3)",
            fontSize: 13,
            lineHeight: 1,
            cursor: "pointer",
            padding: "0 2px",
          }}
        >
          ×
        </button>
      ) : (
        count && (
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
        )
      )}
    </div>
  );
}
