import type { ImportReport, ReportItem } from "../../lib/ipc";

interface Props {
  report: ImportReport | null;
  onClose: () => void;
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

function Section({
  title,
  color,
  items,
  showNote,
}: {
  title: string;
  color: string;
  items: ReportItem[];
  showNote: boolean;
}) {
  if (items.length === 0) return null;
  return (
    <div style={{ marginTop: 14 }}>
      <div style={{ fontSize: 11, fontWeight: 600, color, marginBottom: 6 }}>
        {title} ({items.length})
      </div>
      <div style={{ display: "flex", flexDirection: "column", gap: 5 }}>
        {items.map((it, i) => (
          <div
            key={`${it.key}-${i}`}
            style={{ fontSize: 12, color: "var(--color-t2)" }}
          >
            <span
              style={{
                color: "var(--color-t1)",
                fontFamily: "var(--font-mono)",
              }}
            >
              {it.key}
            </span>
            {showNote && it.note ? (
              <span style={{ color: "var(--color-t3)" }}> — {it.note}</span>
            ) : null}
          </div>
        ))}
      </div>
    </div>
  );
}

/** Shows the honest conversion outcome after importing an external preset. */
export default function ImportReportModal({ report, onClose }: Props) {
  if (!report) return null;
  const total =
    report.mapped.length + report.approximated.length + report.dropped.length;

  return (
    <div
      style={overlay}
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div
        style={{
          width: 460,
          maxWidth: "92vw",
          maxHeight: "84vh",
          overflowY: "auto",
          background: "#26262a",
          border: "1px solid var(--color-line-2)",
          borderRadius: "var(--radius-lg)",
          boxShadow: "0 24px 80px rgba(0,0,0,.6)",
        }}
      >
        <div
          style={{
            padding: "14px 18px",
            borderBottom: "1px solid var(--color-line)",
          }}
        >
          <div
            style={{ fontSize: 14, fontWeight: 600, color: "var(--color-t1)" }}
          >
            Preset imported
          </div>
          <div style={{ fontSize: 11, color: "var(--color-t3)", marginTop: 3 }}>
            {report.sourceFormat}
            {report.sourceProcessVersion
              ? ` · process ${report.sourceProcessVersion}`
              : ""}{" "}
            · {report.mapped.length} mapped · {report.approximated.length}{" "}
            approximated · {report.dropped.length} dropped
          </div>
        </div>

        <div style={{ padding: "4px 18px 18px" }}>
          {total === 0 ? (
            <div
              style={{ fontSize: 12, color: "var(--color-t3)", marginTop: 14 }}
            >
              No recognized settings were found in this file.
            </div>
          ) : (
            <>
              <Section
                title="Mapped"
                color="#5fb878"
                items={report.mapped}
                showNote={false}
              />
              <Section
                title="Approximated"
                color="#d8a23a"
                items={report.approximated}
                showNote
              />
              <Section
                title="Dropped"
                color="#c46060"
                items={report.dropped}
                showNote
              />
            </>
          )}
        </div>

        <div
          style={{
            padding: "12px 18px",
            borderTop: "1px solid var(--color-line)",
            display: "flex",
            justifyContent: "flex-end",
          }}
        >
          <button
            onClick={onClose}
            style={{
              border: "none",
              background: "var(--color-accent)",
              color: "#fff",
              borderRadius: "var(--radius-sm)",
              padding: "6px 16px",
              fontSize: 12,
              cursor: "pointer",
            }}
          >
            Done
          </button>
        </div>
      </div>
    </div>
  );
}
