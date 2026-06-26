import Histogram from "./Histogram";
import Module from "./Module";
import Slider from "./Slider";
import ToneCurve from "./ToneCurve";
import ColorMixer from "./ColorMixer";
import ColorBalance from "./ColorBalance";
import MaskPanel from "./MaskPanel";
import Icon from "../../components/Icon";
import { useDevelopStore } from "../../store/develop";
import {
  DEFAULT_CROP,
  type DevelopParams,
  type LocalAdjust,
  type Mask,
  type MaskComponent,
  type ScalarParamKey,
  type ToneCurveChannel,
  type CurvePoint,
  type HslBand,
  type Crop,
  type CbRgb,
  DEFAULT_CB_RGB,
} from "../../lib/ipc";

interface InstrumentPanelProps {
  params: DevelopParams;
  onParamChange: (key: ScalarParamKey, value: number) => void;
  onCurveChange: (channel: ToneCurveChannel, points: CurvePoint[]) => void;
  onHslChange: (index: number, patch: Partial<HslBand>) => void;
  onCropChange: (patch: Partial<Crop>) => void;
  onColorBalanceChange: (patch: Partial<CbRgb>) => void;
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
  onCropChange,
  onColorBalanceChange,
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
  const cropMode = useDevelopStore((s) => s.cropMode);
  const setCropMode = useDevelopStore((s) => s.setCropMode);
  const imageAspect = useDevelopStore((s) => s.imageAspect);

  // Centered crop rect of a target pixel aspect ratio that fills the frame. `target<=0` ⇒ full frame.
  // imageAspect is 0 until the Stage measures the loaded image; in that brief window a ratio preset
  // only opens the tool (no wrong-aspect crop) — the user can re-click once the image is measured.
  const setAspect = (target: number) => {
    if (target <= 0) {
      onCropChange({ cx: 0.5, cy: 0.5, hw: 0.5, hh: 0.5 });
    } else if (imageAspect > 0) {
      const k = target / imageAspect; // hw/hh
      const hw = k >= 1 ? 0.5 : 0.5 * k;
      const hh = k >= 1 ? 0.5 / k : 0.5;
      onCropChange({ cx: 0.5, cy: 0.5, hw, hh });
    }
    setCropMode(true);
  };

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
          onParamChange("toneAmount", 100);
          onCurveChange("rgb", []);
          onCurveChange("r", []);
          onCurveChange("g", []);
          onCurveChange("b", []);
        }}
      >
        <Slider
          label="Base curve"
          min={0}
          max={100}
          defaultValue={100}
          value={params.toneAmount}
          onChange={(v) => onParamChange("toneAmount", v)}
        />
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

      {/* Color Balance (4-way grading) */}
      <Module
        title="Color balance"
        defaultCollapsed
        onReset={() => onColorBalanceChange({ ...DEFAULT_CB_RGB })}
      >
        <ColorBalance value={params.cbRgb} onChange={onColorBalanceChange} />
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

      {/* Crop & straighten */}
      <Module
        title="Crop & straighten"
        defaultCollapsed
        onReset={() => {
          onCropChange({ ...DEFAULT_CROP });
          setCropMode(false);
        }}
      >
        <button
          onClick={() => setCropMode(!cropMode)}
          style={{
            width: "100%",
            padding: "7px 0",
            marginBottom: 10,
            border: "1px solid var(--color-line)",
            borderRadius: "var(--radius-sm)",
            fontSize: 12,
            background: cropMode ? "var(--color-accent)" : "transparent",
            color: cropMode ? "#fff" : "var(--color-t2)",
            cursor: "pointer",
          }}
        >
          {cropMode ? "Done cropping" : "Adjust crop"}
        </button>
        <div
          style={{
            display: "grid",
            gridTemplateColumns: "repeat(4, 1fr)",
            gap: 6,
            marginBottom: 12,
          }}
        >
          {(
            [
              ["Full", 0],
              ["1:1", 1],
              ["4:5", 4 / 5],
              ["5:4", 5 / 4],
              ["3:2", 3 / 2],
              ["2:3", 2 / 3],
              ["4:3", 4 / 3],
              ["16:9", 16 / 9],
            ] as [string, number][]
          ).map(([label, ar]) => (
            <button
              key={label}
              onClick={() => setAspect(ar)}
              style={{
                padding: "5px 0",
                border: "1px solid var(--color-line)",
                borderRadius: "var(--radius-sm)",
                fontSize: 11,
                color: "var(--color-t2)",
                background: "transparent",
                cursor: "pointer",
              }}
            >
              {label}
            </button>
          ))}
        </div>
        <Slider
          label="Straighten"
          min={-45}
          max={45}
          defaultValue={0}
          bipolar
          decimals={1}
          value={params.crop.angle}
          onChange={(v) => onCropChange({ angle: v })}
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
          Reset to default
        </button>
      </div>
    </aside>
  );
}
