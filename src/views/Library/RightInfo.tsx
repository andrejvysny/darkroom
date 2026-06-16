import { thumbUrl, type ImageRow } from "../../lib/ipc";

const LABEL_COLORS: { key: string; bg: string }[] = [
  { key: "red", bg: "var(--color-lab-red)" },
  { key: "yellow", bg: "var(--color-lab-yellow)" },
  { key: "green", bg: "var(--color-lab-green)" },
  { key: "blue", bg: "var(--color-lab-blue)" },
  { key: "purple", bg: "var(--color-lab-purple)" },
];

function HistogramSvg() {
  const W = 232;
  const H = 64;

  function buildPath(seed: number, scale: number): string {
    let d = `M0 ${H} `;
    for (let x = 0; x <= W; x += 4) {
      const v = Math.sin(x * 0.05 + seed) * Math.cos(x * 0.013 + seed * 2);
      const y =
        H -
        (0.5 + 0.5 * v) *
          H *
          scale *
          (0.4 + 0.6 * Math.exp(-Math.pow((x - W * 0.45) / 110, 2)));
      d += `L${x} ${y.toFixed(1)} `;
    }
    return d + `L${W} ${H} Z`;
  }

  const channels: [string, number, number][] = [
    ["#c56d6d", 1.5, 0],
    ["#6db074", 2, 2.1],
    ["#5b93cf", 1.7, 4.2],
  ];

  return (
    <svg
      width="100%"
      height="100%"
      viewBox={`0 0 ${W} ${H}`}
      preserveAspectRatio="none"
    >
      {channels.map(([color, scale, seed]) => (
        <path
          key={color}
          d={buildPath(seed, scale)}
          fill={color}
          opacity={0.5}
          style={{ mixBlendMode: "screen" }}
        />
      ))}
    </svg>
  );
}

function formatDate(epoch: number): string {
  const d = new Date(epoch * 1000);
  const y = d.getFullYear();
  const m = String(d.getMonth() + 1).padStart(2, "0");
  const day = String(d.getDate()).padStart(2, "0");
  return `${y}-${m}-${day}`;
}

export interface RightInfoHandlers {
  onSetRating: (stars: number) => void;
  onSetFlag: (flag: "none" | "pick" | "reject") => void;
  onSetLabel: (label: string | null) => void;
}

interface RightInfoProps {
  selectedImage: ImageRow | null;
  handlers: RightInfoHandlers;
}

export default function RightInfo({ selectedImage, handlers }: RightInfoProps) {
  const meta = selectedImage;
  const stars = meta?.stars ?? 0;
  const flag = meta?.flag ?? "none";
  const activeLabel = meta?.colorLabel ?? "";

  const metaRows: { label: string; value: string }[] = meta
    ? [
        { label: "Camera", value: meta.cameraModel ?? "—" },
        { label: "Lens", value: meta.lens ?? "—" },
        {
          label: "Focal",
          value: meta.focalLength != null ? `${meta.focalLength} mm` : "—",
        },
        {
          label: "Aperture",
          value: meta.aperture != null ? `f/${meta.aperture}` : "—",
        },
        { label: "Shutter", value: meta.shutter ?? "—" },
        { label: "ISO", value: meta.iso != null ? String(meta.iso) : "—" },
        {
          label: "Size",
          value:
            meta.width != null && meta.height != null
              ? `${meta.width}×${meta.height}`
              : "—",
        },
        {
          label: "Date",
          value: meta.captureDate != null ? formatDate(meta.captureDate) : "—",
        },
        { label: "File", value: meta.filename },
      ]
    : [];

  const previewSrc = meta ? thumbUrl(meta.contentHash, 512) : null;

  return (
    <aside
      style={{
        background: "var(--color-app)",
        borderLeft: "1px solid var(--color-line)",
        overflowY: "auto",
        minHeight: 0,
        height: "100%",
      }}
    >
      {/* Preview */}
      <div
        style={{
          aspectRatio: "3/2",
          margin: 14,
          borderRadius: "var(--radius-md)",
          overflow: "hidden",
          outline: "1px solid var(--color-line)",
          position: "relative",
          background: "var(--color-stage)",
        }}
      >
        {previewSrc && (
          <img
            src={previewSrc}
            alt={meta?.filename ?? ""}
            style={{
              position: "absolute",
              inset: 0,
              width: "100%",
              height: "100%",
              objectFit: "cover",
            }}
          />
        )}
        <div
          style={{
            position: "absolute",
            inset: 0,
            boxShadow: "inset 0 0 50px rgba(0,0,0,.4)",
            pointerEvents: "none",
          }}
        />
      </div>

      {/* Rating */}
      <div
        style={{
          padding: "14px 16px",
          borderTop: "1px solid var(--color-line)",
        }}
      >
        <div
          style={{
            fontSize: 10.5,
            letterSpacing: ".06em",
            textTransform: "uppercase",
            color: "var(--color-t3)",
            fontWeight: 600,
            marginBottom: 10,
          }}
        >
          Rating
        </div>
        <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
          {/* Stars */}
          <div style={{ display: "flex", gap: 1 }}>
            {[1, 2, 3, 4, 5].map((n) => (
              <svg
                key={n}
                viewBox="0 0 16 16"
                width={16}
                height={16}
                fill={n <= stars ? "var(--color-star)" : "none"}
                stroke={n <= stars ? "var(--color-star)" : "var(--color-t3)"}
                strokeWidth="1.2"
                style={{
                  cursor: meta ? "pointer" : "default",
                  display: "block",
                }}
                onClick={() => {
                  if (!meta) return;
                  handlers.onSetRating(n === stars ? 0 : n);
                }}
              >
                <path d="M8 2.2l1.7 3.6 3.9.5-2.9 2.7.8 3.9L8 11l-3.5 1.9.8-3.9L2.4 6.3l3.9-.5z" />
              </svg>
            ))}
          </div>
          {/* Pick/Reject */}
          <div style={{ display: "flex", gap: 6, flex: 1 }}>
            <button
              disabled={!meta}
              onClick={() =>
                handlers.onSetFlag(flag === "pick" ? "none" : "pick")
              }
              style={{
                flex: 1,
                padding: 6,
                borderRadius: "var(--radius-sm)",
                border: "1px solid",
                fontSize: 12,
                fontWeight: 500,
                color:
                  flag === "pick" ? "var(--color-pick)" : "var(--color-t2)",
                borderColor:
                  flag === "pick"
                    ? "rgba(109,176,116,.4)"
                    : "var(--color-line)",
                background:
                  flag === "pick" ? "rgba(109,176,116,.1)" : "transparent",
                cursor: meta ? "pointer" : "default",
              }}
            >
              Pick
            </button>
            <button
              disabled={!meta}
              onClick={() =>
                handlers.onSetFlag(flag === "reject" ? "none" : "reject")
              }
              style={{
                flex: 1,
                padding: 6,
                borderRadius: "var(--radius-sm)",
                border: "1px solid",
                fontSize: 12,
                fontWeight: 500,
                color:
                  flag === "reject" ? "var(--color-reject)" : "var(--color-t2)",
                borderColor:
                  flag === "reject"
                    ? "rgba(197,109,109,.4)"
                    : "var(--color-line)",
                background:
                  flag === "reject" ? "rgba(197,109,109,.1)" : "transparent",
                cursor: meta ? "pointer" : "default",
              }}
            >
              Reject
            </button>
          </div>
        </div>
        {/* Color labels */}
        <div
          style={{
            display: "flex",
            gap: 6,
            alignItems: "center",
            marginTop: 12,
          }}
        >
          {LABEL_COLORS.map(({ key, bg }) => (
            <span
              key={key}
              onClick={() => {
                if (!meta) return;
                handlers.onSetLabel(activeLabel === key ? null : key);
              }}
              style={{
                width: 9,
                height: 9,
                borderRadius: "50%",
                background: bg,
                boxShadow:
                  activeLabel === key
                    ? "0 0 0 2px var(--color-accent-line)"
                    : "0 0 0 2px rgba(0,0,0,.3)",
                cursor: meta ? "pointer" : "default",
                display: "block",
              }}
            />
          ))}
        </div>
      </div>

      {/* Histogram */}
      <div
        style={{
          padding: "14px 16px",
          borderTop: "1px solid var(--color-line)",
        }}
      >
        <div
          style={{
            fontSize: 10.5,
            letterSpacing: ".06em",
            textTransform: "uppercase",
            color: "var(--color-t3)",
            fontWeight: 600,
            marginBottom: 10,
          }}
        >
          Histogram
        </div>
        <div
          style={{
            height: 64,
            borderRadius: "var(--radius-sm)",
            background: "var(--color-stage)",
            outline: "1px solid var(--color-line)",
            overflow: "hidden",
          }}
        >
          <HistogramSvg />
        </div>
      </div>

      {/* Metadata */}
      <div
        style={{
          padding: "14px 16px",
          borderTop: "1px solid var(--color-line)",
        }}
      >
        <div
          style={{
            fontSize: 10.5,
            letterSpacing: ".06em",
            textTransform: "uppercase",
            color: "var(--color-t3)",
            fontWeight: 600,
            marginBottom: 10,
          }}
        >
          Metadata
        </div>
        {meta === null ? (
          <div style={{ fontSize: 12, color: "var(--color-t3)" }}>
            No image selected
          </div>
        ) : (
          <dl
            style={{
              display: "grid",
              gridTemplateColumns: "auto 1fr",
              gap: "7px 14px",
              fontSize: 12,
            }}
          >
            {metaRows.map(({ label, value }) => (
              <>
                <dt key={`dt-${label}`} style={{ color: "var(--color-t3)" }}>
                  {label}
                </dt>
                <dd
                  key={`dd-${label}`}
                  style={{
                    color: "var(--color-t1)",
                    fontFamily: "var(--font-mono)",
                    fontSize: 11.5,
                    textAlign: "right",
                  }}
                >
                  {value}
                </dd>
              </>
            ))}
          </dl>
        )}
      </div>

      {/* Keywords */}
      <div
        style={{
          padding: "14px 16px",
          borderTop: "1px solid var(--color-line)",
        }}
      >
        <div
          style={{
            fontSize: 10.5,
            letterSpacing: ".06em",
            textTransform: "uppercase",
            color: "var(--color-t3)",
            fontWeight: 600,
            marginBottom: 10,
          }}
        >
          Keywords
        </div>
        <div style={{ display: "flex", flexWrap: "wrap", gap: 6 }}>
          {["coast", "golden hour", "travel"].map((kw) => (
            <span
              key={kw}
              style={{
                fontSize: 11.5,
                color: "var(--color-t2)",
                background: "var(--color-elev)",
                border: "1px solid var(--color-line)",
                borderRadius: 20,
                padding: "3px 9px",
              }}
            >
              {kw}
            </span>
          ))}
          <span
            style={{
              fontSize: 11.5,
              color: "var(--color-t3)",
              background: "transparent",
              border: "1px dashed var(--color-line)",
              borderRadius: 20,
              padding: "3px 9px",
              cursor: "pointer",
            }}
          >
            + add
          </span>
        </div>
      </div>
    </aside>
  );
}
