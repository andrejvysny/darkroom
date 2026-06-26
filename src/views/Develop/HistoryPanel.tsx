import { useState } from "react";
import type { HistoryApi } from "./useHistory";

interface Props {
  undo: () => void;
  redo: () => void;
  canUndo: boolean;
  canRedo: boolean;
  onRevertOpened: () => void;
  onResetDefault: () => void;
  history: HistoryApi;
}

const actionBtn = (enabled: boolean): React.CSSProperties => ({
  flex: 1,
  border: "1px solid var(--color-line-2)",
  background: "var(--color-elev)",
  color: enabled ? "var(--color-t1)" : "var(--color-t3)",
  borderRadius: "var(--radius-sm)",
  padding: "6px 0",
  fontSize: 11.5,
  cursor: enabled ? "pointer" : "default",
  opacity: enabled ? 1 : 0.55,
});

const linkBtn: React.CSSProperties = {
  width: "100%",
  textAlign: "left",
  border: "1px solid var(--color-line-2)",
  background: "var(--color-elev)",
  color: "var(--color-t1)",
  borderRadius: "var(--radius-sm)",
  padding: "7px 10px",
  fontSize: 12,
  cursor: "pointer",
};

/** Edit history: session undo/redo + revert/reset + persistent named snapshots. */
export default function HistoryPanel({
  undo,
  redo,
  canUndo,
  canRedo,
  onRevertOpened,
  onResetDefault,
  history,
}: Props) {
  const [adding, setAdding] = useState(false);
  const [name, setName] = useState("");
  const [renamingId, setRenamingId] = useState<number | null>(null);
  const [draft, setDraft] = useState("");

  const submitNew = () => {
    void history.createSnapshot(name);
    setName("");
    setAdding(false);
  };
  const commitRename = () => {
    if (renamingId !== null && draft.trim()) {
      void history.renameSnapshot(renamingId, draft.trim());
    }
    setRenamingId(null);
  };

  return (
    <div style={{ display: "flex", flexDirection: "column" }}>
      {/* Undo / redo / revert / reset */}
      <div
        style={{
          padding: "12px 14px",
          borderBottom: "1px solid var(--color-line)",
          display: "flex",
          flexDirection: "column",
          gap: 8,
        }}
      >
        <div style={{ display: "flex", gap: 6 }}>
          <button style={actionBtn(canUndo)} disabled={!canUndo} onClick={undo}>
            ↶ Undo
          </button>
          <button style={actionBtn(canRedo)} disabled={!canRedo} onClick={redo}>
            Redo ↷
          </button>
        </div>
        <button style={linkBtn} onClick={onRevertOpened}>
          Revert to opened state
        </button>
        <button style={linkBtn} onClick={onResetDefault}>
          Reset to default
        </button>
      </div>

      {/* Snapshots */}
      <div
        style={{
          display: "flex",
          alignItems: "center",
          padding: "10px 10px 8px 14px",
          gap: 4,
        }}
      >
        <span
          style={{
            flex: 1,
            fontSize: 11,
            color: "var(--color-t3)",
            fontWeight: 600,
          }}
        >
          SNAPSHOTS
        </span>
        <button
          title="Create snapshot"
          onClick={() => {
            setAdding(true);
            setName("");
          }}
          style={{
            background: "none",
            border: "none",
            color: "var(--color-t2)",
            cursor: "pointer",
            fontSize: 18,
            lineHeight: "14px",
            padding: 4,
          }}
        >
          +
        </button>
      </div>

      {adding && (
        <div style={{ display: "flex", gap: 6, padding: "0 14px 10px" }}>
          <input
            autoFocus
            value={name}
            placeholder="Snapshot name"
            onChange={(e) => setName(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") submitNew();
              if (e.key === "Escape") setAdding(false);
            }}
            style={{
              flex: 1,
              background: "var(--color-elev)",
              border: "1px solid var(--color-accent)",
              borderRadius: "var(--radius-sm)",
              padding: "5px 8px",
              fontSize: 12,
              color: "var(--color-t1)",
              outline: "none",
            }}
          />
          <button
            onClick={submitNew}
            style={{
              border: "none",
              background: "var(--color-accent)",
              color: "#fff",
              borderRadius: "var(--radius-sm)",
              padding: "0 12px",
              fontSize: 12,
              cursor: "pointer",
            }}
          >
            Save
          </button>
        </div>
      )}

      {history.snapshots.length === 0 && !adding && (
        <div
          style={{
            padding: "0 14px 14px",
            fontSize: 12,
            color: "var(--color-t3)",
          }}
        >
          No snapshots. Save one to return to this look later.
        </div>
      )}

      {history.snapshots.map((s) => (
        <div
          key={s.id}
          style={{
            display: "flex",
            alignItems: "center",
            gap: 6,
            padding: "6px 12px 6px 14px",
            fontSize: 12.5,
            color: "var(--color-t1)",
          }}
        >
          {renamingId === s.id ? (
            <input
              autoFocus
              value={draft}
              onChange={(e) => setDraft(e.target.value)}
              onBlur={commitRename}
              onKeyDown={(e) => {
                if (e.key === "Enter") commitRename();
                if (e.key === "Escape") setRenamingId(null);
              }}
              style={{
                flex: 1,
                background: "var(--color-elev)",
                border: "1px solid var(--color-accent)",
                borderRadius: "var(--radius-sm)",
                padding: "3px 6px",
                fontSize: 12.5,
                color: "var(--color-t1)",
                outline: "none",
              }}
            />
          ) : (
            <span
              style={{ flex: 1, cursor: "pointer" }}
              title="Restore snapshot"
              onClick={() => void history.restoreSnapshot(s.id)}
              onDoubleClick={() => {
                setRenamingId(s.id);
                setDraft(s.name);
              }}
            >
              {s.name}
            </span>
          )}
          <button
            title="Delete snapshot"
            onClick={() => void history.removeSnapshot(s.id)}
            style={{
              background: "none",
              border: "none",
              color: "var(--color-t3)",
              cursor: "pointer",
              fontSize: 14,
              padding: "2px 4px",
            }}
          >
            ✕
          </button>
        </div>
      ))}
    </div>
  );
}
