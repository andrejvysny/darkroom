import type { DupImage } from "../../lib/ipc";

/** Frame outline colour by verdict: keeper green, rejected red, focused accent, else faint. */
export function borderColor(
  img: DupImage,
  keeperId: number,
  focusId: number,
  isStaged: (id: number) => boolean,
): string {
  if (img.id === keeperId) return "var(--color-pick)";
  if (isStaged(img.id)) return "var(--color-reject)";
  if (img.id === focusId) return "var(--color-accent)";
  return "rgba(255,255,255,.12)";
}

export function Badge({
  img,
  keeperId,
  isStaged,
  showNeutral = true,
}: {
  img: DupImage;
  keeperId: number;
  isStaged: (id: number) => boolean;
  /** Show a "CHALLENGER" badge for plain (non-keeper, non-rejected) frames. */
  showNeutral?: boolean;
}) {
  const staged = isStaged(img.id);
  const isKeeper = img.id === keeperId;
  if (!isKeeper && !staged && !showNeutral) return null;
  const text = isKeeper ? "KEEPER" : staged ? "REJECTED" : "CHALLENGER";
  const bg = isKeeper
    ? "var(--color-pick)"
    : staged
      ? "var(--color-reject)"
      : "rgba(0,0,0,.6)";
  const color = isKeeper
    ? "#0e1a10"
    : staged
      ? "#1a0e0e"
      : "var(--color-accent)";
  return (
    <div
      style={{
        position: "absolute",
        top: 10,
        left: 10,
        fontSize: 11,
        fontWeight: 700,
        letterSpacing: ".04em",
        padding: "4px 9px",
        borderRadius: 6,
        background: bg,
        color,
      }}
    >
      {text}
    </div>
  );
}

export function Caption({ img, exif }: { img: DupImage; exif: string }) {
  return (
    <div
      style={{
        position: "absolute",
        left: 0,
        right: 0,
        bottom: 0,
        padding: "20px 12px 10px",
        background: "linear-gradient(transparent,rgba(0,0,0,.78))",
      }}
    >
      <div
        style={{ fontFamily: "var(--font-mono)", fontSize: 12, color: "#fff" }}
      >
        {img.filename}
      </div>
      <div style={{ fontSize: 11, color: "rgba(255,255,255,.6)" }}>{exif}</div>
    </div>
  );
}
