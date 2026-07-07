import Slider from "./Slider";
import type { ChannelMix, Rgb3 } from "../../lib/ipc";
import { DEFAULT_CHANNEL_MIX } from "../../lib/ipc";

/** Channel mixer (Photoshop/GIMP-style): each output channel (Red/Green/Blue) is a weighted sum of
 * the source R/G/B. Mirrors Rust `ChannelMix` (@binding(15), applied on display sRGB). A pure swap is
 * a permutation of the identity rows — exposed as one-click preset buttons.
 *
 * UI is in percent (100 = 1.0 weight); the stored value is the fraction. Drives `onChange` with the
 * full updated row on every move. */

const RANGE = 200; // ±200% source weight, matching Photoshop's channel mixer

type OutKey = "red" | "green" | "blue";
const OUTPUTS: { key: OutKey; label: string }[] = [
  { key: "red", label: "Red" },
  { key: "green", label: "Green" },
  { key: "blue", label: "Blue" },
];
const SOURCES = ["R", "G", "B"];

interface Props {
  value: ChannelMix;
  onChange: (patch: Partial<ChannelMix>) => void;
}

const swap = (a: Rgb3, b: Rgb3): [Rgb3, Rgb3] => [
  [...b] as Rgb3,
  [...a] as Rgb3,
];

export default function ChannelMixer({ value, onChange }: Props) {
  const setWeight = (out: OutKey, src: number, ui: number) => {
    const row = [...value[out]] as Rgb3;
    row[src] = ui / 100;
    onChange({ [out]: row });
  };

  const swapGB = () => {
    const [green, blue] = swap(value.green, value.blue);
    onChange({ green, blue });
  };
  const swapRG = () => {
    const [red, green] = swap(value.red, value.green);
    onChange({ red, green });
  };
  const swapRB = () => {
    const [red, blue] = swap(value.red, value.blue);
    onChange({ red, blue });
  };

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 14 }}>
      <div style={{ display: "flex", flexWrap: "wrap", gap: 6 }}>
        <PresetButton label="Swap G / B" onClick={swapGB} />
        <PresetButton label="Swap R / G" onClick={swapRG} />
        <PresetButton label="Swap R / B" onClick={swapRB} />
        <PresetButton
          label="Reset"
          onClick={() => onChange({ ...DEFAULT_CHANNEL_MIX })}
        />
      </div>

      {OUTPUTS.map(({ key, label }) => (
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
            {label} output
          </span>
          {SOURCES.map((sl, i) => (
            <Slider
              key={sl}
              label={sl}
              min={-RANGE}
              max={RANGE}
              defaultValue={i === OUTPUTS.findIndex((o) => o.key === key) ? 100 : 0}
              bipolar
              value={value[key][i] * 100}
              onChange={(v) => setWeight(key, i, v)}
            />
          ))}
        </div>
      ))}
    </div>
  );
}

function PresetButton({
  label,
  onClick,
}: {
  label: string;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      style={{
        fontSize: 11,
        padding: "4px 8px",
        borderRadius: "var(--radius-sm)",
        border: "1px solid var(--color-line-2)",
        background: "var(--color-elev)",
        color: "var(--color-t1)",
        cursor: "pointer",
      }}
    >
      {label}
    </button>
  );
}
