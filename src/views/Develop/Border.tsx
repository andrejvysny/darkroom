import Slider from "./Slider";
import type { Border as BorderData, Rgb3 } from "../../lib/ipc";

/** Border / frame controls: a color (White / Black / Custom), a size (% of the long edge), and an
 * optional target aspect ratio the photo is padded into. Mirrors Rust `Border`. Drives `onChange`
 * with the changed field(s) on every edit. */

interface Props {
  value: BorderData;
  onChange: (patch: Partial<BorderData>) => void;
}

const ASPECTS: { label: string; w: number; h: number }[] = [
  { label: "Original", w: 0, h: 0 },
  { label: "1:1 Square", w: 1, h: 1 },
  { label: "4:5 Portrait", w: 4, h: 5 },
  { label: "5:4 Landscape", w: 5, h: 4 },
  { label: "3:2", w: 3, h: 2 },
  { label: "2:3", w: 2, h: 3 },
  { label: "16:9", w: 16, h: 9 },
  { label: "9:16", w: 9, h: 16 },
];

function rgb3ToHex(c: Rgb3): string {
  const q = (v: number) =>
    Math.round(Math.min(1, Math.max(0, v)) * 255)
      .toString(16)
      .padStart(2, "0");
  return `#${q(c[0])}${q(c[1])}${q(c[2])}`;
}
function hexToRgb3(hex: string): Rgb3 {
  const n = hex.replace("#", "");
  const r = parseInt(n.slice(0, 2), 16) / 255;
  const g = parseInt(n.slice(2, 4), 16) / 255;
  const b = parseInt(n.slice(4, 6), 16) / 255;
  return [r || 0, g || 0, b || 0];
}
const isColor = (c: Rgb3, r: number, g: number, b: number) =>
  c[0] === r && c[1] === g && c[2] === b;

export default function Border({ value, onChange }: Props) {
  const aspectKey = `${value.aspectW}:${value.aspectH}`;
  const white = isColor(value.color, 1, 1, 1);
  const black = isColor(value.color, 0, 0, 0);

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 12 }}>
      {/* Color */}
      <div style={{ display: "flex", flexDirection: "column", gap: 5 }}>
        <span style={label}>Color</span>
        <div style={{ display: "flex", gap: 6, alignItems: "center" }}>
          <Swatch
            active={white}
            fill="#ffffff"
            onClick={() => onChange({ color: [1, 1, 1] })}
          />
          <Swatch
            active={black}
            fill="#000000"
            onClick={() => onChange({ color: [0, 0, 0] })}
          />
          <label
            title="Custom color"
            style={{
              display: "flex",
              alignItems: "center",
              gap: 6,
              fontSize: 11,
              color: "var(--color-t2)",
              cursor: "pointer",
            }}
          >
            <input
              type="color"
              value={rgb3ToHex(value.color)}
              onChange={(e) => onChange({ color: hexToRgb3(e.target.value) })}
              style={{
                width: 26,
                height: 22,
                padding: 0,
                border: "1px solid var(--color-line-2)",
                borderRadius: "var(--radius-sm)",
                background: "transparent",
                cursor: "pointer",
              }}
            />
            Custom
          </label>
        </div>
      </div>

      {/* Size */}
      <Slider
        label="Size"
        min={0}
        max={25}
        defaultValue={0}
        decimals={1}
        value={value.size}
        onChange={(v) => onChange({ size: v })}
      />

      {/* Aspect ratio */}
      <div style={{ display: "flex", flexDirection: "column", gap: 5 }}>
        <span style={label}>Aspect ratio</span>
        <select
          value={aspectKey}
          onChange={(e) => {
            const a = ASPECTS.find((x) => `${x.w}:${x.h}` === e.target.value);
            if (a) onChange({ aspectW: a.w, aspectH: a.h });
          }}
          style={{
            width: "100%",
            background: "var(--color-elev)",
            border: "1px solid var(--color-line-2)",
            borderRadius: "var(--radius-sm)",
            padding: "6px 8px",
            fontSize: 12,
            color: "var(--color-t1)",
            outline: "none",
          }}
        >
          {ASPECTS.map((a) => (
            <option key={a.label} value={`${a.w}:${a.h}`}>
              {a.label}
            </option>
          ))}
        </select>
      </div>
    </div>
  );
}

const label: React.CSSProperties = {
  fontSize: 11,
  textTransform: "uppercase",
  letterSpacing: 0.5,
  color: "var(--color-t2)",
};

function Swatch({
  active,
  fill,
  onClick,
}: {
  active: boolean;
  fill: string;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      style={{
        width: 26,
        height: 22,
        borderRadius: "var(--radius-sm)",
        background: fill,
        border: active
          ? "2px solid var(--color-accent)"
          : "1px solid var(--color-line-2)",
        cursor: "pointer",
      }}
    />
  );
}
