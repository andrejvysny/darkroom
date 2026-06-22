import { useEffect, useState, useCallback } from "react";
import {
  personFaces,
  faceConfirm,
  faceReject,
  personMerge,
  personSetName,
  faceCropStyle,
  type PersonRow,
  type PersonFace,
} from "../../lib/ipc";

interface PeopleReviewProps {
  person: PersonRow;
  /** All people, for the "Merge into…" picker. */
  allPeople: PersonRow[];
  onClose: () => void;
  /** Called after any change so the sidebar reloads. */
  onChanged: () => void;
}

/** Person detail / Review modal: confirm-or-reject suggested faces (Apple's "Review More Photos"
 *  flow), rename, and merge this cluster into another person. */
export default function PeopleReview({
  person,
  allPeople,
  onClose,
  onChanged,
}: PeopleReviewProps) {
  const [faces, setFaces] = useState<PersonFace[]>([]);
  const [name, setName] = useState(person.name ?? "");
  const [mergeOpen, setMergeOpen] = useState(false);

  const reload = useCallback(async () => {
    try {
      setFaces(await personFaces(person.id));
    } catch {
      setFaces([]);
    }
  }, [person.id]);

  useEffect(() => {
    void reload();
  }, [reload]);

  // Close on Escape.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const unconfirmed = faces.filter((f) => f.status === "unconfirmed");
  const confirmed = faces.filter((f) => f.status === "confirmed");

  async function act(fn: () => Promise<void>) {
    await fn();
    await reload();
    onChanged();
  }

  async function commitName() {
    const n = name.trim();
    await personSetName(person.id, n || null);
    onChanged();
  }

  async function doMerge(dst: number) {
    await personMerge(dst, person.id);
    setMergeOpen(false);
    onChanged();
    onClose();
  }

  const others = allPeople.filter((p) => p.id !== person.id);

  return (
    <div
      onClick={onClose}
      style={{
        position: "fixed",
        inset: 0,
        background: "rgba(0,0,0,0.55)",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        zIndex: 1000,
      }}
    >
      <div
        onClick={(e) => e.stopPropagation()}
        style={{
          width: "min(820px, 92vw)",
          maxHeight: "86vh",
          display: "flex",
          flexDirection: "column",
          background: "var(--color-app)",
          border: "1px solid var(--color-line)",
          borderRadius: "var(--radius-md, 10px)",
          overflow: "hidden",
        }}
      >
        {/* Header */}
        <div
          style={{
            display: "flex",
            alignItems: "center",
            gap: 10,
            padding: "12px 14px",
            borderBottom: "1px solid var(--color-line)",
          }}
        >
          <input
            value={name}
            onChange={(e) => setName(e.target.value)}
            onBlur={commitName}
            onKeyDown={(e) => {
              if (e.key === "Enter") (e.target as HTMLInputElement).blur();
            }}
            placeholder="Name this person…"
            style={{
              flex: 1,
              minWidth: 0,
              background: "transparent",
              border: "none",
              borderBottom: "1px solid var(--color-line)",
              color: "var(--color-t1)",
              fontSize: 16,
              fontWeight: 600,
              padding: "3px 2px",
              outline: "none",
            }}
          />
          <span style={{ fontSize: 12, color: "var(--color-t3)" }}>
            {person.faceCount.toLocaleString()} photos
          </span>
          <div style={{ position: "relative" }}>
            <button
              onClick={() => setMergeOpen((v) => !v)}
              disabled={others.length === 0}
              style={btn()}
            >
              Merge into ▾
            </button>
            {mergeOpen && others.length > 0 && (
              <div
                style={{
                  position: "absolute",
                  right: 0,
                  top: "calc(100% + 4px)",
                  minWidth: 180,
                  maxHeight: 260,
                  overflowY: "auto",
                  background: "var(--color-panel)",
                  border: "1px solid var(--color-line)",
                  borderRadius: "var(--radius-sm)",
                  zIndex: 1,
                  padding: 4,
                }}
              >
                {others.map((p) => (
                  <div
                    key={p.id}
                    onClick={() => void doMerge(p.id)}
                    style={{
                      padding: "6px 8px",
                      fontSize: 12.5,
                      borderRadius: "var(--radius-sm)",
                      cursor: "pointer",
                      color: p.name ? "var(--color-t1)" : "var(--color-t3)",
                      fontStyle: p.name ? undefined : "italic",
                    }}
                  >
                    {p.name ?? "Unnamed"} ({p.faceCount})
                  </div>
                ))}
              </div>
            )}
          </div>
          <button
            onClick={onClose}
            style={{ ...btn(), fontSize: 15 }}
            aria-label="Close"
          >
            ×
          </button>
        </div>

        {/* Body */}
        <div style={{ overflowY: "auto", padding: 14 }}>
          {unconfirmed.length > 0 && (
            <Section title={`Review (${unconfirmed.length})`}>
              {unconfirmed.map((f) => (
                <FaceTile key={f.id} face={f}>
                  <TileButton
                    title="Confirm"
                    onClick={() => void act(() => faceConfirm(f.id))}
                  >
                    ✓
                  </TileButton>
                  <TileButton
                    title={
                      person.name ? `Not ${person.name}` : "Not this person"
                    }
                    onClick={() => void act(() => faceReject(f.id))}
                  >
                    ✕
                  </TileButton>
                </FaceTile>
              ))}
            </Section>
          )}

          {confirmed.length > 0 && (
            <Section title={`Confirmed (${confirmed.length})`}>
              {confirmed.map((f) => (
                <FaceTile key={f.id} face={f}>
                  <TileButton
                    title={person.name ? `Not ${person.name}` : "Remove"}
                    onClick={() => void act(() => faceReject(f.id))}
                  >
                    ✕
                  </TileButton>
                </FaceTile>
              ))}
            </Section>
          )}

          {faces.length === 0 && (
            <div style={{ color: "var(--color-t3)", fontSize: 13, padding: 8 }}>
              No faces.
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

function Section({
  title,
  children,
}: {
  title: string;
  children: React.ReactNode;
}) {
  return (
    <div style={{ marginBottom: 16 }}>
      <div
        style={{
          fontSize: 10.5,
          letterSpacing: ".06em",
          textTransform: "uppercase",
          color: "var(--color-t3)",
          fontWeight: 600,
          marginBottom: 8,
        }}
      >
        {title}
      </div>
      <div
        style={{
          display: "grid",
          gridTemplateColumns: "repeat(auto-fill, minmax(84px, 1fr))",
          gap: 8,
        }}
      >
        {children}
      </div>
    </div>
  );
}

function FaceTile({
  face,
  children,
}: {
  face: PersonFace;
  children: React.ReactNode;
}) {
  return (
    <div
      style={{
        position: "relative",
        aspectRatio: "1",
        borderRadius: "var(--radius-sm)",
        border: "1px solid var(--color-line)",
        ...faceCropStyle(face.imageHash, face.bbox, 0.25),
      }}
    >
      <div
        style={{
          position: "absolute",
          left: 0,
          right: 0,
          bottom: 0,
          display: "flex",
          justifyContent: "center",
          gap: 6,
          padding: 4,
          background: "linear-gradient(transparent, rgba(0,0,0,0.5))",
        }}
      >
        {children}
      </div>
    </div>
  );
}

function TileButton({
  title,
  onClick,
  children,
}: {
  title: string;
  onClick: () => void;
  children: React.ReactNode;
}) {
  return (
    <button
      title={title}
      aria-label={title}
      onClick={onClick}
      style={{
        width: 22,
        height: 22,
        borderRadius: "50%",
        border: "none",
        background: "rgba(255,255,255,0.9)",
        color: "#111",
        fontSize: 12,
        lineHeight: 1,
        cursor: "pointer",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
      }}
    >
      {children}
    </button>
  );
}

function btn(): React.CSSProperties {
  return {
    fontSize: 11.5,
    padding: "3px 8px",
    borderRadius: "var(--radius-sm)",
    border: "1px solid var(--color-line)",
    background: "transparent",
    color: "var(--color-t2)",
    cursor: "pointer",
  };
}
