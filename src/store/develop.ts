import { create } from "zustand";
import {
  DEFAULT_PARAMS,
  type DevelopParams,
  type HistData,
  type ScalarParamKey,
} from "../lib/ipc";

/** Fresh deep clone of the defaults (so the hsl array / toneCurve aren't shared across images). */
export function freshDefaults(): DevelopParams {
  return {
    ...DEFAULT_PARAMS,
    toneCurve: { rgb: [], r: [], g: [], b: [] },
    hsl: DEFAULT_PARAMS.hsl.map((b) => ({ ...b })),
    crop: { ...DEFAULT_PARAMS.crop },
    masks: [],
    cbRgb: {
      global: [...DEFAULT_PARAMS.cbRgb.global],
      shadows: [...DEFAULT_PARAMS.cbRgb.shadows],
      midtones: [...DEFAULT_PARAMS.cbRgb.midtones],
      highlights: [...DEFAULT_PARAMS.cbRgb.highlights],
      contrast: DEFAULT_PARAMS.cbRgb.contrast,
      saturation: DEFAULT_PARAMS.cbRgb.saturation,
    },
  };
}

interface DevelopState {
  params: DevelopParams;
  setParam: (key: ScalarParamKey, value: number) => void;
  resetParams: () => void;
  /** Instant embedded-JPEG preview shown until the first canvas render lands. */
  previewUrl: string | null;
  setPreviewUrl: (url: string | null) => void;
  rendering: boolean;
  setRendering: (b: boolean) => void;
  showBefore: boolean;
  setShowBefore: (b: boolean) => void;
  histogram: HistData | null;
  setHistogram: (h: HistData | null) => void;
  /** Index of the mask being edited (into params.masks), or null. */
  selectedMaskIndex: number | null;
  setSelectedMaskIndex: (i: number | null) => void;
  /** Index of the active component within the selected mask. */
  selectedComponentIndex: number;
  setSelectedComponentIndex: (i: number) => void;
  /** Whether the mask coverage/handle overlay is shown on the stage. */
  maskOverlayVisible: boolean;
  setMaskOverlayVisible: (b: boolean) => void;
  /** Current brush settings used when painting new strokes. */
  brush: BrushSettings;
  setBrush: (patch: Partial<BrushSettings>) => void;
  /** Eyedropper armed for the color-range mask. */
  pickingColor: boolean;
  setPickingColor: (b: boolean) => void;
  /** Crop tool active: stage shows full (uncropped) image + draggable crop rect. */
  cropMode: boolean;
  setCropMode: (b: boolean) => void;
  /** Source image aspect (W/H), set by DevelopView from the ImageRow — used by crop aspect presets. */
  imageAspect: number;
  setImageAspect: (a: number) => void;
  /** Preset / paste apply strength, 0..100 (100 = full). Shared by the Presets panel + paste. */
  presetAmount: number;
  setPresetAmount: (n: number) => void;
  /** Copied develop settings (full source params + the field scope), for Paste settings. */
  copiedSettings: CopiedSettings | null;
  setCopiedSettings: (c: CopiedSettings | null) => void;
  /** In-memory session undo/redo of full param states (cleared on image change). */
  undoStack: DevelopParams[];
  redoStack: DevelopParams[];
  /** Record a pre-edit state for undo (clears the redo stack). */
  pushUndo: (prior: DevelopParams) => void;
  clearHistory: () => void;
}

/** Max session undo depth (full param snapshots, ~2–5 KB each). */
const UNDO_CAP = 60;

export interface CopiedSettings {
  params: DevelopParams;
  fieldKeys: string[];
}

export interface BrushSettings {
  size: number; // fraction of longest edge
  hardness: number; // 0..1
  flow: number; // 0..1
  opacity: number; // 0..1
  isErase: boolean;
}

const DEFAULT_BRUSH: BrushSettings = {
  size: 0.08,
  hardness: 0.5,
  flow: 1,
  opacity: 1,
  isErase: false,
};

export const useDevelopStore = create<DevelopState>((set) => ({
  params: freshDefaults(),
  setParam: (key, value) =>
    set((s) => ({ params: { ...s.params, [key]: value } })),
  resetParams: () => set({ params: freshDefaults() }),
  previewUrl: null,
  setPreviewUrl: (url) => set({ previewUrl: url }),
  rendering: false,
  setRendering: (b) => set({ rendering: b }),
  showBefore: false,
  setShowBefore: (b) => set({ showBefore: b }),
  histogram: null,
  setHistogram: (h) => set({ histogram: h }),
  selectedMaskIndex: null,
  setSelectedMaskIndex: (i) =>
    set({ selectedMaskIndex: i, selectedComponentIndex: 0 }),
  selectedComponentIndex: 0,
  setSelectedComponentIndex: (i) => set({ selectedComponentIndex: i }),
  maskOverlayVisible: true,
  setMaskOverlayVisible: (b) => set({ maskOverlayVisible: b }),
  brush: DEFAULT_BRUSH,
  setBrush: (patch) => set((s) => ({ brush: { ...s.brush, ...patch } })),
  pickingColor: false,
  setPickingColor: (b) => set({ pickingColor: b }),
  cropMode: false,
  setCropMode: (b) => set({ cropMode: b }),
  imageAspect: 1.5,
  setImageAspect: (a) => set({ imageAspect: a }),
  presetAmount: 100,
  setPresetAmount: (n) => set({ presetAmount: n }),
  copiedSettings: null,
  setCopiedSettings: (c) => set({ copiedSettings: c }),
  undoStack: [],
  redoStack: [],
  pushUndo: (prior) =>
    set((s) => ({
      undoStack: [...s.undoStack, prior].slice(-UNDO_CAP),
      redoStack: [],
    })),
  clearHistory: () => set({ undoStack: [], redoStack: [] }),
}));
