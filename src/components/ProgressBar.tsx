/** Bare progress track — a filled accent bar clamped to [0,100]%. Shared by the model manager, the
 *  global download pill, and the People sidebar. */
export function ProgressBar({
  pct,
  height = 3,
  color,
}: {
  pct: number;
  height?: number;
  color?: string;
}) {
  const w = Math.max(0, Math.min(100, pct));
  return (
    <div
      style={{
        height,
        borderRadius: 2,
        background: "var(--color-line)",
        overflow: "hidden",
      }}
    >
      <div
        style={{
          height: "100%",
          width: `${w}%`,
          background: color ?? "var(--color-accent)",
          transition: "width .2s ease",
        }}
      />
    </div>
  );
}
