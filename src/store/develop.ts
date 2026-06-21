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
  };
}

interface DevelopState {
  params: DevelopParams;
  setParam: (key: ScalarParamKey, value: number) => void;
  resetParams: () => void;
  imageUrl: string | null;
  setImageUrl: (url: string | null) => void;
  /** Instant embedded-JPEG preview shown until the processed render lands. */
  previewUrl: string | null;
  setPreviewUrl: (url: string | null) => void;
  rendering: boolean;
  setRendering: (b: boolean) => void;
  /** True when the stage is zoomed in past fit — render the full-resolution frame, not the preview. */
  fullRes: boolean;
  setFullRes: (b: boolean) => void;
  showBefore: boolean;
  setShowBefore: (b: boolean) => void;
  histogram: HistData | null;
  setHistogram: (h: HistData | null) => void;
  /** Index of the mask being edited (into params.masks), or null. */
  selectedMaskIndex: number | null;
  setSelectedMaskIndex: (i: number | null) => void;
  /** Index of the active component within the selected mask (for overlay + param editing). */
  selectedComponentIndex: number;
  setSelectedComponentIndex: (i: number) => void;
  /** Whether the mask coverage/handle overlay is shown on the stage. */
  maskOverlayVisible: boolean;
  setMaskOverlayVisible: (b: boolean) => void;
  /** Current brush settings used when painting new strokes. */
  brush: BrushSettings;
  setBrush: (patch: Partial<BrushSettings>) => void;
  /** Eyedropper armed for the color-range mask (next image click samples a target color). */
  pickingColor: boolean;
  setPickingColor: (b: boolean) => void;
  /** Crop tool active: the stage shows the full (uncropped) image + a draggable crop rectangle. */
  cropMode: boolean;
  setCropMode: (b: boolean) => void;
  /** Source image aspect (W/H), set by the Stage on load — used by the crop aspect presets. */
  imageAspect: number;
  setImageAspect: (a: number) => void;
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
  imageUrl: null,
  setImageUrl: (url) => set({ imageUrl: url }),
  previewUrl: null,
  setPreviewUrl: (url) => set({ previewUrl: url }),
  rendering: false,
  setRendering: (b) => set({ rendering: b }),
  fullRes: false,
  setFullRes: (b) => set({ fullRes: b }),
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
}));
