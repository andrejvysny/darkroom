import { thumbUrl, type DupImage } from "../../lib/ipc";
import { fmtBytes, fmtExif } from "./helpers";

interface PreviewOverlayProps {
  img: DupImage;
  previewEdge: number;
  onClose: () => void;
}

/** Full-screen sharp preview of one frame. Closed via click or Escape (handled by the parent). */
export default function PreviewOverlay({
  img,
  previewEdge,
  onClose,
}: PreviewOverlayProps) {
  return (
    <div
      onClick={onClose}
      style={{
        position: "fixed",
        inset: 0,
        background: "rgba(0,0,0,.93)",
        zIndex: 100,
        display: "flex",
        flexDirection: "column",
        alignItems: "center",
        justifyContent: "center",
        padding: 20,
      }}
    >
      <img
        src={thumbUrl(img.contentHash, 512, null, null, previewEdge)}
        alt={img.filename}
        onClick={(e) => e.stopPropagation()}
        style={{
          maxWidth: "95vw",
          maxHeight: "88vh",
          objectFit: "contain",
          borderRadius: 4,
          boxShadow: "0 8px 64px rgba(0,0,0,.7)",
        }}
      />
      <div
        style={{
          marginTop: 14,
          fontSize: 12,
          color: "rgba(255,255,255,.55)",
          fontFamily: "var(--font-mono)",
          display: "flex",
          gap: 16,
        }}
      >
        <span>{img.filename}</span>
        <span>{fmtBytes(img.fileSize)}</span>
        <span>{fmtExif(img)}</span>
        <span style={{ color: "rgba(255,255,255,.3)" }}>
          Esc or click to close
        </span>
      </div>
    </div>
  );
}
