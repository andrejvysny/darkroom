import { useUpdateStore } from "../store/update";
import {
  downloadAndInstall,
  restartApp,
  dismissUpdate,
} from "../lib/useUpdater";
import { ProgressBar } from "./ProgressBar";

const MB = 1024 * 1024;
function fmtMB(b: number): string {
  return `${(b / MB).toFixed(1)} MB`;
}

const primaryBtn: React.CSSProperties = {
  border: "none",
  borderRadius: "var(--radius-sm)",
  padding: "6px 14px",
  fontSize: 12,
  fontWeight: 500,
  background: "var(--color-accent)",
  color: "#fff",
  cursor: "pointer",
};

const ghostBtn: React.CSSProperties = {
  border: "1px solid var(--color-line-2)",
  borderRadius: "var(--radius-sm)",
  padding: "6px 12px",
  fontSize: 12,
  background: "transparent",
  color: "var(--color-t2)",
  cursor: "pointer",
};

/**
 * App-wide update prompt. Reads the shared update store; mounted once in App. Walks the user
 * through available → downloading → ready → restart. Hidden in the `idle`/`checking`/`uptodate`
 * states (those surface via toast instead).
 */
export default function UpdateBanner() {
  const status = useUpdateStore((s) => s.status);
  const version = useUpdateStore((s) => s.version);
  const bytesDone = useUpdateStore((s) => s.bytesDone);
  const bytesTotal = useUpdateStore((s) => s.bytesTotal);
  const error = useUpdateStore((s) => s.error);

  const visible =
    status === "available" ||
    status === "downloading" ||
    status === "ready" ||
    (status === "error" && error != null);
  if (!visible) return null;

  const pct = bytesTotal > 0 ? Math.round((bytesDone / bytesTotal) * 100) : 0;

  return (
    <div
      style={{
        position: "fixed",
        top: 12,
        left: "50%",
        transform: "translateX(-50%)",
        width: 400,
        maxWidth: "92vw",
        zIndex: 90,
        background: "var(--color-elev)",
        border: "1px solid var(--color-line-2)",
        borderRadius: "var(--radius-md)",
        boxShadow: "0 16px 48px rgba(0,0,0,.55)",
        padding: "13px 15px",
        fontFamily: "var(--font-ui)",
      }}
    >
      {status === "available" && (
        <>
          <div style={{ fontSize: 13, fontWeight: 600, color: "var(--color-t1)" }}>
            Update available
          </div>
          <div
            style={{
              fontSize: 11.5,
              color: "var(--color-t3)",
              marginTop: 3,
              marginBottom: 11,
              lineHeight: 1.5,
            }}
          >
            Version {version} is ready to install. Download it now, then restart
            when you're ready.
          </div>
          <div style={{ display: "flex", gap: 8 }}>
            <button style={primaryBtn} onClick={() => void downloadAndInstall()}>
              Download &amp; install
            </button>
            <button style={ghostBtn} onClick={dismissUpdate}>
              Later
            </button>
          </div>
        </>
      )}

      {status === "downloading" && (
        <>
          <div style={{ fontSize: 13, fontWeight: 600, color: "var(--color-t1)" }}>
            Downloading update{version ? ` ${version}` : ""}…
          </div>
          <div style={{ margin: "10px 0 6px" }}>
            <ProgressBar pct={pct} height={4} />
          </div>
          <div style={{ fontSize: 10.5, color: "var(--color-t3)" }}>
            {bytesTotal > 0
              ? `${fmtMB(bytesDone)} / ${fmtMB(bytesTotal)} · ${pct}%`
              : `${fmtMB(bytesDone)} downloaded`}
          </div>
        </>
      )}

      {status === "ready" && (
        <>
          <div style={{ fontSize: 13, fontWeight: 600, color: "var(--color-t1)" }}>
            Update ready
          </div>
          <div
            style={{
              fontSize: 11.5,
              color: "var(--color-t3)",
              marginTop: 3,
              marginBottom: 11,
              lineHeight: 1.5,
            }}
          >
            {version ? `Version ${version} is installed. ` : ""}Restart Darkroom
            to finish updating.
          </div>
          <div style={{ display: "flex", gap: 8 }}>
            <button style={primaryBtn} onClick={() => void restartApp()}>
              Restart now
            </button>
            <button style={ghostBtn} onClick={dismissUpdate}>
              Later
            </button>
          </div>
        </>
      )}

      {status === "error" && (
        <>
          <div style={{ fontSize: 13, fontWeight: 600, color: "var(--color-t1)" }}>
            Update failed
          </div>
          <div
            style={{
              fontSize: 11.5,
              color: "var(--color-t3)",
              marginTop: 3,
              marginBottom: 11,
              lineHeight: 1.5,
              overflow: "hidden",
              textOverflow: "ellipsis",
              whiteSpace: "nowrap",
            }}
            title={error ?? undefined}
          >
            {error}
          </div>
          <div style={{ display: "flex", gap: 8 }}>
            {version && (
              <button
                style={primaryBtn}
                onClick={() => void downloadAndInstall()}
              >
                Retry
              </button>
            )}
            <button style={ghostBtn} onClick={dismissUpdate}>
              Dismiss
            </button>
          </div>
        </>
      )}
    </div>
  );
}
