import { useCallback, useRef } from "react";
import type { Crop } from "../../lib/ipc";

interface CropOverlayProps {
  width: number;
  height: number;
  crop: Crop;
  onChange: (patch: Partial<Crop>) => void;
}

const MIN_HALF = 0.02; // smallest crop half-extent (4% of the edge)

type Handle = "move" | "nw" | "ne" | "sw" | "se";

/** Draggable crop-rectangle overlay shown over the full (un-cropped) image while the crop tool is
 * active. Works in normalized image coords; the host div is sized to the fit-displayed image, so
 * pointer positions normalize via the container's bounding rect (zoom/pan-safe). */
export default function CropOverlay({
  width,
  height,
  crop,
  onChange,
}: CropOverlayProps) {
  const ref = useRef<HTMLDivElement | null>(null);

  // Crop edges in normalized [0,1] (left/right/top/bottom).
  const l = crop.cx - crop.hw;
  const r = crop.cx + crop.hw;
  const t = crop.cy - crop.hh;
  const b = crop.cy + crop.hh;

  const startDrag = useCallback(
    (handle: Handle) => (e: React.PointerEvent) => {
      e.preventDefault();
      e.stopPropagation();
      const box = ref.current;
      if (!box) return;
      const rect = box.getBoundingClientRect();
      const start = { x: e.clientX, y: e.clientY };
      const e0 = {
        l: crop.cx - crop.hw,
        r: crop.cx + crop.hw,
        t: crop.cy - crop.hh,
        b: crop.cy + crop.hh,
      };

      const move = (ev: PointerEvent) => {
        const dx = (ev.clientX - start.x) / rect.width;
        const dy = (ev.clientY - start.y) / rect.height;
        let { l: nl, r: nr, t: nt, b: nb } = e0;
        if (handle === "move") {
          const w = e0.r - e0.l;
          const h = e0.b - e0.t;
          nl = Math.min(Math.max(e0.l + dx, 0), 1 - w);
          nt = Math.min(Math.max(e0.t + dy, 0), 1 - h);
          nr = nl + w;
          nb = nt + h;
        } else {
          if (handle === "nw" || handle === "sw")
            nl = Math.min(Math.max(e0.l + dx, 0), e0.r - 2 * MIN_HALF);
          if (handle === "ne" || handle === "se")
            nr = Math.max(Math.min(e0.r + dx, 1), e0.l + 2 * MIN_HALF);
          if (handle === "nw" || handle === "ne")
            nt = Math.min(Math.max(e0.t + dy, 0), e0.b - 2 * MIN_HALF);
          if (handle === "sw" || handle === "se")
            nb = Math.max(Math.min(e0.b + dy, 1), e0.t + 2 * MIN_HALF);
        }
        onChange({
          cx: (nl + nr) / 2,
          cy: (nt + nb) / 2,
          hw: (nr - nl) / 2,
          hh: (nb - nt) / 2,
        });
      };
      const up = () => {
        window.removeEventListener("pointermove", move);
        window.removeEventListener("pointerup", up);
      };
      window.addEventListener("pointermove", move);
      window.addEventListener("pointerup", up);
    },
    [crop, onChange],
  );

  const corner = (h: Handle, cx: number, cy: number, cursor: string) => (
    <div
      onPointerDown={startDrag(h)}
      style={{
        position: "absolute",
        left: `${cx * 100}%`,
        top: `${cy * 100}%`,
        width: 14,
        height: 14,
        transform: "translate(-50%, -50%)",
        border: "2px solid #fff",
        background: "rgba(0,0,0,0.35)",
        borderRadius: 2,
        cursor,
        pointerEvents: "auto",
      }}
    />
  );

  return (
    <div
      ref={ref}
      style={{
        position: "absolute",
        inset: 0,
        width,
        height,
        pointerEvents: "none",
      }}
    >
      {/* Crop rectangle: dims everything outside via a huge spread shadow. */}
      <div
        onPointerDown={startDrag("move")}
        style={{
          position: "absolute",
          left: `${l * 100}%`,
          top: `${t * 100}%`,
          width: `${(r - l) * 100}%`,
          height: `${(b - t) * 100}%`,
          boxShadow: "0 0 0 9999px rgba(0,0,0,0.5)",
          outline: "1px solid rgba(255,255,255,0.9)",
          cursor: "move",
          pointerEvents: "auto",
        }}
      >
        {/* Rule-of-thirds guides. */}
        {[1 / 3, 2 / 3].map((f) => (
          <div
            key={`v${f}`}
            style={{
              position: "absolute",
              left: `${f * 100}%`,
              top: 0,
              bottom: 0,
              width: 1,
              background: "rgba(255,255,255,0.3)",
            }}
          />
        ))}
        {[1 / 3, 2 / 3].map((f) => (
          <div
            key={`h${f}`}
            style={{
              position: "absolute",
              top: `${f * 100}%`,
              left: 0,
              right: 0,
              height: 1,
              background: "rgba(255,255,255,0.3)",
            }}
          />
        ))}
      </div>
      {corner("nw", l, t, "nwse-resize")}
      {corner("ne", r, t, "nesw-resize")}
      {corner("sw", l, b, "nesw-resize")}
      {corner("se", r, b, "nwse-resize")}
    </div>
  );
}
