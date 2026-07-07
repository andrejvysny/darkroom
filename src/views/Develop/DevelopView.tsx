import { useCallback, useEffect, useRef, useState } from "react";
import Stage from "./Stage";
import InstrumentPanel from "./InstrumentPanel";
import Filmstrip from "./Filmstrip";
import DevelopSidePanel from "./DevelopSidePanel";
import PresetsPanel from "./PresetsPanel";
import HistoryPanel from "./HistoryPanel";
import CreatePresetDialog from "./CreatePresetDialog";
import ImportReportModal from "./ImportReportModal";
import { useDevelop } from "./useDevelop";
import { usePresets } from "./usePresets";
import { useHistory } from "./useHistory";
import { useAppStore } from "../../store/app";
import { useDevelopStore } from "../../store/develop";
import { developPreviewJpeg } from "../../lib/ipc";
import { log } from "../../lib/logger";

export default function DevelopView() {
  const selectedId = useAppStore((s) => s.selectedId);
  const libraryImages = useAppStore((s) => s.libraryImages);
  const setOnDevelopReset = useAppStore((s) => s.setOnDevelopReset);
  const setOnSavePreset = useAppStore((s) => s.setOnSavePreset);
  const setOnCopySettings = useAppStore((s) => s.setOnCopySettings);
  const setOnPasteSettings = useAppStore((s) => s.setOnPasteSettings);
  const rendering = useDevelopStore((s) => s.rendering);
  const showBefore = useDevelopStore((s) => s.showBefore);
  const setShowBefore = useDevelopStore((s) => s.setShowBefore);

  const {
    params,
    previewUrl,
    renderFrame,
    renderTick,
    renderError,
    retryRender,
    onParamChange,
    onCurveChange,
    onHslChange,
    onCropChange,
    onColorBalanceChange,
    onChannelMixChange,
    onBorderChange,
    resetKeys,
    reset,
    applyDevelopParams,
    previewDevelopParams,
    undo,
    redo,
    revertToOpened,
    addMask,
    updateMask,
    updateMaskAdjust,
    updateMaskComponentKind,
    addComponent,
    updateComponent,
    deleteComponent,
    appendStroke,
    appendAiPoint,
    deleteMask,
  } = useDevelop();

  const [createOpen, setCreateOpen] = useState(false);
  const getCurrentParams = useCallback(
    () => useDevelopStore.getState().params,
    [],
  );
  const presets = usePresets({
    apply: applyDevelopParams,
    preview: previewDevelopParams,
    getCurrentParams,
  });

  // Expose preset/copy/paste actions to the command palette.
  useEffect(() => {
    setOnSavePreset(() => setCreateOpen(true));
    setOnCopySettings(() => presets.copySettings());
    setOnPasteSettings(() => void presets.pasteSettings());
    return () => {
      setOnSavePreset(null);
      setOnCopySettings(null);
      setOnPasteSettings(null);
    };
  }, [
    setOnSavePreset,
    setOnCopySettings,
    setOnPasteSettings,
    presets.copySettings,
    presets.pasteSettings,
  ]);

  // ⌘⇧N save preset · ⌘⇧C copy settings · ⌘⇧V paste settings. Ignored while typing in a field.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!(e.metaKey || e.ctrlKey) || !e.shiftKey) return;
      const t = e.target as HTMLElement | null;
      if (t && (t.tagName === "INPUT" || t.tagName === "TEXTAREA")) return;
      if (e.code === "KeyN") {
        e.preventDefault();
        setCreateOpen(true);
      } else if (e.code === "KeyC") {
        e.preventDefault();
        presets.copySettings();
      } else if (e.code === "KeyV") {
        e.preventDefault();
        void presets.pasteSettings();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [presets.copySettings, presets.pasteSettings]);

  const history = useHistory({ apply: applyDevelopParams, getCurrentParams });
  const canUndo = useDevelopStore((s) => s.undoStack.length > 0);
  const canRedo = useDevelopStore((s) => s.redoStack.length > 0);

  // ⌘Z undo · ⌘⇧Z redo. Ignored while typing in a field.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!(e.metaKey || e.ctrlKey) || e.code !== "KeyZ") return;
      const t = e.target as HTMLElement | null;
      if (t && (t.tagName === "INPUT" || t.tagName === "TEXTAREA")) return;
      e.preventDefault();
      if (e.shiftKey) redo();
      else undo();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [undo, redo]);

  // Natural sensor dims from the selected ImageRow (drives viewport math + readout). Until the row
  // is available the aspect is unknown — GPU renders are suppressed (dimsKnown) so no wrong-aspect
  // frame is ever issued; the 3:2 placeholder only sizes the empty canvas.
  const selectedRow = libraryImages.find((r) => r.id === selectedId) ?? null;
  const dimsKnown = (selectedRow?.width ?? 0) > 0;
  const natural = {
    w: selectedRow?.width ?? 3,
    h: selectedRow?.height ?? 2,
  };

  // Publish the DISPLAYED-frame aspect so the crop tool's ratio presets can size hw/hh (useDevelop
  // resets it to 0 on image switch; this re-sets it once the row's real dims are known). 90°
  // rotation swaps W↔H. Without this the presets are gated off and silently do nothing.
  const setImageAspect = useDevelopStore((s) => s.setImageAspect);
  const rot90 = params.crop.rot90;
  useEffect(() => {
    if (!dimsKnown) return;
    const odd = (((rot90 % 4) + 4) % 4) % 2 === 1;
    setImageAspect(odd ? natural.h / natural.w : natural.w / natural.h);
  }, [dimsKnown, natural.w, natural.h, rot90, setImageAspect]);

  // Suppress GPU renders until the row's real dims are known (no wrong-aspect frame). Read through
  // a ref so the gated callback stays referentially stable for Stage.
  const dimsKnownRef = useRef(dimsKnown);
  dimsKnownRef.current = dimsKnown;
  const gatedRenderFrame = useCallback(
    (d: Parameters<typeof renderFrame>[0]) =>
      dimsKnownRef.current ? renderFrame(d) : Promise.resolve(null),
    [renderFrame],
  );

  // Embedded preview <img> for instant first paint on the canvas.
  const [previewImg, setPreviewImg] = useState<HTMLImageElement | null>(null);
  const previewUrlRef = useRef<string | null>(null);
  useEffect(() => {
    if (previewUrl === previewUrlRef.current) return;
    previewUrlRef.current = previewUrl;
    if (!previewUrl) {
      setPreviewImg(null);
      return;
    }
    const img = new Image();
    img.onload = () => setPreviewImg(img);
    img.onerror = () => {
      // thumb:// / object URL failed to load — fall back to the embedded camera JPEG so the
      // first paint is never silently blank.
      log.warn("develop", "preview url failed to load", { url: previewUrl });
      const id = useAppStore.getState().selectedId;
      if (id === null) return;
      developPreviewJpeg(id)
        .then((url) => {
          const fb = new Image();
          fb.onload = () => {
            setPreviewImg(fb);
            URL.revokeObjectURL(url);
          };
          fb.onerror = () => URL.revokeObjectURL(url);
          fb.src = url;
        })
        .catch((e) =>
          log.warn("develop", "preview jpeg fallback failed", {
            imageId: id,
            ...log.errorSummary(e),
          }),
        );
    };
    img.src = previewUrl;
  }, [previewUrl]);

  // Expose develop reset to the TopBar / command palette.
  useEffect(() => {
    setOnDevelopReset(reset);
    return () => setOnDevelopReset(null);
  }, [reset, setOnDevelopReset]);

  // `\` toggles before/after. Ignored while typing in a field.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "\\") return;
      const t = e.target as HTMLElement | null;
      if (t && (t.tagName === "INPUT" || t.tagName === "TEXTAREA")) return;
      e.preventDefault();
      setShowBefore(!useDevelopStore.getState().showBefore);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [setShowBefore]);

  if (selectedId === null) {
    return (
      <div
        style={{
          display: "flex",
          flex: 1,
          alignItems: "center",
          justifyContent: "center",
          color: "var(--color-t3)",
          fontSize: 14,
        }}
      >
        Select a photo to develop
      </div>
    );
  }

  return (
    <div
      style={{
        display: "flex",
        flexDirection: "column",
        flex: 1,
        minHeight: 0,
      }}
    >
      <div
        style={{
          display: "flex",
          flex: 1,
          minHeight: 0,
          overflow: "hidden",
        }}
      >
        <DevelopSidePanel
          presetsContent={
            <PresetsPanel
              api={presets}
              onOpenCreate={() => setCreateOpen(true)}
            />
          }
          historyContent={
            <HistoryPanel
              undo={undo}
              redo={redo}
              canUndo={canUndo}
              canRedo={canRedo}
              onRevertOpened={revertToOpened}
              onResetDefault={reset}
              history={history}
            />
          }
        />
        <Stage
          showBefore={showBefore}
          rendering={rendering}
          masks={params.masks}
          crop={params.crop}
          natural={natural}
          onCropChange={onCropChange}
          onChangeMaskKind={updateMaskComponentKind}
          onCommitStroke={appendStroke}
          onAiPoint={appendAiPoint}
          renderFn={gatedRenderFrame}
          renderTick={renderTick}
          renderGate={dimsKnown}
          previewImg={previewImg}
          renderError={renderError}
          onRetryRender={retryRender}
        />
        <InstrumentPanel
          params={params}
          onParamChange={onParamChange}
          onCurveChange={onCurveChange}
          onHslChange={onHslChange}
          onCropChange={onCropChange}
          onColorBalanceChange={onColorBalanceChange}
          onChannelMixChange={onChannelMixChange}
          onBorderChange={onBorderChange}
          resetKeys={resetKeys}
          onReset={reset}
          onAddMask={addMask}
          onDeleteMask={deleteMask}
          onUpdateMask={updateMask}
          onUpdateMaskAdjust={updateMaskAdjust}
          onAddComponent={addComponent}
          onUpdateComponent={updateComponent}
          onDeleteComponent={deleteComponent}
        />
      </div>
      <Filmstrip />
      <CreatePresetDialog
        open={createOpen}
        onClose={() => setCreateOpen(false)}
        onSave={presets.saveCurrentAsPreset}
      />
      <ImportReportModal
        report={presets.report}
        onClose={presets.clearReport}
      />
    </div>
  );
}
