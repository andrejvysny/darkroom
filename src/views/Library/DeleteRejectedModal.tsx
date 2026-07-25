import { useEffect, useState } from "react";
import {
  cullRejectedSummary,
  cullDeleteRejected,
  type QueryParams,
  type RejectSummary,
} from "../../lib/ipc";
import Icon from "../../components/Icon";

interface Props {
  open: boolean;
  /** The filter the grid is showing. Only ever used to *narrow* the set — the backend forces the
   *  `reject` flag on, so this can never reach an unflagged or picked photo. */
  params: QueryParams;
  onClose: () => void;
  /** Called after a delete pass with a human-readable outcome; the caller refreshes the library. */
  onDeleted: (message: string) => void;
}

/** Deliberate two-step confirmation for an irreversible bulk delete: the user must click "Continue"
 *  on the summary, then a second, differently-labelled button on a screen that restates exactly what
 *  is about to happen. Neither step is armed by default, and the destructive button is only enabled
 *  once the backend has told us how many files there really are. */
type Step = "review" | "confirm" | "working";

function formatBytes(n: number): string {
  if (n <= 0) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  const i = Math.min(units.length - 1, Math.floor(Math.log(n) / Math.log(1024)));
  const v = n / Math.pow(1024, i);
  return `${v >= 10 || i === 0 ? Math.round(v) : v.toFixed(1)} ${units[i]}`;
}

export default function DeleteRejectedModal({
  open,
  params,
  onClose,
  onDeleted,
}: Props) {
  const [step, setStep] = useState<Step>("review");
  const [summary, setSummary] = useState<RejectSummary | null>(null);
  const [error, setError] = useState<string | null>(null);

  // Re-count on every open: the grid's total is paged and can be stale, and the count shown is the
  // number the user is consenting to.
  useEffect(() => {
    if (!open) return;
    setStep("review");
    setSummary(null);
    setError(null);
    let cancelled = false;
    void cullRejectedSummary(params)
      .then((s) => {
        if (!cancelled) setSummary(s);
      })
      .catch((e) => {
        if (!cancelled) setError(String(e));
      });
    return () => {
      cancelled = true;
    };
  }, [open, params]);

  if (!open) return null;

  const total = summary ? summary.images + summary.companions : 0;
  const busy = step === "working";

  async function handleDelete() {
    setStep("working");
    setError(null);
    try {
      const res = await cullDeleteRejected(params);
      const parts = [`Moved ${res.trashed} file${res.trashed === 1 ? "" : "s"} to Trash`];
      if (res.companions > 0) parts.push(`${res.companions} paired`);
      if (res.failed > 0) parts.push(`${res.failed} could not be deleted`);
      onDeleted(parts.join(" · "));
      onClose();
    } catch (e) {
      setError(String(e));
      setStep("confirm");
    }
  }

  return (
    <div
      onClick={(e) => {
        if (e.target === e.currentTarget && !busy) onClose();
      }}
      style={{
        position: "fixed",
        inset: 0,
        background: "rgba(0,0,0,.5)",
        backdropFilter: "blur(3px)",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        zIndex: 60,
      }}
    >
      <div
        data-testid="delete-rejected-modal"
        style={{
          width: 440,
          maxWidth: "94vw",
          background: "#26262a",
          border: "1px solid var(--color-line-2)",
          borderRadius: "var(--radius-lg)",
          boxShadow: "0 24px 80px rgba(0,0,0,.7)",
          overflow: "hidden",
        }}
      >
        <div
          style={{
            padding: "12px 20px",
            borderBottom: "1px solid var(--color-line)",
            display: "flex",
            alignItems: "center",
            gap: 8,
            fontSize: 14,
            fontWeight: 600,
            color: "var(--color-t1)",
          }}
        >
          <Icon name="trash" size={14} />
          {step === "confirm" ? "Are you sure?" : "Delete rejected photos"}
        </div>

        <div
          style={{
            padding: "18px 20px",
            fontSize: 13,
            lineHeight: 1.55,
            color: "var(--color-t2)",
            display: "flex",
            flexDirection: "column",
            gap: 12,
          }}
        >
          {error && (
            <div style={{ color: "var(--color-danger, #e5685f)" }}>{error}</div>
          )}

          {summary == null && !error && <div>Counting rejected photos…</div>}

          {summary != null && total === 0 && (
            <div>No rejected photos in the current view.</div>
          )}

          {summary != null && total > 0 && step !== "confirm" && (
            <>
              <div data-testid="delete-rejected-count">
                <strong style={{ color: "var(--color-t1)" }}>
                  {summary.images.toLocaleString()} rejected photo
                  {summary.images === 1 ? "" : "s"}
                </strong>
                {summary.companions > 0 && (
                  <>
                    {" "}
                    plus{" "}
                    <strong style={{ color: "var(--color-t1)" }}>
                      {summary.companions.toLocaleString()} paired camera file
                      {summary.companions === 1 ? "" : "s"}
                    </strong>{" "}
                    (the JPEG/HEIF shot alongside a rejected RAW)
                  </>
                )}{" "}
                — {formatBytes(summary.bytes)}.
              </div>
              <div style={{ color: "var(--color-t3)" }}>
                Only photos flagged <em>reject</em> are touched. Picked and
                unflagged photos are never affected.
              </div>
            </>
          )}

          {summary != null && total > 0 && step === "confirm" && (
            <>
              <div style={{ color: "var(--color-t1)" }}>
                This will move{" "}
                <strong>
                  {total.toLocaleString()} file{total === 1 ? "" : "s"}
                </strong>{" "}
                to the system Trash and remove them from your catalog, along with
                their ratings, keywords and edits.
              </div>
              <div style={{ color: "var(--color-t3)" }}>
                Files stay recoverable from the Trash until you empty it. The
                catalog entries cannot be restored from inside Darkroom.
              </div>
            </>
          )}
        </div>

        <div
          style={{
            padding: "12px 20px",
            borderTop: "1px solid var(--color-line)",
            display: "flex",
            justifyContent: "flex-end",
            gap: 8,
          }}
        >
          <button
            onClick={onClose}
            disabled={busy}
            style={{
              padding: "6px 14px",
              borderRadius: "var(--radius-sm)",
              border: "1px solid var(--color-line-2)",
              background: "var(--color-elev)",
              color: "var(--color-t1)",
              fontSize: 12.5,
              cursor: busy ? "default" : "pointer",
              opacity: busy ? 0.6 : 1,
            }}
          >
            Cancel
          </button>

          {step === "review" && (
            <button
              data-testid="delete-rejected-continue"
              onClick={() => setStep("confirm")}
              disabled={summary == null || total === 0}
              style={{
                padding: "6px 14px",
                borderRadius: "var(--radius-sm)",
                border: "1px solid var(--color-line-2)",
                background: "var(--color-elev)",
                color:
                  summary == null || total === 0
                    ? "var(--color-t3)"
                    : "var(--color-danger, #e5685f)",
                fontSize: 12.5,
                cursor:
                  summary == null || total === 0 ? "default" : "pointer",
                opacity: summary == null || total === 0 ? 0.5 : 1,
              }}
            >
              Continue…
            </button>
          )}

          {step !== "review" && (
            <button
              data-testid="delete-rejected-confirm"
              onClick={() => void handleDelete()}
              disabled={busy}
              style={{
                padding: "6px 14px",
                borderRadius: "var(--radius-sm)",
                border: "1px solid #b3261e",
                background: "#b3261e",
                color: "#fff",
                fontSize: 12.5,
                fontWeight: 600,
                cursor: busy ? "default" : "pointer",
                opacity: busy ? 0.7 : 1,
              }}
            >
              {busy
                ? "Deleting…"
                : `Move ${total.toLocaleString()} file${total === 1 ? "" : "s"} to Trash`}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
