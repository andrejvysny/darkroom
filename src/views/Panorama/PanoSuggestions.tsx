import { useState } from "react";
import { useAppStore } from "../../store/app";
import { thumbUrl, type PanoGroupRow, type PanoMemberRow } from "../../lib/ipc";
import { usePanoDetect, panoDetectProgressLabel } from "../../lib/usePanoDetect";

const PREVIEW_CAP = 5;

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
  width: 780,
  maxWidth: "94vw",
  maxHeight: "88vh",
  display: "flex",
  flexDirection: "column",
  background: "#26262a",
  border: "1px solid var(--color-line-2)",
  borderRadius: "var(--radius-lg)",
  boxShadow: "0 24px 80px rgba(0,0,0,.6)",
};

function fmtCaptureDate(t: number): string {
  return new Date(t * 1000).toLocaleString();
}

/** First filename if the group is a singleton (shouldn't happen — groups are ≥2), else a
 *  first–last filename range. */
function memberRangeLabel(members: PanoMemberRow[]): string {
  if (members.length === 0) return "";
  const first = members[0];
  const last = members[members.length - 1];
  return first.filename === last.filename
    ? first.filename
    : `${first.filename} – ${last.filename}`;
}

function captureRangeLabel(members: PanoMemberRow[]): string {
  const dated = members.filter(
    (m): m is PanoMemberRow & { captureDate: number } => m.captureDate != null,
  );
  if (dated.length === 0) return "Unknown capture time";
  const first = dated[0].captureDate;
  const last = dated[dated.length - 1].captureDate;
  return first === last
    ? fmtCaptureDate(first)
    : `${fmtCaptureDate(first)} – ${fmtCaptureDate(last)}`;
}

function GroupRow({
  group,
  onDismiss,
  onMerge,
}: {
  group: PanoGroupRow;
  onDismiss: (dismissed: boolean) => void;
  onMerge: () => void;
}) {
  const preview = group.members.slice(0, PREVIEW_CAP);
  const overflow = group.members.length - PREVIEW_CAP;
  const merged = group.status === "merged";
  const dismissed = group.status === "dismissed";
  // Derived client-side from members rather than a backend `missingCount` field — trivial to
  // compute and keeps `PanoGroupRow` from carrying a second representation of the same data.
  const hasMissing = group.members.some((m) => !m.present);
  const canMerge = group.allRaw && !hasMissing;
  const mergeBlockedReason = !group.allRaw
    ? "Merging requires RAW source files"
    : hasMissing
      ? "A source file for this group is missing"
      : undefined;

  return (
    <div
      style={{
        display: "flex",
        alignItems: "center",
        gap: 16,
        background: "var(--color-panel)",
        border: `1px solid ${dismissed ? "var(--color-line)" : "var(--color-line-2)"}`,
        borderRadius: "var(--radius-lg)",
        padding: "12px 14px",
        opacity: dismissed ? 0.7 : 1,
      }}
    >
      <div style={{ display: "flex", gap: 5, flex: "none" }}>
        {preview.map((m) => (
          <div
            key={m.imageId}
            style={{
              position: "relative",
              width: 62,
              height: 42,
              borderRadius: "var(--radius-sm)",
              overflow: "hidden",
              background: "var(--color-stage)",
            }}
          >
            <img
              src={thumbUrl(m.contentHash, 256)}
              alt={m.filename}
              style={{
                width: "100%",
                height: "100%",
                objectFit: "cover",
                display: "block",
                opacity: m.present ? 1 : 0.35,
              }}
              loading="lazy"
            />
            {!m.present && (
              <span
                title={`${m.filename} is missing`}
                style={{
                  position: "absolute",
                  inset: 0,
                  display: "flex",
                  alignItems: "center",
                  justifyContent: "center",
                  fontSize: 8.5,
                  fontWeight: 700,
                  letterSpacing: ".04em",
                  textTransform: "uppercase",
                  color: "var(--color-t1)",
                  background: "rgba(0,0,0,.45)",
                }}
              >
                Missing
              </span>
            )}
          </div>
        ))}
        {overflow > 0 && (
          <div
            style={{
              width: 62,
              height: 42,
              borderRadius: "var(--radius-sm)",
              border: "1px dashed var(--color-line-2)",
              display: "flex",
              alignItems: "center",
              justifyContent: "center",
              fontSize: 12,
              color: "var(--color-t3)",
            }}
          >
            +{overflow}
          </div>
        )}
      </div>

      <div style={{ flex: 1, minWidth: 0 }}>
        <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
          <span
            style={{
              fontSize: 13,
              color: "var(--color-t1)",
              fontWeight: 500,
              overflow: "hidden",
              textOverflow: "ellipsis",
              whiteSpace: "nowrap",
            }}
          >
            {memberRangeLabel(group.members)}
          </span>
          <span
            title="Brown-Lowe confidence — min necessary link"
            style={{
              fontSize: 10.5,
              fontWeight: 700,
              color: "var(--color-t2)",
              background: "var(--color-elev)",
              borderRadius: 4,
              padding: "2px 6px",
              flex: "none",
            }}
          >
            {group.confidence.toFixed(2)}
          </span>
          {(dismissed || merged) && (
            <span
              style={{
                fontSize: 10,
                fontWeight: 700,
                letterSpacing: ".05em",
                textTransform: "uppercase",
                color: merged ? "var(--color-pick)" : "var(--color-t3)",
                flex: "none",
              }}
            >
              {merged ? "Merged" : "Dismissed"}
            </span>
          )}
        </div>
        <div style={{ fontSize: 11.5, color: "var(--color-t2)", marginTop: 5 }}>
          {group.members.length} photos ·{" "}
          {group.allRaw ? "all RAW" : "mixed format"} ·{" "}
          {captureRangeLabel(group.members)}
        </div>
      </div>

      <div style={{ display: "flex", alignItems: "center", gap: 8, flex: "none" }}>
        {merged ? (
          <span
            style={{
              fontSize: 11,
              fontWeight: 600,
              color: "var(--color-pick)",
            }}
          >
            Merged
          </span>
        ) : (
          <>
            <button
              className="tbtn ghost"
              onClick={() => onDismiss(!dismissed)}
              style={{ fontSize: 12 }}
            >
              {dismissed ? "Undo" : "Dismiss"}
            </button>
            <button
              onClick={onMerge}
              disabled={!canMerge}
              title={mergeBlockedReason}
              style={{
                border: "none",
                background: canMerge
                  ? "var(--color-accent)"
                  : "var(--color-line-2)",
                color: "#fff",
                borderRadius: "var(--radius-sm)",
                padding: "6px 14px",
                fontSize: 12,
                cursor: canMerge ? "pointer" : "default",
                opacity: canMerge ? 1 : 0.6,
                whiteSpace: "nowrap",
              }}
            >
              Preview &amp; Merge…
            </button>
          </>
        )}
      </div>
    </div>
  );
}

/**
 * Panorama-suggestions review overlay. Self-gated on `panoSuggestOpen` (opened from LeftNav's
 * Panoramas section or the command palette), mirrors `PanoramaModal`'s backdrop/card look with
 * content structured like Dedup `Overview`'s group rows. "Preview & Merge…" hands the group off to
 * the existing `PanoramaModal` via `usePanoDetect().openMerge` and closes this overlay so the merge
 * modal takes over.
 */
export default function PanoSuggestions() {
  const open = useAppStore((s) => s.panoSuggestOpen);
  const setPanoSuggestOpen = useAppStore((s) => s.setPanoSuggestOpen);
  const panoDetect = usePanoDetect();
  const [showDismissed, setShowDismissed] = useState(false);

  if (!open) return null;

  const visibleGroups = panoDetect.groups.filter(
    (g) => g.status !== "dismissed" || showDismissed,
  );
  const suggestedCount = panoDetect.suggested;

  return (
    <div
      style={overlay}
      onClick={(e) => {
        if (e.target === e.currentTarget) setPanoSuggestOpen(false);
      }}
    >
      <div style={card}>
        <div
          style={{
            display: "flex",
            alignItems: "center",
            gap: 12,
            padding: "14px 18px",
            borderBottom: "1px solid var(--color-line)",
            flex: "none",
          }}
        >
          <div style={{ flex: 1, minWidth: 0 }}>
            <div style={{ fontSize: 14, fontWeight: 600, color: "var(--color-t1)" }}>
              Panorama suggestions
            </div>
            <div style={{ fontSize: 11.5, color: "var(--color-t3)", marginTop: 2 }}>
              {suggestedCount} suggestion{suggestedCount === 1 ? "" : "s"} awaiting review
            </div>
          </div>

          {/* Detection is started from the "Run AI scan…" modal. The Detect/↺ buttons that used to
              sit here bypassed the unified scan job entirely, so they could launch a second
              CPU-heavy pass while one was already running. */}

          <label
            style={{
              display: "flex",
              alignItems: "center",
              gap: 6,
              fontSize: 12,
              color: "var(--color-t2)",
              cursor: "pointer",
            }}
          >
            <input
              type="checkbox"
              checked={showDismissed}
              onChange={(e) => setShowDismissed(e.target.checked)}
            />
            Show dismissed
          </label>

          <button
            onClick={() => setPanoSuggestOpen(false)}
            aria-label="Close"
            title="Close"
            style={{
              display: "flex",
              alignItems: "center",
              justifyContent: "center",
              width: 24,
              height: 24,
              border: "none",
              background: "transparent",
              color: "var(--color-t3)",
              fontSize: 16,
              cursor: "pointer",
              lineHeight: 1,
            }}
          >
            ×
          </button>
        </div>

        {panoDetect.progress && (
          <div
            style={{
              padding: "10px 18px",
              borderBottom: "1px solid var(--color-line)",
              flex: "none",
            }}
          >
            <div
              style={{
                fontSize: 11.5,
                color: "var(--color-t2)",
                marginBottom: 6,
              }}
            >
              {panoDetectProgressLabel(panoDetect.progress)}
            </div>
            <div
              style={{
                height: 4,
                background: "var(--color-elev)",
                borderRadius: 3,
                overflow: "hidden",
              }}
            >
              <div
                style={{
                  height: "100%",
                  width:
                    panoDetect.progress.total > 0
                      ? `${Math.min(100, (panoDetect.progress.done / panoDetect.progress.total) * 100)}%`
                      : "35%",
                  background: "var(--color-accent)",
                  transition: "width .2s",
                }}
              />
            </div>
          </div>
        )}

        <div style={{ overflowY: "auto", padding: "16px 18px", minHeight: 0 }}>
          {visibleGroups.length === 0 ? (
            <div
              style={{
                padding: 60,
                textAlign: "center",
                color: "var(--color-t3)",
                fontSize: 13,
                lineHeight: 1.7,
              }}
            >
              No panorama suggestions yet — run{" "}
              <strong style={{ color: "var(--color-t2)" }}>Detect</strong> to
              scan the library for stitchable sequences.
            </div>
          ) : (
            <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
              {visibleGroups.map((group) => (
                <GroupRow
                  key={group.id}
                  group={group}
                  onDismiss={(dismissed) =>
                    void panoDetect.dismiss(group.id, dismissed)
                  }
                  onMerge={() => {
                    panoDetect.openMerge(group);
                    setPanoSuggestOpen(false);
                  }}
                />
              ))}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
