import { useState } from "react";
import Icon from "../../components/Icon";
import {
  clearedFilters,
  faceCropStyle,
  personSetName,
  personSetHidden,
  type PersonRow,
  type QueryParams,
} from "../../lib/ipc";
import { useFaces } from "../../lib/useFaces";
import PeopleReview from "./PeopleReview";

interface PeopleNavProps {
  params: QueryParams;
  patchParams: (patch: Partial<QueryParams>) => void;
  clearFilters: () => void;
}

/** "People" sidebar section: clustered people (named first, then unnamed "Suggested"), each a
 *  face-cropped avatar. Click filters the library by person; the pencil names a cluster. The header
 *  "Find People" button runs the (manual) face pass. Owns its own data + the Review/merge modal. */
export default function PeopleNav({
  params,
  patchParams,
  clearFilters,
}: PeopleNavProps) {
  const faces = useFaces();
  const [showSuggested, setShowSuggested] = useState(true);
  const [reviewPerson, setReviewPerson] = useState<PersonRow | null>(null);
  const onReviewPerson = (p: PersonRow) => setReviewPerson(p);
  const running = faces.status?.running === true || faces.progress !== null;
  const named = faces.people.filter((p) => p.name != null);
  const suggested = faces.people.filter((p) => p.name == null);

  function toggle(id: number) {
    if (params.personId === id) clearFilters();
    else patchParams({ ...clearedFilters(), personId: id });
  }

  return (
    <div>
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
        <span>People</span>
        <FindButton
          running={running}
          onFind={() => void faces.findPeople(false)}
          onCancel={() => void faces.cancel()}
        />
      </div>

      {faces.progress && (
        <ProgressBar
          label={
            faces.progress.kind === "models"
              ? "Downloading models"
              : "Finding people"
          }
          done={faces.progress.done}
          total={faces.progress.total}
        />
      )}

      {named.map((p) => (
        <PersonRowItem
          key={p.id}
          person={p}
          active={params.personId === p.id}
          onClick={() => toggle(p.id)}
          onReview={onReviewPerson ? () => onReviewPerson(p) : undefined}
          onRename={async (name) => {
            await personSetName(p.id, name);
            await faces.reload();
          }}
          onHide={async () => {
            await personSetHidden(p.id, true);
            await faces.reload();
          }}
        />
      ))}

      {suggested.length > 0 && (
        <>
          <div
            onClick={() => setShowSuggested((v) => !v)}
            style={{
              display: "flex",
              alignItems: "center",
              gap: 6,
              padding: "6px 8px",
              fontSize: 11.5,
              color: "var(--color-t3)",
              cursor: "pointer",
            }}
          >
            <span style={{ width: 10 }}>{showSuggested ? "▾" : "▸"}</span>
            Suggested ({suggested.length})
          </div>
          {showSuggested &&
            suggested.map((p) => (
              <PersonRowItem
                key={p.id}
                person={p}
                active={params.personId === p.id}
                onClick={() => toggle(p.id)}
                onReview={onReviewPerson ? () => onReviewPerson(p) : undefined}
                onRename={async (name) => {
                  await personSetName(p.id, name);
                  await faces.reload();
                }}
                onHide={async () => {
                  await personSetHidden(p.id, true);
                  await faces.reload();
                }}
              />
            ))}
        </>
      )}

      {faces.people.length === 0 && !running && (
        <div
          style={{ fontSize: 12, color: "var(--color-t3)", padding: "4px 8px" }}
        >
          {faces.status && faces.status.processed > 0
            ? "No people found yet"
            : "Find People to detect faces"}
        </div>
      )}

      {reviewPerson && (
        <PeopleReview
          person={reviewPerson}
          allPeople={faces.people}
          onClose={() => setReviewPerson(null)}
          onChanged={() => void faces.reload()}
        />
      )}
    </div>
  );
}

function PersonRowItem({
  person,
  active,
  onClick,
  onRename,
  onHide,
  onReview,
}: {
  person: PersonRow;
  active: boolean;
  onClick: () => void;
  onRename: (name: string) => void;
  onHide: () => void;
  onReview?: () => void;
}) {
  const [hover, setHover] = useState(false);
  const [renaming, setRenaming] = useState(false);
  const [draft, setDraft] = useState(person.name ?? "");

  const commit = () => {
    const name = draft.trim();
    if (name) onRename(name);
    setRenaming(false);
  };

  return (
    <div
      onClick={renaming ? undefined : onClick}
      onMouseEnter={() => setHover(true)}
      onMouseLeave={() => setHover(false)}
      style={{
        display: "flex",
        alignItems: "center",
        gap: 9,
        padding: "5px 8px",
        borderRadius: "var(--radius-sm)",
        color: active ? "var(--color-t1)" : "var(--color-t2)",
        fontSize: 12.5,
        cursor: "pointer",
        position: "relative",
        background: active ? "var(--color-accent-dim)" : "transparent",
      }}
    >
      {active && (
        <span
          style={{
            position: "absolute",
            left: 0,
            top: 5,
            bottom: 5,
            width: 2,
            borderRadius: 2,
            background: "var(--color-accent)",
          }}
        />
      )}
      <Avatar person={person} />
      {renaming ? (
        <input
          autoFocus
          value={draft}
          onClick={(e) => e.stopPropagation()}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              commit();
            } else if (e.key === "Escape") setRenaming(false);
          }}
          onBlur={commit}
          placeholder="Name…"
          style={{
            flex: 1,
            minWidth: 0,
            background: "var(--color-panel)",
            border: "1px solid var(--color-accent-line)",
            borderRadius: "var(--radius-sm)",
            color: "var(--color-t1)",
            fontSize: 12.5,
            padding: "3px 6px",
            outline: "none",
          }}
        />
      ) : (
        <span
          style={{
            overflow: "hidden",
            textOverflow: "ellipsis",
            whiteSpace: "nowrap",
            color: person.name == null ? "var(--color-t3)" : undefined,
            fontStyle: person.name == null ? "italic" : undefined,
          }}
        >
          {person.name ?? "Name…"}
        </span>
      )}
      {hover && !renaming ? (
        <span style={{ marginLeft: "auto", display: "flex", gap: 4 }}>
          {onReview && (
            <button
              onClick={(e) => {
                e.stopPropagation();
                onReview();
              }}
              title="Review faces"
              aria-label="Review faces"
              style={iconBtn()}
            >
              <Icon name="scan" size={11} />
            </button>
          )}
          <button
            onClick={(e) => {
              e.stopPropagation();
              setDraft(person.name ?? "");
              setRenaming(true);
            }}
            title="Name person"
            aria-label="Name person"
            style={iconBtn()}
          >
            <Icon name="edit" size={11} />
          </button>
          <button
            onClick={(e) => {
              e.stopPropagation();
              onHide();
            }}
            title="Hide person"
            aria-label="Hide person"
            style={{ ...iconBtn(), fontSize: 13 }}
          >
            ×
          </button>
        </span>
      ) : (
        !renaming && (
          <span
            style={{
              marginLeft: "auto",
              fontFamily: "var(--font-mono)",
              fontSize: 11,
              color: "var(--color-t3)",
            }}
          >
            {person.faceCount.toLocaleString()}
          </span>
        )
      )}
    </div>
  );
}

function Avatar({ person }: { person: PersonRow }) {
  const size = 26;
  const base: React.CSSProperties = {
    width: size,
    height: size,
    borderRadius: "50%",
    flexShrink: 0,
    border: "1px solid var(--color-line)",
    background: "var(--color-panel)",
  };
  if (person.coverImageHash && person.coverBbox) {
    return (
      <div
        style={{
          ...base,
          ...faceCropStyle(person.coverImageHash, person.coverBbox),
        }}
      />
    );
  }
  return (
    <div
      style={{
        ...base,
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        color: "var(--color-t3)",
      }}
    >
      <Icon name="scan" size={12} />
    </div>
  );
}

function ProgressBar({
  label,
  done,
  total,
}: {
  label: string;
  done: number;
  total: number;
}) {
  const pct = total > 0 ? Math.round((done / total) * 100) : 0;
  return (
    <div style={{ padding: "2px 8px 6px" }}>
      <div
        style={{ fontSize: 10.5, color: "var(--color-t3)", marginBottom: 3 }}
      >
        {label} {done}/{total}
      </div>
      <div
        style={{
          height: 3,
          borderRadius: 2,
          background: "var(--color-line)",
          overflow: "hidden",
        }}
      >
        <div
          style={{
            height: "100%",
            width: `${pct}%`,
            background: "var(--color-accent)",
          }}
        />
      </div>
    </div>
  );
}

function FindButton({
  running,
  onFind,
  onCancel,
}: {
  running: boolean;
  onFind: () => void;
  onCancel: () => void;
}) {
  return (
    <button
      onClick={(e) => {
        e.stopPropagation();
        if (running) onCancel();
        else onFind();
      }}
      title={running ? "Stop" : "Detect faces in your library"}
      aria-label={running ? "Stop finding people" : "Find people"}
      style={{
        fontSize: 10,
        padding: "1px 6px",
        borderRadius: "var(--radius-sm)",
        border: "1px solid var(--color-line)",
        background: "transparent",
        color: "var(--color-t2)",
        cursor: "pointer",
        lineHeight: 1.5,
      }}
    >
      {running ? "Stop" : "Find People"}
    </button>
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
