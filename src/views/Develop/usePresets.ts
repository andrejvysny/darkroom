import { useCallback, useEffect, useRef, useState } from "react";
import { open, save } from "@tauri-apps/plugin-dialog";
import { useAppStore } from "../../store/app";
import { useDevelopStore } from "../../store/develop";
import { ALL_FIELD_KEYS } from "../../lib/presetScope";
import {
  developApplySettings,
  presetsApply,
  presetsDelete,
  presetsDuplicate,
  presetsExport,
  presetsImportFile,
  presetsList,
  presetsSave,
  presetsUpdate,
  type DevelopParams,
  type ImportReport,
  type PresetSummary,
} from "../../lib/ipc";
import { log } from "../../lib/logger";

interface UsePresetsOpts {
  /** Commit + persist a complete params set (preset apply / paste). */
  apply: (p: DevelopParams) => void;
  /** Transient render of a params set (hover preview), no persist. */
  preview: (p: DevelopParams) => void;
  /** The current live develop params. */
  getCurrentParams: () => DevelopParams;
}

/** Preset list + apply/create/import/export + copy-paste-settings + hover-preview logic. */
export function usePresets(opts: UsePresetsOpts) {
  const { apply, preview, getCurrentParams } = opts;
  const selectedId = useAppStore((s) => s.selectedId);
  const setToast = useAppStore((s) => s.setToast);
  const amount = useDevelopStore((s) => s.presetAmount);
  const setPresetAmount = useDevelopStore((s) => s.setPresetAmount);
  const copied = useDevelopStore((s) => s.copiedSettings);
  const setCopiedSettings = useDevelopStore((s) => s.setCopiedSettings);

  const [presets, setPresets] = useState<PresetSummary[]>([]);
  const [report, setReport] = useState<ImportReport | null>(null);

  // Params saved before a hover-preview, restored on hover-end. A monotonic token drops stale async.
  const hoverSaved = useRef<DevelopParams | null>(null);
  const hoverToken = useRef(0);

  const refresh = useCallback(() => {
    presetsList()
      .then(setPresets)
      .catch((e) => log.warn("presets", "list failed", log.errorSummary(e)));
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  // A stale hover-preview must never restore onto a DIFFERENT photo: drop the saved pre-hover params
  // (and supersede any in-flight hover) whenever the selected image changes.
  useEffect(() => {
    hoverSaved.current = null;
    hoverToken.current++;
  }, [selectedId]);

  const applyPreset = useCallback(
    async (presetId: number, replaceAll = false) => {
      if (selectedId === null) return;
      const saved = hoverSaved.current; // real pre-hover params, if a preview is showing
      hoverSaved.current = null;
      hoverToken.current++;
      // Restore the real params into the store (no render) so the undo step records the true
      // pre-apply state rather than the hover preview.
      if (saved) useDevelopStore.setState({ params: saved });
      // Snapshot the params reference; if it changes during the async apply (a slider drag, undo, or
      // image switch), the user has taken over — don't clobber their newer state with this result.
      const before = useDevelopStore.getState().params;
      try {
        const p = await presetsApply(
          selectedId,
          presetId,
          amount / 100,
          replaceAll,
        );
        if (useDevelopStore.getState().params !== before) return;
        apply(p);
      } catch (e) {
        setToast("Couldn't apply preset");
        log.warn("presets", "apply failed", log.errorSummary(e));
      }
    },
    [selectedId, amount, apply, setToast],
  );

  const hoverStart = useCallback(
    async (presetId: number) => {
      if (selectedId === null) return;
      if (useDevelopStore.getState().showBefore) return;
      if (hoverSaved.current === null) hoverSaved.current = getCurrentParams();
      const token = ++hoverToken.current;
      try {
        const p = await presetsApply(selectedId, presetId, amount / 100, false);
        if (token !== hoverToken.current) return; // superseded / hover ended
        preview(p);
      } catch {
        /* hover preview is best-effort */
      }
    },
    [selectedId, amount, preview, getCurrentParams],
  );

  const hoverEnd = useCallback(() => {
    hoverToken.current++;
    const saved = hoverSaved.current;
    hoverSaved.current = null;
    if (saved) preview(saved);
  }, [preview]);

  const saveCurrentAsPreset = useCallback(
    async (
      name: string,
      groupName: string | undefined,
      fieldKeys: string[],
      isFavorite: boolean,
    ) => {
      try {
        await presetsSave(
          name,
          groupName,
          fieldKeys,
          isFavorite,
          getCurrentParams(),
        );
        refresh();
        setToast(`Saved preset “${name}”`);
      } catch (e) {
        setToast("Couldn't save preset");
        log.warn("presets", "save failed", log.errorSummary(e));
      }
    },
    [getCurrentParams, refresh, setToast],
  );

  const renamePreset = useCallback(
    async (presetId: number, name: string) => {
      try {
        await presetsUpdate(presetId, { name });
        refresh();
      } catch (e) {
        setToast("Couldn't rename preset");
        log.warn("presets", "rename failed", log.errorSummary(e));
      }
    },
    [refresh, setToast],
  );

  const toggleFavorite = useCallback(
    async (p: PresetSummary) => {
      try {
        await presetsUpdate(p.id, { isFavorite: !p.isFavorite });
        refresh();
      } catch (e) {
        log.warn("presets", "favorite failed", log.errorSummary(e));
      }
    },
    [refresh],
  );

  const removePreset = useCallback(
    async (presetId: number) => {
      try {
        await presetsDelete(presetId);
        refresh();
      } catch (e) {
        setToast("Couldn't delete preset");
        log.warn("presets", "delete failed", log.errorSummary(e));
      }
    },
    [refresh, setToast],
  );

  const duplicatePreset = useCallback(
    async (presetId: number) => {
      try {
        await presetsDuplicate(presetId);
        refresh();
      } catch (e) {
        setToast("Couldn't duplicate preset");
        log.warn("presets", "duplicate failed", log.errorSummary(e));
      }
    },
    [refresh, setToast],
  );

  const exportPreset = useCallback(
    async (p: PresetSummary) => {
      try {
        const dest = await save({
          title: "Export preset",
          defaultPath: `${p.name}.json`,
          filters: [{ name: "Preset", extensions: ["json"] }],
        });
        if (!dest) return;
        await presetsExport(p.id, dest);
        setToast(`Exported “${p.name}”`);
      } catch (e) {
        setToast("Couldn't export preset");
        log.warn("presets", "export failed", log.errorSummary(e));
      }
    },
    [setToast],
  );

  const importPreset = useCallback(async () => {
    try {
      const src = await open({
        title: "Import preset",
        multiple: false,
        filters: [
          { name: "Presets", extensions: ["json", "xmp", "lrtemplate"] },
        ],
      });
      if (!src || typeof src !== "string") return;
      const res = await presetsImportFile(src);
      refresh();
      setReport(res.report);
    } catch (e) {
      setToast("Couldn't import preset");
      log.warn("presets", "import failed", log.errorSummary(e));
    }
  }, [refresh, setToast]);

  const copySettings = useCallback(() => {
    setCopiedSettings({
      params: getCurrentParams(),
      fieldKeys: [...ALL_FIELD_KEYS],
    });
    setToast("Settings copied");
  }, [getCurrentParams, setCopiedSettings, setToast]);

  const pasteSettings = useCallback(async () => {
    if (selectedId === null) return;
    const c = useDevelopStore.getState().copiedSettings;
    if (!c) {
      setToast("Nothing to paste");
      return;
    }
    const before = useDevelopStore.getState().params;
    try {
      const p = await developApplySettings(
        selectedId,
        c.params,
        c.fieldKeys,
        amount / 100,
        false,
      );
      if (useDevelopStore.getState().params !== before) return;
      apply(p);
      setToast("Settings pasted");
    } catch (e) {
      setToast("Couldn't paste settings");
      log.warn("presets", "paste failed", log.errorSummary(e));
    }
  }, [selectedId, amount, apply, setToast]);

  return {
    presets,
    refresh,
    amount,
    setPresetAmount,
    applyPreset,
    hoverStart,
    hoverEnd,
    saveCurrentAsPreset,
    renamePreset,
    toggleFavorite,
    removePreset,
    duplicatePreset,
    exportPreset,
    importPreset,
    copySettings,
    pasteSettings,
    canPaste: copied !== null,
    report,
    clearReport: () => setReport(null),
  };
}

export type PresetsApi = ReturnType<typeof usePresets>;
