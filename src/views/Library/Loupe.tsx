import {
  useRef,
  useState,
  useCallback,
  useEffect,
  useLayoutEffect,
} from "react";
import { thumbUrl, loupeJpeg } from "../../lib/ipc";
import type { ImageRow } from "../../lib/ipc";

interface LoupeProps {
  image: ImageRow;
}

const FIT_PREVIEW_EDGE = 2560; // sharp fit-view source
// Once the on-screen long edge exceeds the 2560 preview, upgrade to native full-res.
const FULL_RES_TRIGGER_PX = FIT_PREVIEW_EDGE;
const MIN_SCALE = 1; // 1 = fit-to-container

export default function Loupe({ image }: LoupeProps) {
  const containerRef = useRef<HTMLDivElement>(null);

  const [src, setSrc] = useState(() =>
    thumbUrl(image.contentHash, 512, image.editedAt),
  );
  const [scale, setScale] = useState(1);
  const [offset, setOffset] = useState({ x: 0, y: 0 });
  const [isDragging, setIsDragging] = useState(false);
  // Container size in CSS px; remeasured on resize.
  const [container, setContainer] = useState({ w: 0, h: 0 });
  // Natural pixel dimensions of the photo (for contain-fit + 1:1 ceiling).
  const [natural, setNatural] = useState({
    w: image.width ?? 0,
    h: image.height ?? 0,
  });

  // Mirrors so the (passive:false) wheel listener and drag handlers read live values.
  const scaleRef = useRef(scale);
  scaleRef.current = scale;
  const offsetRef = useRef(offset);
  offsetRef.current = offset;
  const containerRefVal = useRef(container);
  containerRefVal.current = container;
  const naturalRef = useRef(natural);
  naturalRef.current = natural;

  const dragging = useRef(false);
  const dragStart = useRef({ mx: 0, my: 0, ox: 0, oy: 0 });
  // Object URLs to revoke on unmount / image change.
  const urls = useRef<string[]>([]);
  const fullRequested = useRef(false);

  // ── Geometry helpers (pure, read from refs so they're stable) ─────────────
  const fittedSize = useCallback(() => {
    const { w: cw, h: ch } = containerRefVal.current;
    const nw = naturalRef.current.w || cw;
    const nh = naturalRef.current.h || ch;
    if (!cw || !ch || !nw || !nh) return { w: cw, h: ch };
    const ar = nw / nh;
    if (cw / ch > ar) return { w: ch * ar, h: ch };
    return { w: cw, h: cw / ar };
  }, []);

  const maxScale = useCallback(() => {
    const f = fittedSize();
    const nw = naturalRef.current.w;
    if (!f.w || !nw) return 8;
    return Math.max(MIN_SCALE, nw / f.w); // 1:1 ceiling
  }, [fittedSize]);

  // Keep image within pan bounds; centered on the axes where it fits.
  const clampOffset = useCallback(
    (off: { x: number; y: number }, s: number) => {
      const f = fittedSize();
      const c = containerRefVal.current;
      const maxX = Math.max(0, (f.w * s - c.w) / 2);
      const maxY = Math.max(0, (f.h * s - c.h) / 2);
      return {
        x: Math.max(-maxX, Math.min(maxX, off.x)),
        y: Math.max(-maxY, Math.min(maxY, off.y)),
      };
    },
    [fittedSize],
  );

  const pannable = useCallback(
    (s: number) => {
      const f = fittedSize();
      const c = containerRefVal.current;
      return f.w * s > c.w + 0.5 || f.h * s > c.h + 0.5;
    },
    [fittedSize],
  );

  // ── Progressive source loading ────────────────────────────────────────────
  // Preload off-DOM so the previous frame stays painted until the next decodes.
  const swapSrc = useCallback((url: string) => {
    const im = new Image();
    im.onload = () => setSrc(url);
    im.src = url;
  }, []);

  // Lazily upgrade to native full-res once zoomed past the 2560 preview's detail.
  const requestFull = useCallback(() => {
    if (fullRequested.current) return;
    fullRequested.current = true;
    loupeJpeg(image.id, 0)
      .then((url) => {
        urls.current.push(url);
        swapSrc(url);
      })
      .catch(() => {
        fullRequested.current = false; // allow retry on next zoom
      });
  }, [image.id, swapSrc]);

  // On image change: reset view, load the 2560 fit preview, revoke old URLs.
  useEffect(() => {
    setScale(1);
    setOffset({ x: 0, y: 0 });
    setSrc(thumbUrl(image.contentHash, 512, image.editedAt));
    setNatural({ w: image.width ?? 0, h: image.height ?? 0 });
    fullRequested.current = false;

    let cancelled = false;
    loupeJpeg(image.id, FIT_PREVIEW_EDGE)
      .then((url) => {
        if (cancelled) {
          URL.revokeObjectURL(url);
          return;
        }
        urls.current.push(url);
        swapSrc(url);
      })
      .catch(() => {});

    return () => {
      cancelled = true;
      for (const u of urls.current) URL.revokeObjectURL(u);
      urls.current = [];
    };
  }, [
    image.id,
    image.contentHash,
    image.width,
    image.height,
    image.editedAt,
    swapSrc,
  ]);

  // ── Container measurement ─────────────────────────────────────────────────
  useLayoutEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    const measure = () => {
      const r = el.getBoundingClientRect();
      setContainer({ w: r.width, h: r.height });
    };
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  // Apply a new scale anchored at a container-relative point (cx,cy from center).
  const zoomTo = useCallback(
    (nextScale: number, cx: number, cy: number) => {
      const prev = scaleRef.current;
      const s = Math.max(MIN_SCALE, Math.min(maxScale(), nextScale));
      const off = offsetRef.current;
      // content point under cursor stays put: off' = cur - (s/prev)*(cur - off)
      const k = s / prev;
      const next = clampOffset(
        { x: cx - k * (cx - off.x), y: cy - k * (cy - off.y) },
        s,
      );
      setScale(s);
      setOffset(next);
      const f = fittedSize();
      if (f.w * s > FULL_RES_TRIGGER_PX || f.h * s > FULL_RES_TRIGGER_PX) {
        requestFull();
      }
    },
    [maxScale, clampOffset, fittedSize, requestFull],
  );

  // ── Wheel zoom (native listener for passive:false preventDefault) ─────────
  const handleWheel = useCallback(
    (e: WheelEvent) => {
      e.preventDefault();
      const el = containerRef.current;
      if (!el) return;
      const r = el.getBoundingClientRect();
      const cx = e.clientX - r.left - r.width / 2;
      const cy = e.clientY - r.top - r.height / 2;
      const factor = Math.exp(-e.deltaY * 0.0015);
      zoomTo(scaleRef.current * factor, cx, cy);
    },
    [zoomTo],
  );

  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    el.addEventListener("wheel", handleWheel, { passive: false });
    return () => el.removeEventListener("wheel", handleWheel);
  }, [handleWheel]);

  // ── Drag-to-pan (only when zoomed past fit) ───────────────────────────────
  function handleMouseDown(e: React.MouseEvent) {
    if (e.button !== 0 || !pannable(scaleRef.current)) return;
    dragging.current = true;
    setIsDragging(true);
    dragStart.current = {
      mx: e.clientX,
      my: e.clientY,
      ox: offsetRef.current.x,
      oy: offsetRef.current.y,
    };
    e.preventDefault();
  }

  function handleMouseMove(e: React.MouseEvent) {
    if (!dragging.current) return;
    const next = {
      x: dragStart.current.ox + (e.clientX - dragStart.current.mx),
      y: dragStart.current.oy + (e.clientY - dragStart.current.my),
    };
    setOffset(clampOffset(next, scaleRef.current));
  }

  function handleMouseUp() {
    dragging.current = false;
    setIsDragging(false);
  }

  function handleDoubleClick(e: React.MouseEvent) {
    const el = containerRef.current;
    if (!el) return;
    const r = el.getBoundingClientRect();
    const cx = e.clientX - r.left - r.width / 2;
    const cy = e.clientY - r.top - r.height / 2;
    if (scaleRef.current > MIN_SCALE + 0.01) {
      setScale(1);
      setOffset({ x: 0, y: 0 });
    } else {
      zoomTo(maxScale(), cx, cy);
    }
  }

  const canPan = pannable(scale);
  const cursor = canPan ? (isDragging ? "grabbing" : "grab") : "zoom-in";

  return (
    <div
      ref={containerRef}
      data-testid="loupe"
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
        cursor,
        userSelect: "none",
      }}
    >
      <img
        src={src}
        alt={image.filename}
        draggable={false}
        onLoad={(e) => {
          const el = e.currentTarget;
          if (
            el.naturalWidth &&
            (!naturalRef.current.w || !naturalRef.current.h)
          ) {
            setNatural({ w: el.naturalWidth, h: el.naturalHeight });
          }
        }}
        style={{
          maxWidth: "none",
          maxHeight: "none",
          width: "100%",
          height: "100%",
          objectFit: "contain",
          transform: `translate(${offset.x}px, ${offset.y}px) scale(${scale})`,
          transformOrigin: "center center",
          transition: isDragging ? "none" : "transform .08s ease-out",
          pointerEvents: "none",
        }}
      />
    </div>
  );
}
