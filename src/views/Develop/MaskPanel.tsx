import Module from "./Module";
import Slider from "./Slider";
import { useDevelopStore } from "../../store/develop";
import {
  maskKindLabel,
  newBrushMask,
  newColorMask,
  newLinearMask,
  newLuminanceMask,
  newRadialMask,
} from "../../lib/maskGeom";
import {
  MASK_CAP,
  type LocalAdjust,
  type Mask,
  type MaskComponent,
  type MaskOp,
} from "../../lib/ipc";

interface MaskPanelProps {
  masks: Mask[];
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

// (label, key) pairs for the per-mask local adjustment sliders.
const ADJUST_SLIDERS: {
  label: string;
  key: keyof LocalAdjust;
  exposure?: boolean;
}[] = [
  { label: "Exposure", key: "exposure", exposure: true },
  { label: "Contrast", key: "contrast" },
  { label: "Highlights", key: "highlights" },
  { label: "Shadows", key: "shadows" },
  { label: "Whites", key: "whites" },
  { label: "Blacks", key: "blacks" },
  { label: "Saturation", key: "saturation" },
  { label: "Temp", key: "temp" },
  { label: "Tint", key: "tint" },
];

export default function MaskPanel({
  masks,
  onAddMask,
  onDeleteMask,
  onUpdateMask,
  onUpdateMaskAdjust,
  onAddComponent,
  onUpdateComponent,
  onDeleteComponent,
}: MaskPanelProps) {
  const selected = useDevelopStore((s) => s.selectedMaskIndex);
  const setSelected = useDevelopStore((s) => s.setSelectedMaskIndex);
  const overlayVisible = useDevelopStore((s) => s.maskOverlayVisible);
  const setOverlayVisible = useDevelopStore((s) => s.setMaskOverlayVisible);
  const brush = useDevelopStore((s) => s.brush);
  const setBrush = useDevelopStore((s) => s.setBrush);
  const pickingColor = useDevelopStore((s) => s.pickingColor);
  const setPickingColor = useDevelopStore((s) => s.setPickingColor);
  const activeComp = useDevelopStore((s) => s.selectedComponentIndex);
  const setActiveComp = useDevelopStore((s) => s.setSelectedComponentIndex);

  const atCap = masks.length >= MASK_CAP;
  const mask = selected !== null ? masks[selected] : undefined;
  const ci = mask ? Math.min(activeComp, mask.components.length - 1) : 0;
  const comp: MaskComponent | undefined = mask?.components[ci];
  const isBrush = comp?.kind.type === "brush";

  // Patch the active component's top-level fields (op/invert/feather).
  const setComp0 = (patch: Partial<MaskComponent>) => {
    if (selected === null) return;
    onUpdateComponent(selected, ci, patch);
  };
  // Patch the active component's kind parameters (preserving its variant `type`).
  const setComp0Kind = (patch: Record<string, number | string>) => {
    if (selected === null || !comp) return;
    onUpdateComponent(selected, ci, {
      kind: { ...comp.kind, ...patch } as MaskComponent["kind"],
    });
  };

  return (
    <Module title="Masks">
      <div
        style={{
          display: "flex",
          flexWrap: "wrap",
          gap: 6,
          marginBottom: 10,
        }}
      >
        <AddBtn
          label="+ Linear"
          disabled={atCap}
          onClick={() => onAddMask(newLinearMask())}
        />
        <AddBtn
          label="+ Radial"
          disabled={atCap}
          onClick={() => onAddMask(newRadialMask())}
        />
        <AddBtn
          label="+ Brush"
          disabled={atCap}
          onClick={() => onAddMask(newBrushMask())}
        />
        <AddBtn
          label="+ Luma"
          disabled={atCap}
          onClick={() => onAddMask(newLuminanceMask())}
        />
        <AddBtn
          label="+ Color"
          disabled={atCap}
          onClick={() => onAddMask(newColorMask())}
        />
      </div>

      {masks.length === 0 && (
        <p
          style={{
            fontSize: 11.5,
            color: "var(--color-t3)",
            margin: "4px 0 8px",
          }}
        >
          Add a mask to make a local adjustment.
        </p>
      )}

      {/* Mask list */}
      <div
        style={{
          display: "flex",
          flexDirection: "column",
          gap: 4,
          marginBottom: 8,
        }}
      >
        {masks.map((m, i) => {
          const isSel = i === selected;
          return (
            <div
              key={i}
              onClick={() => setSelected(isSel ? null : i)}
              style={{
                display: "flex",
                alignItems: "center",
                gap: 8,
                padding: "6px 8px",
                borderRadius: "var(--radius-sm)",
                cursor: "pointer",
                background: isSel ? "var(--color-hover)" : "transparent",
                border: isSel
                  ? "1px solid var(--color-accent)"
                  : "1px solid transparent",
              }}
            >
              <input
                type="checkbox"
                checked={m.enabled}
                onClick={(e) => e.stopPropagation()}
                onChange={(e) => onUpdateMask(i, { enabled: e.target.checked })}
              />
              <span style={{ flex: 1, fontSize: 12, color: "var(--color-t1)" }}>
                {maskKindLabel(m)} {i + 1}
              </span>
              <button
                onClick={(e) => {
                  e.stopPropagation();
                  onDeleteMask(i);
                }}
                style={{
                  background: "none",
                  border: "none",
                  color: "var(--color-t3)",
                  cursor: "pointer",
                  fontSize: 13,
                  padding: 0,
                }}
                title="Delete mask"
              >
                ✕
              </button>
            </div>
          );
        })}
      </div>

      {/* Selected mask editor */}
      {mask && selected !== null && comp && (
        <div
          style={{ borderTop: "1px solid var(--color-line)", paddingTop: 8 }}
        >
          <div
            style={{
              display: "flex",
              alignItems: "center",
              justifyContent: "space-between",
              marginBottom: 8,
            }}
          >
            <label
              style={{
                fontSize: 11.5,
                color: "var(--color-t2)",
                display: "flex",
                alignItems: "center",
                gap: 6,
              }}
            >
              <input
                type="checkbox"
                checked={overlayVisible}
                onChange={(e) => setOverlayVisible(e.target.checked)}
              />
              Show overlay
            </label>
            <label
              style={{
                fontSize: 11.5,
                color: "var(--color-t2)",
                display: "flex",
                alignItems: "center",
                gap: 6,
              }}
            >
              <input
                type="checkbox"
                checked={comp.invert}
                onChange={(e) => setComp0({ invert: e.target.checked })}
              />
              Invert
            </label>
          </div>

          <label
            style={{
              fontSize: 11.5,
              color: "var(--color-t2)",
              display: "flex",
              alignItems: "center",
              gap: 6,
              marginBottom: 8,
            }}
          >
            <input
              type="checkbox"
              checked={comp.feather}
              onChange={(e) => setComp0({ feather: e.target.checked })}
            />
            Refine edges (edge-aware feather)
          </label>

          {/* Components (combine with Add / Subtract / Intersect) */}
          <div style={{ marginBottom: 10 }}>
            {mask.components.map((c, idx) => (
              <div
                key={idx}
                onClick={() => setActiveComp(idx)}
                style={{
                  display: "flex",
                  alignItems: "center",
                  gap: 6,
                  padding: "4px 6px",
                  borderRadius: "var(--radius-sm)",
                  cursor: "pointer",
                  background: idx === ci ? "var(--color-hover)" : "transparent",
                  border:
                    idx === ci
                      ? "1px solid var(--color-accent)"
                      : "1px solid transparent",
                }}
              >
                <span
                  style={{ flex: 1, fontSize: 11.5, color: "var(--color-t1)" }}
                >
                  {compLabel(c)}
                  {c.invert ? " (inv)" : ""}
                </span>
                {idx === 0 ? (
                  <span style={{ fontSize: 10.5, color: "var(--color-t3)" }}>
                    base
                  </span>
                ) : (
                  <select
                    value={c.op}
                    onClick={(e) => e.stopPropagation()}
                    onChange={(e) =>
                      onUpdateComponent(selected, idx, {
                        op: e.target.value as MaskOp,
                      })
                    }
                    style={{
                      fontSize: 10.5,
                      background: "var(--color-app)",
                      color: "var(--color-t1)",
                      border: "1px solid var(--color-line)",
                      borderRadius: 4,
                    }}
                  >
                    <option value="add">Add</option>
                    <option value="subtract">Subtract</option>
                    <option value="intersect">Intersect</option>
                  </select>
                )}
                {mask.components.length > 1 && (
                  <button
                    onClick={(e) => {
                      e.stopPropagation();
                      onDeleteComponent(selected, idx);
                    }}
                    style={{
                      background: "none",
                      border: "none",
                      color: "var(--color-t3)",
                      cursor: "pointer",
                      fontSize: 12,
                      padding: 0,
                    }}
                    title="Remove component"
                  >
                    ✕
                  </button>
                )}
              </div>
            ))}
            <div
              style={{
                display: "flex",
                flexWrap: "wrap",
                gap: 4,
                marginTop: 6,
              }}
            >
              <CompBtn
                label="+Lin"
                onClick={() =>
                  onAddComponent(selected, newLinearMask().components[0])
                }
              />
              <CompBtn
                label="+Rad"
                onClick={() =>
                  onAddComponent(selected, newRadialMask().components[0])
                }
              />
              <CompBtn
                label="+Brush"
                onClick={() =>
                  onAddComponent(selected, newBrushMask().components[0])
                }
              />
              <CompBtn
                label="+Luma"
                onClick={() =>
                  onAddComponent(selected, newLuminanceMask().components[0])
                }
              />
              <CompBtn
                label="+Color"
                onClick={() =>
                  onAddComponent(selected, newColorMask().components[0])
                }
              />
            </div>
          </div>

          {comp.kind.type === "luminanceRange" && (
            <div style={{ marginBottom: 8 }}>
              <Slider
                label="Range low"
                min={0}
                max={100}
                defaultValue={40}
                value={Math.round(comp.kind.lo * 100)}
                onChange={(v) => setComp0Kind({ lo: v / 100 })}
              />
              <Slider
                label="Range high"
                min={0}
                max={100}
                defaultValue={100}
                value={Math.round(comp.kind.hi * 100)}
                onChange={(v) => setComp0Kind({ hi: v / 100 })}
              />
              <Slider
                label="Range feather"
                min={0}
                max={50}
                defaultValue={8}
                value={Math.round(comp.kind.feather * 100)}
                onChange={(v) => setComp0Kind({ feather: v / 100 })}
              />
              <div style={{ height: 6 }} />
            </div>
          )}

          {comp.kind.type === "colorRange" && (
            <div style={{ marginBottom: 8 }}>
              <button
                onClick={() => setPickingColor(!pickingColor)}
                style={{
                  width: "100%",
                  padding: "6px 0",
                  marginBottom: 8,
                  fontSize: 12,
                  borderRadius: "var(--radius-sm)",
                  border: pickingColor
                    ? "1px solid var(--color-accent)"
                    : "1px solid var(--color-line)",
                  background: pickingColor
                    ? "var(--color-accent)"
                    : "var(--color-hover)",
                  color: pickingColor ? "#fff" : "var(--color-t1)",
                  cursor: "pointer",
                }}
              >
                {pickingColor ? "Click image to sample…" : "⊹ Pick color"}
              </button>
              <Slider
                label="Tolerance"
                min={1}
                max={50}
                defaultValue={8}
                value={Math.round(comp.kind.tol * 100)}
                onChange={(v) => setComp0Kind({ tol: v / 100 })}
              />
              <Slider
                label="Range feather"
                min={0}
                max={50}
                defaultValue={6}
                value={Math.round(comp.kind.feather * 100)}
                onChange={(v) => setComp0Kind({ feather: v / 100 })}
              />
              <div style={{ height: 6 }} />
            </div>
          )}

          {isBrush && (
            <div style={{ marginBottom: 8 }}>
              <p
                style={{
                  fontSize: 11,
                  color: "var(--color-t3)",
                  margin: "0 0 6px",
                }}
              >
                Paint on the image. Drag to add strokes.
              </p>
              <label
                style={{
                  fontSize: 11.5,
                  color: "var(--color-t2)",
                  display: "flex",
                  alignItems: "center",
                  gap: 6,
                  marginBottom: 6,
                }}
              >
                <input
                  type="checkbox"
                  checked={brush.isErase}
                  onChange={(e) => setBrush({ isErase: e.target.checked })}
                />
                Erase
              </label>
              <Slider
                label="Brush size"
                min={1}
                max={50}
                defaultValue={8}
                value={Math.round(brush.size * 100)}
                onChange={(v) => setBrush({ size: v / 100 })}
              />
              <Slider
                label="Hardness"
                min={0}
                max={100}
                defaultValue={50}
                value={Math.round(brush.hardness * 100)}
                onChange={(v) => setBrush({ hardness: v / 100 })}
              />
              <Slider
                label="Strength"
                min={0}
                max={100}
                defaultValue={100}
                value={Math.round(brush.opacity * 100)}
                onChange={(v) => setBrush({ opacity: v / 100 })}
              />
              <div style={{ height: 6 }} />
            </div>
          )}

          <Slider
            label="Mask opacity"
            min={0}
            max={100}
            defaultValue={100}
            value={Math.round(mask.opacity * 100)}
            onChange={(v) => onUpdateMask(selected, { opacity: v / 100 })}
          />

          <div style={{ height: 6 }} />

          {ADJUST_SLIDERS.map(({ label, key, exposure }) => (
            <Slider
              key={key}
              label={label}
              min={exposure ? -5 : -100}
              max={exposure ? 5 : 100}
              defaultValue={0}
              bipolar
              decimals={exposure ? 2 : 0}
              value={mask.adjust[key]}
              onChange={(v) => onUpdateMaskAdjust(selected, { [key]: v })}
            />
          ))}
        </div>
      )}
    </Module>
  );
}

function compLabel(c: MaskComponent): string {
  switch (c.kind.type) {
    case "linear":
      return "Linear";
    case "radial":
      return "Radial";
    case "brush":
      return "Brush";
    case "luminanceRange":
      return "Luminance";
    case "colorRange":
      return "Color";
    case "ai":
      return "AI";
    default:
      return "Component";
  }
}

function CompBtn({ label, onClick }: { label: string; onClick: () => void }) {
  return (
    <button
      onClick={onClick}
      style={{
        flex: "1 1 auto",
        padding: "4px 6px",
        fontSize: 10.5,
        borderRadius: "var(--radius-sm)",
        border: "1px solid var(--color-line)",
        background: "var(--color-app)",
        color: "var(--color-t2)",
        cursor: "pointer",
        whiteSpace: "nowrap",
      }}
    >
      {label}
    </button>
  );
}

function AddBtn({
  label,
  onClick,
  disabled,
}: {
  label: string;
  onClick: () => void;
  disabled?: boolean;
}) {
  return (
    <button
      onClick={onClick}
      disabled={disabled}
      style={{
        flex: "1 1 auto",
        padding: "6px 8px",
        fontSize: 11.5,
        borderRadius: "var(--radius-sm)",
        border: "1px solid var(--color-line)",
        background: "var(--color-hover)",
        color: disabled ? "var(--color-t3)" : "var(--color-t1)",
        cursor: disabled ? "default" : "pointer",
        opacity: disabled ? 0.5 : 1,
        whiteSpace: "nowrap",
      }}
    >
      {label}
    </button>
  );
}
