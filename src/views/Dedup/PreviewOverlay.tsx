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
      <div
        onClick={(e) => e.stopPropagation()}
        style={{
          position: "relative",
          display: "inline-flex",
          borderRadius: 4,
          overflow: "hidden",
          boxShadow: "0 8px 64px rgba(0,0,0,.7)",
        }}
      >
        {/* Instant low-res thumb (cached 512) sizes the box and shows immediately. */}
        <img
          src={thumbUrl(img.contentHash, 512, null, null)}
          alt={img.filename}
          style={{
            display: "block",
            maxWidth: "95vw",
            maxHeight: "88vh",
            objectFit: "contain",
          }}
        />
        {/* Sharp preview overlays the thumb and replaces it once decoded. */}
        <img
          src={thumbUrl(img.contentHash, 512, null, null, previewEdge)}
          alt=""
          style={{
            position: "absolute",
            inset: 0,
            width: "100%",
            height: "100%",
            objectFit: "contain",
          }}
        />
      </div>
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
