import Slider from "./Slider";
import type { CbRgb, Rgb3 } from "../../lib/ipc";

/** Color-balance-RGB grading: 4 tonal zones (global/shadows/midtones/highlights), each an R/G/B
 * grading offset, plus scene-linear contrast + saturation. Mirrors `CbRgb` (Rust @binding(14)).
 *
 * UI is in the app's −100..100 convention; we map RGB to the grading ±0.5 range and contrast/
 * saturation to ±1. Drives `onChange` with the full updated field on every move. */

const RGB_RANGE = 0.5; // grading offset reached at UI ±100

type Zone = "global" | "shadows" | "midtones" | "highlights";
const ZONES: { key: Zone; label: string }[] = [
  { key: "global", label: "Global" },
  { key: "shadows", label: "Shadows" },
  { key: "midtones", label: "Midtones" },
  { key: "highlights", label: "Highlights" },
];
const CHANNELS: { i: number; label: string }[] = [
  { i: 0, label: "Red" },
  { i: 1, label: "Green" },
  { i: 2, label: "Blue" },
];

interface Props {
  value: CbRgb;
  onChange: (patch: Partial<CbRgb>) => void;
}

export default function ColorBalance({ value, onChange }: Props) {
  const setChannel = (zone: Zone, ch: number, ui: number) => {
    const arr = [...value[zone]] as Rgb3;
    arr[ch] = (ui / 100) * RGB_RANGE;
    onChange({ [zone]: arr });
  };

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 14 }}>
      {ZONES.map(({ key, label }) => (
        <div
          key={key}
          style={{ display: "flex", flexDirection: "column", gap: 4 }}
        >
          <span
            style={{
              fontSize: 11,
              textTransform: "uppercase",
              letterSpacing: 0.5,
              color: "var(--color-t2)",
            }}
          >
            {label}
          </span>
          {CHANNELS.map(({ i, label: cl }) => (
            <Slider
              key={cl}
              label={cl}
              min={-100}
              max={100}
              defaultValue={0}
              bipolar
              value={(value[key][i] / RGB_RANGE) * 100}
              onChange={(v) => setChannel(key, i, v)}
            />
          ))}
        </div>
      ))}

      <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
        <span
          style={{
            fontSize: 11,
            textTransform: "uppercase",
            letterSpacing: 0.5,
            color: "var(--color-t2)",
          }}
        >
          Master
        </span>
        <Slider
          label="Contrast"
          min={-100}
          max={100}
          defaultValue={0}
          bipolar
          value={value.contrast * 100}
          onChange={(v) => onChange({ contrast: v / 100 })}
        />
        <Slider
          label="Saturation"
          min={-100}
          max={100}
          defaultValue={0}
          bipolar
          value={value.saturation * 100}
          onChange={(v) => onChange({ saturation: v / 100 })}
        />
      </div>
    </div>
  );
}
