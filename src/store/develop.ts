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
  showBefore: boolean;
  setShowBefore: (b: boolean) => void;
  histogram: HistData | null;
  setHistogram: (h: HistData | null) => void;
}

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
  showBefore: false,
  setShowBefore: (b) => set({ showBefore: b }),
  histogram: null,
  setHistogram: (h) => set({ histogram: h }),
}));
