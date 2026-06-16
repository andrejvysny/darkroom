import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import MaskOverlay from "./MaskOverlay";
import { useDevelopStore } from "../../store/develop";
import type { BrushStroke, ComponentKind, Mask } from "../../lib/ipc";

const PLACEHOLDER_SRC =
  "data:image/svg+xml;base64,PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHZpZXdCb3g9IjAgMCAxNTAwIDEwMDAiPgo8ZGVmcz4KPGxpbmVhckdyYWRpZW50IGlkPSJzIiB4MT0iMCIgeTE9IjAiIHgyPSIwIiB5Mj0iMSI+CjxzdG9wIG9mZnNldD0iMCIgc3RvcC1jb2xvcj0iIzViN2I5NiIvPjxzdG9wIG9mZnNldD0iLjM4IiBzdG9wLWNvbG9yPSIjODY5NTlhIi8+CjxzdG9wIG9mZnNldD0iLjUyIiBzdG9wLWNvbG9yPSIjOWE4YTZlIi8+PHN0b3Agb2Zmc2V0PSIuNjYiIHN0b3AtY29sb3I9IiM1YzUzNDAiLz4KPHN0b3Agb2Zmc2V0PSIxIiBzdG9wLWNvbG9yPSIjMzMyYzIyIi8+PC9saW5lYXJHcmFkaWVudD4KPHJhZGlhbEdyYWRpZW50IGlkPSJ1IiBjeD0iLjUiIGN5PSIuMSIgcj0iLjU1Ij4KPHN0b3Agb2Zmc2V0PSIwIiBzdG9wLWNvbG9yPSIjZTZiZDdkIi8+PHN0b3Agb2Zmc2V0PSIuNDUiIHN0b3AtY29sb3I9IiNiODg5NWEiIHN0b3Atb3BhY2l0eT0iLjUiLz4KPHN0b3Agb2Zmc2V0PSIuOCIgc3RvcC1jb2xvcj0iI2I4ODk1YSIgc3RvcC1vcGFjaXR5PSIwIi8+PC9yYWRpYWxHcmFkaWVudD4KPHJhZGlhbEdyYWRpZW50IGlkPSJ2IiBjeD0iLjUiIGN5PSIuNSIgcj0iLjc1Ij4KPHN0b3Agb2Zmc2V0PSIuNTgiIHN0b3AtY29sb3I9IiMwMDAiIHN0b3Atb3BhY2l0eT0iMCIvPjxzdG9wIG9mZnNldD0iMSIgc3RvcC1jb2xvcj0iIzAwMCIgc3RvcC1vcGFjaXR5PSIuNCIvPjwvcmFkaWFsR3JhZGllbnQ+CjwvZGVmcz4KPHJlY3Qgd2lkdGg9IjE1MDAiIGhlaWdodD0iMTAwMCIgZmlsbD0idXJsKCNzKSIvPgo8cmVjdCB3aWR0aD0iMTUwMCIgaGVpZ2h0PSIxMDAwIiBmaWxsPSJ1cmwoI3UpIi8+CjxyZWN0IHdpZHRoPSIxNTAwIiBoZWlnaHQ9IjEwMDAiIGZpbGw9InVybCgjdikiLz4KPC9zdmc+Cg==";

const PAD = 40;

interface StageProps {
  /** True while the original (unedited) render is shown. */
  showBefore: boolean;
  imageUrl: string | null;
  /** Instant embedded-JPEG preview, shown until the processed render lands. */
  previewUrl: string | null;
  rendering: boolean;
  masks: Mask[];
  onChangeMaskKind: (
    index: number,
    compIndex: number,
    kind: ComponentKind,
  ) => void;
  onCommitStroke: (index: number, stroke: BrushStroke) => void;
}

export default function Stage({
  showBefore,
  imageUrl,
  previewUrl,
  rendering,
  masks,
  onChangeMaskKind,
  onCommitStroke,
}: StageProps) {
  const src = imageUrl ?? previewUrl ?? PLACEHOLDER_SRC;
  const selectedMaskIndex = useDevelopStore((s) => s.selectedMaskIndex);
  const selectedComponentIndex = useDevelopStore(
    (s) => s.selectedComponentIndex,
  );
  const maskOverlayVisible = useDevelopStore((s) => s.maskOverlayVisible);
  const brush = useDevelopStore((s) => s.brush);

  const sectionRef = useRef<HTMLElement | null>(null);
  const [avail, setAvail] = useState({ w: 0, h: 0 });
  const [nat, setNat] = useState({ w: 3, h: 2 }); // image natural aspect
  const [zoom, setZoom] = useState(1);
  const [pan, setPan] = useState({ x: 0, y: 0 });

  // Measure the available stage area (minus padding).
  useLayoutEffect(() => {
    const el = sectionRef.current;
    if (!el) return;
    const measure = () =>
      setAvail({
        w: Math.max(0, el.clientWidth - PAD * 2),
        h: Math.max(0, el.clientHeight - PAD * 2),
      });
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  // Reset zoom/pan when the image changes.
  useEffect(() => {
    setZoom(1);
    setPan({ x: 0, y: 0 });
  }, [imageUrl]);

  // Fit-to-area displayed size (before zoom).
  const fit = Math.min(avail.w / nat.w, avail.h / nat.h) || 0;
  const dispW = Math.max(1, nat.w * fit);
  const dispH = Math.max(1, nat.h * fit);

  const onWheel = useCallback((e: React.WheelEvent) => {
    e.preventDefault();
    setZoom((z) => Math.min(8, Math.max(0.5, z * (1 - e.deltaY * 0.0015))));
  }, []);

  // Background drag = pan. Mask handles stop propagation, so they never trigger a pan.
  const onPointerDown = useCallback(
    (e: React.PointerEvent) => {
      if (e.button !== 0) return;
      const start = { x: e.clientX, y: e.clientY };
      const startPan = pan;
      const move = (ev: PointerEvent) =>
        setPan({
          x: startPan.x + (ev.clientX - start.x),
          y: startPan.y + (ev.clientY - start.y),
        });
      const up = () => {
        window.removeEventListener("pointermove", move);
        window.removeEventListener("pointerup", up);
      };
      window.addEventListener("pointermove", move);
      window.addEventListener("pointerup", up);
    },
    [pan],
  );

  const resetView = useCallback(() => {
    setZoom(1);
    setPan({ x: 0, y: 0 });
  }, []);

  const selectedMask =
    selectedMaskIndex !== null ? masks[selectedMaskIndex] : undefined;
  const showOverlay =
    maskOverlayVisible &&
    !showBefore &&
    selectedMask !== undefined &&
    !!imageUrl;

  return (
    <section
      ref={sectionRef}
      onWheel={onWheel}
      onPointerDown={onPointerDown}
      onDoubleClick={resetView}
      style={{
        flex: "1 1 auto",
        background: "var(--color-stage-dev)",
        position: "relative",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        padding: PAD,
        minWidth: 0,
        minHeight: 0,
        overflow: "hidden",
      }}
    >
      <div
        style={{
          position: "relative",
          width: dispW,
          height: dispH,
          transform: `translate(${pan.x}px, ${pan.y}px) scale(${zoom})`,
          transformOrigin: "center center",
          boxShadow:
            "0 10px 50px rgba(0,0,0,.55), 0 0 0 1px rgba(255,255,255,.06)",
          borderRadius: 3,
        }}
      >
        <img
          src={src}
          alt="RAW preview"
          onLoad={(e) => {
            const im = e.currentTarget;
            if (im.naturalWidth > 0)
              setNat({ w: im.naturalWidth, h: im.naturalHeight });
          }}
          style={{
            display: "block",
            width: "100%",
            height: "100%",
            borderRadius: 3,
            userSelect: "none",
            pointerEvents: "none",
            opacity: rendering ? 0.7 : 1,
          }}
        />
        {showOverlay && (
          <MaskOverlay
            width={dispW}
            height={dispH}
            mask={selectedMask}
            compIndex={Math.min(
              selectedComponentIndex,
              selectedMask.components.length - 1,
            )}
            onChangeKind={(kind) =>
              onChangeMaskKind(
                selectedMaskIndex!,
                Math.min(
                  selectedComponentIndex,
                  selectedMask.components.length - 1,
                ),
                kind,
              )
            }
            brush={brush}
            onCommitStroke={(stroke) =>
              onCommitStroke(selectedMaskIndex!, stroke)
            }
            imageUrl={imageUrl}
          />
        )}
      </div>

      {showBefore && (
        <div
          style={{
            position: "absolute",
            top: 14,
            left: 16,
            padding: "3px 8px",
            borderRadius: "var(--radius-sm)",
            background: "var(--color-accent)",
            color: "#fff",
            fontSize: 10.5,
            fontWeight: 600,
            letterSpacing: 0.5,
            fontFamily: "var(--font-mono)",
          }}
        >
          BEFORE
        </div>
      )}
      {rendering && (
        <div
          style={{
            position: "absolute",
            top: 14,
            right: 14,
            width: 8,
            height: 8,
            borderRadius: "50%",
            background: "var(--color-accent)",
            opacity: 0.8,
          }}
        />
      )}
      <div
        style={{
          position: "absolute",
          left: 16,
          bottom: 14,
          display: "flex",
          alignItems: "center",
          gap: 14,
          fontSize: 11.5,
          color: "var(--color-t3)",
          fontFamily: "var(--font-mono)",
        }}
      >
        <span
          onClick={resetView}
          style={{ cursor: "pointer" }}
          title="Reset zoom (double-click stage)"
        >
          {Math.round(zoom * 100)}%
        </span>
        <Dot />
        <span>
          {nat.w} × {nat.h}
        </span>
        <Dot />
        <span>Display P3</span>
      </div>
    </section>
  );
}

function Dot() {
  return (
    <span
      style={{
        width: 4,
        height: 4,
        borderRadius: "50%",
        background: "var(--color-t3)",
        display: "block",
      }}
    />
  );
}
