import { useState } from "react";
import Icon, { IconName } from "../../components/Icon";
import PeopleNav from "./PeopleNav";
import {
  hasActiveFilters,
  clearedFilters,
  parseSmartQuery,
  smartQueryFromParams,
  DETECTION_CATEGORIES,
  type FolderRow,
  type KeywordRow,
  type CollectionRow,
  type QueryParams,
  type SortKey,
  type FacetRow,
} from "../../lib/ipc";
import type { AnalysisState, AnalysisActions } from "../../lib/useAnalysis";

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
  onRenameCollection: (id: number, name: string) => void;
  onDeleteKeyword: (id: number) => void;
  /** AI analysis facets + actions — passed from LibraryView via useAnalysis */
  analysis: AnalysisState & AnalysisActions;
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
  onRenameCollection,
  onDeleteKeyword,
  analysis,
}: LeftNavProps) {
  const activeFolderId = params.folderId ?? null;
  const noFilters = !hasActiveFilters(params);
  const picksActive = params.flag === "pick";
  const recentActive = params.sort === "imported_desc";

  const staticCollections = collections.filter((c) => !c.isSmart);
  const smartCollections = collections.filter((c) => c.isSmart);
  const currentPredicate = smartQueryFromParams(params);
  // A predicate worth saving (excludes search/collectionId, which smart collections don't capture).
  const hasPredicate = currentPredicate !== "{}";

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
            onRename={(name) => onRenameCollection(c.id, name)}
          />
        ))}
        <CreateRow
          placeholder="New collection…"
          onSubmit={onCreateCollection}
        />
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
            onRename={(name) => onRenameCollection(c.id, name)}
          />
        ))}
        {hasPredicate ? (
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
              onDelete={() => onDeleteKeyword(kw.id)}
            />
          ))}
        </div>
      )}

      {/* People section */}
      <PeopleNav
        params={params}
        patchParams={patchParams}
        clearFilters={clearFilters}
      />

      {/* Detected section */}
      <div>
        <SectionHeading
          action={
            <AnalyzeButton
              running={
                analysis.status?.running === true || analysis.progress !== null
              }
              onAnalyze={() => void analysis.triggerAnalysis(false)}
              onReanalyze={() => void analysis.triggerAnalysis(true)}
            />
          }
        >
          Detected
        </SectionHeading>
        {DETECTION_CATEGORIES.map((cat) => {
          const row: FacetRow | undefined = analysis.facets.find(
            (f) => f.category === cat,
          );
          const count = row?.count ?? 0;
          const active = params.detectedCategory === cat;
          return (
            <NavRow
              key={cat}
              icon="scan"
              label={cat}
              count={count.toLocaleString()}
              active={active}
              onClick={() =>
                patchParams({ detectedCategory: active ? null : cat })
              }
            />
          );
        })}
      </div>
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
  onRename?: (name: string) => void;
}

function NavRow({
  icon,
  label,
  count,
  active,
  child,
  onClick,
  onDelete,
  onRename,
}: NavRowProps) {
  const [hover, setHover] = useState(false);
  const [renaming, setRenaming] = useState(false);
  const [draft, setDraft] = useState(label);

  if (renaming) {
    const commit = () => {
      const name = draft.trim();
      if (name && name !== label) onRename?.(name);
      setRenaming(false);
    };
    return (
      <input
        autoFocus
        value={draft}
        onChange={(e) => setDraft(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            commit();
          } else if (e.key === "Escape") {
            setRenaming(false);
          }
        }}
        onBlur={commit}
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

  const showActions = hover && (onDelete != null || onRename != null);
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
      {showActions ? (
        <span style={{ marginLeft: "auto", display: "flex", gap: 4 }}>
          {onRename && (
            <button
              onClick={(e) => {
                e.stopPropagation();
                setDraft(label);
                setRenaming(true);
              }}
              title={`Rename ${label}`}
              aria-label={`Rename ${label}`}
              style={iconBtn()}
            >
              <Icon name="edit" size={11} />
            </button>
          )}
          {onDelete && (
            <button
              onClick={(e) => {
                e.stopPropagation();
                onDelete();
              }}
              title={`Delete ${label}`}
              aria-label={`Delete ${label}`}
              style={{ ...iconBtn(), fontSize: 13 }}
            >
              ×
            </button>
          )}
        </span>
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

function iconBtn(): React.CSSProperties {
  return {
    display: "flex",
    alignItems: "center",
    border: "none",
    background: "transparent",
    color: "var(--color-t3)",
    lineHeight: 1,
    cursor: "pointer",
    padding: "0 2px",
  };
}

/** Small header-action button for the Detected section. */
function AnalyzeButton({
  running,
  onAnalyze,
  onReanalyze,
}: {
  running: boolean;
  onAnalyze: () => void;
  onReanalyze: () => void;
}) {
  const [hover, setHover] = useState(false);

  return (
    <span style={{ display: "flex", gap: 3 }}>
      <button
        disabled={running}
        onClick={(e) => {
          e.stopPropagation();
          onAnalyze();
        }}
        onMouseEnter={() => setHover(false)}
        title="Analyze new images"
        aria-label="Analyze new images"
        style={{
          fontSize: 10,
          padding: "1px 6px",
          borderRadius: "var(--radius-sm)",
          border: "1px solid var(--color-line)",
          background: "transparent",
          color: running ? "var(--color-t3)" : "var(--color-t2)",
          cursor: running ? "default" : "pointer",
          lineHeight: 1.5,
          opacity: running ? 0.5 : 1,
        }}
      >
        {running ? "Running…" : "Analyze"}
      </button>
      {!running && (
        <button
          onClick={(e) => {
            e.stopPropagation();
            onReanalyze();
          }}
          onMouseEnter={() => setHover(true)}
          onMouseLeave={() => setHover(false)}
          title="Re-analyze all images"
          aria-label="Re-analyze all images"
          style={{
            fontSize: 10,
            padding: "1px 5px",
            borderRadius: "var(--radius-sm)",
            border: "1px solid var(--color-line)",
            background: hover ? "var(--color-hover)" : "transparent",
            color: "var(--color-t3)",
            cursor: "pointer",
            lineHeight: 1.5,
          }}
        >
          ↺
        </button>
      )}
    </span>
  );
}
