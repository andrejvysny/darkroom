import { useEffect, useState } from "react";
import {
  SCOPE_GROUPS,
  DEFAULT_PRESET_GROUPS,
  fieldsForGroups,
} from "../../lib/presetScope";

interface Props {
  open: boolean;
  onClose: () => void;
  onSave: (
    name: string,
    group: string | undefined,
    fieldKeys: string[],
    isFavorite: boolean,
  ) => void;
}

const overlay: React.CSSProperties = {
  position: "fixed",
  inset: 0,
  background: "rgba(0,0,0,.5)",
  backdropFilter: "blur(2px)",
  display: "flex",
  alignItems: "center",
  justifyContent: "center",
  zIndex: 60,
};

const card: React.CSSProperties = {
  width: 420,
  maxWidth: "92vw",
  maxHeight: "86vh",
  overflowY: "auto",
  background: "#26262a",
  border: "1px solid var(--color-line-2)",
  borderRadius: "var(--radius-lg)",
  boxShadow: "0 24px 80px rgba(0,0,0,.6)",
};

const input: React.CSSProperties = {
  width: "100%",
  background: "var(--color-elev)",
  border: "1px solid var(--color-line-2)",
  borderRadius: "var(--radius-sm)",
  padding: "7px 9px",
  fontSize: 13,
  color: "var(--color-t1)",
  outline: "none",
};

/** "Create preset from current edit" — name, group, per-module scope checklist, favorite. */
export default function CreatePresetDialog({ open, onClose, onSave }: Props) {
  const [name, setName] = useState("");
  const [group, setGroup] = useState("My Presets");
  const [groups, setGroups] = useState<Set<string>>(
    new Set(DEFAULT_PRESET_GROUPS),
  );
  const [favorite, setFavorite] = useState(false);

  useEffect(() => {
    if (open) {
      setName("");
      setGroup("My Presets");
      setGroups(new Set(DEFAULT_PRESET_GROUPS));
      setFavorite(false);
    }
  }, [open]);

  if (!open) return null;

  const toggle = (key: string) =>
    setGroups((s) => {
      const n = new Set(s);
      if (n.has(key)) n.delete(key);
      else n.add(key);
      return n;
    });

  const canSave = name.trim().length > 0 && groups.size > 0;
  const submit = () => {
    if (!canSave) return;
    onSave(
      name.trim(),
      group.trim() || undefined,
      fieldsForGroups([...groups]),
      favorite,
    );
    onClose();
  };

  return (
    <div
      style={overlay}
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div style={card}>
        <div
          style={{
            padding: "14px 18px",
            borderBottom: "1px solid var(--color-line)",
            fontSize: 14,
            fontWeight: 600,
            color: "var(--color-t1)",
          }}
        >
          New preset
        </div>

        <div
          style={{
            padding: "16px 18px",
            display: "flex",
            flexDirection: "column",
            gap: 14,
          }}
        >
          <label style={{ display: "block" }}>
            <div
              style={{
                fontSize: 11,
                color: "var(--color-t3)",
                marginBottom: 5,
              }}
            >
              Name
            </div>
            <input
              autoFocus
              value={name}
              onChange={(e) => setName(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && submit()}
              placeholder="My look"
              style={input}
            />
          </label>

          <label style={{ display: "block" }}>
            <div
              style={{
                fontSize: 11,
                color: "var(--color-t3)",
                marginBottom: 5,
              }}
            >
              Group
            </div>
            <input
              value={group}
              onChange={(e) => setGroup(e.target.value)}
              placeholder="My Presets"
              style={input}
            />
          </label>

          <div>
            <div
              style={{
                fontSize: 11,
                color: "var(--color-t3)",
                marginBottom: 7,
              }}
            >
              Settings to store
            </div>
            <div
              style={{
                display: "grid",
                gridTemplateColumns: "1fr 1fr",
                gap: "6px 12px",
              }}
            >
              {SCOPE_GROUPS.map((g) => (
                <label
                  key={g.key}
                  style={{
                    display: "flex",
                    alignItems: "center",
                    gap: 7,
                    fontSize: 12,
                    color: "var(--color-t1)",
                    cursor: "pointer",
                  }}
                >
                  <input
                    type="checkbox"
                    checked={groups.has(g.key)}
                    onChange={() => toggle(g.key)}
                  />
                  {g.label}
                </label>
              ))}
            </div>
            {groups.has("masks") && (
              <div
                style={{
                  fontSize: 10.5,
                  color: "var(--color-t3)",
                  marginTop: 7,
                  lineHeight: 1.4,
                }}
              >
                Masks store geometry relative to this image — they may not line
                up on photos of a different crop or aspect ratio.
              </div>
            )}
          </div>

          <label
            style={{
              display: "flex",
              alignItems: "center",
              gap: 7,
              fontSize: 12,
              color: "var(--color-t1)",
              cursor: "pointer",
            }}
          >
            <input
              type="checkbox"
              checked={favorite}
              onChange={(e) => setFavorite(e.target.checked)}
            />
            Mark as favorite
          </label>
        </div>

        <div
          style={{
            padding: "12px 18px",
            borderTop: "1px solid var(--color-line)",
            display: "flex",
            justifyContent: "flex-end",
            gap: 8,
          }}
        >
          <button
            onClick={onClose}
            style={{
              border: "1px solid var(--color-line-2)",
              background: "var(--color-elev)",
              color: "var(--color-t1)",
              borderRadius: "var(--radius-sm)",
              padding: "6px 14px",
              fontSize: 12,
              cursor: "pointer",
            }}
          >
            Cancel
          </button>
          <button
            onClick={submit}
            disabled={!canSave}
            style={{
              border: "none",
              background: canSave
                ? "var(--color-accent)"
                : "var(--color-line-2)",
              color: "#fff",
              borderRadius: "var(--radius-sm)",
              padding: "6px 16px",
              fontSize: 12,
              cursor: canSave ? "pointer" : "default",
              opacity: canSave ? 1 : 0.6,
            }}
          >
            Save preset
          </button>
        </div>
      </div>
    </div>
  );
}
