import { useState } from "react";
import Histogram from "./Histogram";
import Module from "./Module";
import Slider from "./Slider";
import ToneCurve from "./ToneCurve";
import ColorMixer from "./ColorMixer";
import MaskPanel from "./MaskPanel";
import Icon from "../../components/Icon";
import type {
  DevelopParams,
  LocalAdjust,
  Mask,
  MaskComponent,
  ScalarParamKey,
  ToneCurveChannel,
  CurvePoint,
  HslBand,
} from "../../lib/ipc";

type AspectRatio = "3:2" | "16:9" | "1:1" | "4:5" | "Free";
const ASPECTS: AspectRatio[] = ["3:2", "16:9", "1:1", "4:5", "Free"];

interface InstrumentPanelProps {
  params: DevelopParams;
  onParamChange: (key: ScalarParamKey, value: number) => void;
  onCurveChange: (channel: ToneCurveChannel, points: CurvePoint[]) => void;
  onHslChange: (index: number, patch: Partial<HslBand>) => void;
  resetKeys: (keys: ScalarParamKey[]) => void;
  onReset: () => void;
  onAddMask: (mask: Mask) => void;
  onDeleteMask: (index: number) => void;
  onUpdateMask: (index: number, patch: Partial<Mask>) => void;
  onUpdateMaskAdjust: (index: number, patch: Partial<LocalAdjust>) => void;
  onAddComponent: (index: number, component: MaskComponent) => void;
  onUpdateComponent: (
    index: number,
    compIndex: number,
    patch: Partial<MaskComponent>,
  ) => void;
  onDeleteComponent: (index: number, compIndex: number) => void;
}

export default function InstrumentPanel({
  params,
  onParamChange,
  onCurveChange,
  onHslChange,
  resetKeys,
  onReset,
  onAddMask,
  onDeleteMask,
  onUpdateMask,
  onUpdateMaskAdjust,
  onAddComponent,
  onUpdateComponent,
  onDeleteComponent,
}: InstrumentPanelProps) {
  const [aspect, setAspect] = useState<AspectRatio>("3:2");

  return (
    <aside
      style={{
        flexShrink: 0,
        width: 304,
        background: "var(--color-app)",
        borderLeft: "1px solid var(--color-line)",
        overflowY: "auto",
        overflowX: "hidden",
        minHeight: 0,
      }}
    >
      <Histogram />

      {/* Masks */}
      <MaskPanel
        masks={params.masks}
        onAddMask={onAddMask}
        onDeleteMask={onDeleteMask}
        onUpdateMask={onUpdateMask}
        onUpdateMaskAdjust={onUpdateMaskAdjust}
        onAddComponent={onAddComponent}
        onUpdateComponent={onUpdateComponent}
        onDeleteComponent={onDeleteComponent}
      />

      {/* White Balance */}
      <Module title="White balance" onReset={() => resetKeys(["temp", "tint"])}>
        <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
          <span style={{ fontSize: 12, color: "var(--color-t2)", flex: 1 }}>
            As shot
          </span>
          <div
            style={{
              display: "flex",
              alignItems: "center",
              gap: 6,
              background: "var(--color-elev)",
              border: "1px solid var(--color-line)",
              borderRadius: "var(--radius-sm)",
              padding: "5px 9px",
              fontSize: 12,
              color: "var(--color-t1)",
              cursor: "pointer",
            }}
          >
            Custom <Icon name="chev" size={11} />
          </div>
          <button
            style={{
              width: 28,
              height: 28,
              border: "1px solid var(--color-line)",
              borderRadius: "var(--radius-sm)",
              display: "flex",
              alignItems: "center",
              justifyContent: "center",
              color: "var(--color-t2)",
              flexShrink: 0,
            }}
          >
            <Icon name="pick" />
          </button>
        </div>
        <Slider
          label="Temp"
          min={-100}
          max={100}
          defaultValue={0}
          bipolar
          value={params.temp}
          onChange={(v) => onParamChange("temp", v)}
        />
        <Slider
          label="Tint"
          min={-100}
          max={100}
          defaultValue={0}
          bipolar
          value={params.tint}
          onChange={(v) => onParamChange("tint", v)}
        />
      </Module>

      {/* Light */}
      <Module
        title="Light"
        onReset={() =>
          resetKeys([
            "exposure",
            "contrast",
            "highlights",
            "shadows",
            "blacks",
            "whites",
          ])
        }
      >
        <Slider
          label="Exposure"
          min={-5}
          max={5}
          defaultValue={0}
          bipolar
          decimals={2}
          value={params.exposure}
          onChange={(v) => onParamChange("exposure", v)}
        />
        <Slider
          label="Contrast"
          min={-100}
          max={100}
          defaultValue={0}
          bipolar
          value={params.contrast}
          onChange={(v) => onParamChange("contrast", v)}
        />
        <Slider
          label="Highlights"
          min={-100}
          max={100}
          defaultValue={0}
          bipolar
          value={params.highlights}
          onChange={(v) => onParamChange("highlights", v)}
        />
        <Slider
          label="Shadows"
          min={-100}
          max={100}
          defaultValue={0}
          bipolar
          value={params.shadows}
          onChange={(v) => onParamChange("shadows", v)}
        />
        <Slider
          label="Whites"
          min={-100}
          max={100}
          defaultValue={0}
          bipolar
          value={params.whites}
          onChange={(v) => onParamChange("whites", v)}
        />
        <Slider
          label="Blacks"
          min={-100}
          max={100}
          defaultValue={0}
          bipolar
          value={params.blacks}
          onChange={(v) => onParamChange("blacks", v)}
        />
      </Module>

      {/* Tone Curve */}
      <Module
        title="Tone curve"
        onReset={() => {
          onCurveChange("rgb", []);
          onCurveChange("r", []);
          onCurveChange("g", []);
          onCurveChange("b", []);
        }}
      >
        <ToneCurve curve={params.toneCurve} onChange={onCurveChange} />
      </Module>

      {/* Color Mixer */}
      <Module
        title="Color mixer"
        onReset={() => {
          onParamChange("saturation", 0);
          params.hsl.forEach((_, i) => onHslChange(i, { h: 0, s: 0, l: 0 }));
        }}
      >
        <ColorMixer
          saturation={params.saturation}
          onSaturationChange={(v) => onParamChange("saturation", v)}
          bands={params.hsl}
          onBandChange={onHslChange}
        />
      </Module>

      {/* Detail */}
      <Module
        title="Detail"
        defaultCollapsed
        onReset={() => resetKeys(["sharpen", "nrLuma", "nrColor"])}
      >
        <Slider
          label="Sharpening"
          min={0}
          max={150}
          defaultValue={0}
          value={params.sharpen}
          onChange={(v) => onParamChange("sharpen", v)}
        />
        <Slider
          label="Noise · luminance"
          min={0}
          max={100}
          defaultValue={0}
          value={params.nrLuma}
          onChange={(v) => onParamChange("nrLuma", v)}
        />
        <Slider
          label="Noise · color"
          min={0}
          max={100}
          defaultValue={0}
          value={params.nrColor}
          onChange={(v) => onParamChange("nrColor", v)}
        />
      </Module>

      {/* Lens Corrections */}
      <Module
        title="Lens corrections"
        defaultCollapsed
        onReset={() => resetKeys(["vignette"])}
      >
        <Slider
          label="Vignette"
          min={-100}
          max={100}
          defaultValue={0}
          bipolar
          value={params.vignette}
          onChange={(v) => onParamChange("vignette", v)}
        />
      </Module>

      {/* Crop & Geometry */}
      <Module title="Crop & geometry" defaultCollapsed>
        <div style={{ display: "flex", gap: 6, flexWrap: "wrap" }}>
          {ASPECTS.map((a) => (
            <button
              key={a}
              onClick={() => setAspect(a)}
              style={{
                padding: "4px 10px",
                border: "1px solid",
                borderRadius: "var(--radius-sm)",
                fontSize: 11.5,
                fontFamily: "var(--font-mono)",
                cursor: "pointer",
                color: aspect === a ? "var(--color-t1)" : "var(--color-t2)",
                borderColor:
                  aspect === a
                    ? "var(--color-accent-line)"
                    : "var(--color-line)",
                background:
                  aspect === a ? "var(--color-accent-dim)" : "transparent",
              }}
            >
              {a}
            </button>
          ))}
        </div>
        <Slider
          label="Angle"
          min={-45}
          max={45}
          defaultValue={0}
          bipolar
          suffix="°"
        />
      </Module>

      {/* Global reset at panel bottom */}
      <div
        style={{
          padding: "12px 14px",
          borderTop: "1px solid var(--color-line)",
        }}
      >
        <button
          onClick={onReset}
          style={{
            width: "100%",
            padding: "7px 0",
            border: "1px solid var(--color-line)",
            borderRadius: "var(--radius-sm)",
            fontSize: 12,
            color: "var(--color-t2)",
            cursor: "pointer",
          }}
        >
          Reset all
        </button>
      </div>
    </aside>
  );
}
