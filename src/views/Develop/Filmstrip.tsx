import { useEffect, useRef } from "react";
import { useAppStore } from "../../store/app";
import { thumbUrl } from "../../lib/ipc";

function StarRow({ count }: { count: number }) {
  return (
    <div style={{ display: "flex", gap: 1 }}>
      {[1, 2, 3, 4, 5].map((n) => (
        <svg
          key={n}
          viewBox="0 0 16 16"
          width={13}
          height={13}
          fill={n <= count ? "var(--color-star)" : "none"}
          stroke={n <= count ? "var(--color-star)" : "var(--color-t3)"}
          strokeWidth="1.2"
          style={{ display: "block" }}
        >
          <path d="M8 2.2l1.7 3.6 3.9.5-2.9 2.7.8 3.9L8 11l-3.5 1.9.8-3.9L2.4 6.3l3.9-.5z" />
        </svg>
      ))}
    </div>
  );
}

export default function Filmstrip() {
  const images = useAppStore((s) => s.libraryImages);
  const selectedId = useAppStore((s) => s.selectedId);
  const setSelectedId = useAppStore((s) => s.setSelectedId);

  const selRef = useRef<HTMLButtonElement>(null);
  const current = images.find((i) => i.id === selectedId) ?? null;

  // Keep the active thumbnail visible as the selection moves.
  useEffect(() => {
    selRef.current?.scrollIntoView({ inline: "center", block: "nearest" });
  }, [selectedId]);

  return (
    <footer
      style={{
        display: "flex",
        alignItems: "center",
        gap: 14,
        padding: "0 14px",
        background: "var(--color-app)",
        borderTop: "1px solid var(--color-line)",
        height: 84,
        flexShrink: 0,
      }}
    >
      {/* Meta: selected image's rating + pick/reject flag */}
      <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
        <StarRow count={current?.stars ?? 0} />
        {current?.flag === "pick" && (
          <span style={{ fontSize: 12, color: "var(--color-pick)" }}>●</span>
        )}
        {current?.flag === "reject" && (
          <span style={{ fontSize: 12, color: "var(--color-reject)" }}>⌀</span>
        )}
      </div>

      {/* Strip */}
      <div
        style={{
          flex: 1,
          display: "flex",
          gap: 8,
          overflowX: "auto",
          padding: "14px 0",
          height: 84,
          alignItems: "center",
          scrollbarWidth: "none",
        }}
      >
        {images.length === 0 && (
          <span style={{ fontSize: 12, color: "var(--color-t3)" }}>
            No photos
          </span>
        )}
        {images.map((img) => {
          const active = img.id === selectedId;
          return (
            <button
              key={img.id}
              ref={active ? selRef : undefined}
              onClick={() => setSelectedId(img.id)}
              title={img.filename}
              style={{
                height: 54,
                aspectRatio: "3/2",
                borderRadius: 4,
                flexShrink: 0,
                padding: 0,
                border: "none",
                outline: active
                  ? "2px solid var(--color-accent)"
                  : "1px solid var(--color-line)",
                outlineOffset: active ? -1 : 0,
                position: "relative",
                cursor: "pointer",
                overflow: "hidden",
                background: "#1a1a1a",
              }}
            >
              <img
                src={thumbUrl(img.contentHash, 256, img.editedAt)}
                alt={img.filename}
                loading="lazy"
                draggable={false}
                style={{
                  width: "100%",
                  height: "100%",
                  objectFit: "cover",
                  display: "block",
                  opacity: active ? 1 : 0.82,
                }}
              />
            </button>
          );
        })}
      </div>
    </footer>
  );
}
