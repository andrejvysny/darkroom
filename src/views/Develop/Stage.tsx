const PLACEHOLDER_SRC =
  "data:image/svg+xml;base64,PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHZpZXdCb3g9IjAgMCAxNTAwIDEwMDAiPgo8ZGVmcz4KPGxpbmVhckdyYWRpZW50IGlkPSJzIiB4MT0iMCIgeTE9IjAiIHgyPSIwIiB5Mj0iMSI+CjxzdG9wIG9mZnNldD0iMCIgc3RvcC1jb2xvcj0iIzViN2I5NiIvPjxzdG9wIG9mZnNldD0iLjM4IiBzdG9wLWNvbG9yPSIjODY5NTlhIi8+CjxzdG9wIG9mZnNldD0iLjUyIiBzdG9wLWNvbG9yPSIjOWE4YTZlIi8+PHN0b3Agb2Zmc2V0PSIuNjYiIHN0b3AtY29sb3I9IiM1YzUzNDAiLz4KPHN0b3Agb2Zmc2V0PSIxIiBzdG9wLWNvbG9yPSIjMzMyYzIyIi8+PC9saW5lYXJHcmFkaWVudD4KPHJhZGlhbEdyYWRpZW50IGlkPSJ1IiBjeD0iLjUiIGN5PSIuMSIgcj0iLjU1Ij4KPHN0b3Agb2Zmc2V0PSIwIiBzdG9wLWNvbG9yPSIjZTZiZDdkIi8+PHN0b3Agb2Zmc2V0PSIuNDUiIHN0b3AtY29sb3I9IiNiODg5NWEiIHN0b3Atb3BhY2l0eT0iLjUiLz4KPHN0b3Agb2Zmc2V0PSIuOCIgc3RvcC1jb2xvcj0iI2I4ODk1YSIgc3RvcC1vcGFjaXR5PSIwIi8+PC9yYWRpYWxHcmFkaWVudD4KPHJhZGlhbEdyYWRpZW50IGlkPSJ2IiBjeD0iLjUiIGN5PSIuNSIgcj0iLjc1Ij4KPHN0b3Agb2Zmc2V0PSIuNTgiIHN0b3AtY29sb3I9IiMwMDAiIHN0b3Atb3BhY2l0eT0iMCIvPjxzdG9wIG9mZnNldD0iMSIgc3RvcC1jb2xvcj0iIzAwMCIgc3RvcC1vcGFjaXR5PSIuNCIvPjwvcmFkaWFsR3JhZGllbnQ+CjwvZGVmcz4KPHJlY3Qgd2lkdGg9IjE1MDAiIGhlaWdodD0iMTAwMCIgZmlsbD0idXJsKCNzKSIvPgo8cmVjdCB3aWR0aD0iMTUwMCIgaGVpZ2h0PSIxMDAwIiBmaWxsPSJ1cmwoI3UpIi8+CjxyZWN0IHdpZHRoPSIxNTAwIiBoZWlnaHQ9IjEwMDAiIGZpbGw9InVybCgjdikiLz4KPC9zdmc+Cg==";

interface StageProps {
  /** True while the original (unedited) render is shown. */
  showBefore: boolean;
  /** Press-and-hold the image to preview the original; release to return. */
  onHoldBefore: (active: boolean) => void;
  imageUrl: string | null;
  rendering: boolean;
}

export default function Stage({
  showBefore,
  onHoldBefore,
  imageUrl,
  rendering,
}: StageProps) {
  const src = imageUrl ?? PLACEHOLDER_SRC;

  return (
    <section
      style={{
        flex: "1 1 auto",
        background: "var(--color-stage-dev)",
        position: "relative",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        padding: 40,
        minWidth: 0,
        minHeight: 0,
        overflow: "hidden",
      }}
    >
      <img
        src={src}
        alt="RAW preview"
        onPointerDown={() => onHoldBefore(true)}
        onPointerUp={() => onHoldBefore(false)}
        onPointerLeave={() => onHoldBefore(false)}
        style={{
          display: "block",
          maxWidth: "100%",
          maxHeight: "100%",
          width: "auto",
          height: "auto",
          borderRadius: 3,
          boxShadow:
            "0 10px 50px rgba(0,0,0,.55), 0 0 0 1px rgba(255,255,255,.06)",
          userSelect: "none",
          cursor: "default",
          opacity: rendering ? 0.7 : 1,
        }}
      />
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
        <span>Fit</span>
        <span
          style={{
            width: 4,
            height: 4,
            borderRadius: "50%",
            background: "var(--color-t3)",
            display: "block",
          }}
        />
        <span>7008 × 4672</span>
        <span
          style={{
            width: 4,
            height: 4,
            borderRadius: "50%",
            background: "var(--color-t3)",
            display: "block",
          }}
        />
        <span>Display P3</span>
      </div>
    </section>
  );
}
