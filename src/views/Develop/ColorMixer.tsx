import { useState } from "react";
import Slider from "./Slider";
import type { HslBand } from "../../lib/ipc";

// Swatch colors for the 8 hue bands (must align with the order of `HUE_CENTERS` in develop.wgsl:
// red, orange, yellow, green, aqua, blue, purple, magenta).
const HUES = [
  "#c8534f",
  "#cd8b4a",
  "#cdc04a",
  "#7bb24f",
  "#4fb39a",
  "#4f86c8",
  "#7d5fc0",
  "#c25fa6",
];

interface ColorMixerProps {
  saturation: number;
  onSaturationChange: (v: number) => void;
  bands: HslBand[];
  onBandChange: (index: number, patch: Partial<HslBand>) => void;
}

export default function ColorMixer({
  saturation,
  onSaturationChange,
  bands,
  onBandChange,
}: ColorMixerProps) {
  const [activeHue, setActiveHue] = useState(0);
  const band = bands[activeHue] ?? { h: 0, s: 0, l: 0 };

  return (
    <>
      <Slider
        label="Saturation"
        min={-100}
        max={100}
        defaultValue={0}
        bipolar
        value={saturation}
        onChange={onSaturationChange}
      />
      <div style={{ display: "flex", gap: 6, marginTop: 4 }}>
        {HUES.map((color, i) => (
          <button
            key={color}
            onClick={() => setActiveHue(i)}
            style={{
              width: 22,
              height: 22,
              borderRadius: "50%",
              background: color,
              border:
                activeHue === i
                  ? "2px solid var(--color-t1)"
                  : "2px solid transparent",
              boxShadow:
                activeHue === i ? "0 0 0 1px var(--color-app)" : "none",
              cursor: "pointer",
              padding: 0,
            }}
          />
        ))}
      </div>
      <Slider
        label="Hue"
        min={-100}
        max={100}
        defaultValue={0}
        bipolar
        value={band.h}
        onChange={(v) => onBandChange(activeHue, { h: v })}
      />
      <Slider
        label="Saturation"
        min={-100}
        max={100}
        defaultValue={0}
        bipolar
        value={band.s}
        onChange={(v) => onBandChange(activeHue, { s: v })}
      />
      <Slider
        label="Luminance"
        min={-100}
        max={100}
        defaultValue={0}
        bipolar
        value={band.l}
        onChange={(v) => onBandChange(activeHue, { l: v })}
      />
    </>
  );
}
