import { useRef, useState, useCallback, useEffect } from "react";
import { thumbUrl } from "../../lib/ipc";
import type { ImageRow } from "../../lib/ipc";

interface LoupeProps {
  image: ImageRow;
}

const MIN_ZOOM = 0.5;
const MAX_ZOOM = 6;

export default function Loupe({ image }: LoupeProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const [zoom, setZoom] = useState(1);
  // pan offset in px, relative to centered fit position
  const [pan, setPan] = useState({ x: 0, y: 0 });
  const [isDragging, setIsDragging] = useState(false);
  const dragging = useRef(false);
  const dragStart = useRef({ mx: 0, my: 0, px: 0, py: 0 });

  // Reset to fit-view when image changes
  useEffect(() => {
    setZoom(1);
    setPan({ x: 0, y: 0 });
  }, [image.id]);

  const handleWheel = useCallback((e: WheelEvent) => {
    e.preventDefault();
    const delta = -e.deltaY * 0.001;
    setZoom((z) => Math.max(MIN_ZOOM, Math.min(MAX_ZOOM, z * (1 + delta * 3))));
  }, []);

  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    el.addEventListener("wheel", handleWheel, { passive: false });
    return () => el.removeEventListener("wheel", handleWheel);
  }, [handleWheel]);

  function handleMouseDown(e: React.MouseEvent) {
    if (e.button !== 0) return;
    dragging.current = true;
    setIsDragging(true);
    dragStart.current = { mx: e.clientX, my: e.clientY, px: pan.x, py: pan.y };
    e.preventDefault();
  }

  function handleMouseMove(e: React.MouseEvent) {
    if (!dragging.current) return;
    setPan({
      x: dragStart.current.px + (e.clientX - dragStart.current.mx),
      y: dragStart.current.py + (e.clientY - dragStart.current.my),
    });
  }

  function handleMouseUp() {
    dragging.current = false;
    setIsDragging(false);
  }

  function handleDoubleClick() {
    setZoom(1);
    setPan({ x: 0, y: 0 });
  }

  const src = thumbUrl(image.contentHash, 1024);

  return (
    <div
      ref={containerRef}
      onMouseDown={handleMouseDown}
      onMouseMove={handleMouseMove}
      onMouseUp={handleMouseUp}
      onMouseLeave={handleMouseUp}
      onDoubleClick={handleDoubleClick}
      style={{
        width: "100%",
        height: "100%",
        overflow: "hidden",
        background: "var(--color-stage)",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        cursor: isDragging ? "grabbing" : "grab",
        userSelect: "none",
      }}
    >
      <img
        src={src}
        alt={image.filename}
        draggable={false}
        style={{
          maxWidth: "none",
          maxHeight: "none",
          width: "100%",
          height: "100%",
          objectFit: "contain",
          transform: `translate(${pan.x}px, ${pan.y}px) scale(${zoom})`,
          transformOrigin: "center center",
          transition: isDragging ? "none" : "transform .05s ease-out",
          pointerEvents: "none",
        }}
      />
    </div>
  );
}
