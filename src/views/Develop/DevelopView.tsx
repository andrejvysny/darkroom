import { useEffect, useRef, useState } from "react";
import Stage from "./Stage";
import InstrumentPanel from "./InstrumentPanel";
import Filmstrip from "./Filmstrip";
import { useDevelop } from "./useDevelop";
import { useAppStore } from "../../store/app";
import { useDevelopStore } from "../../store/develop";

export default function DevelopView() {
  const selectedId = useAppStore((s) => s.selectedId);
  const libraryImages = useAppStore((s) => s.libraryImages);
  const setOnDevelopReset = useAppStore((s) => s.setOnDevelopReset);
  const rendering = useDevelopStore((s) => s.rendering);
  const showBefore = useDevelopStore((s) => s.showBefore);
  const setShowBefore = useDevelopStore((s) => s.setShowBefore);

  const {
    params,
    previewUrl,
    renderFrame,
    renderTick,
    onParamChange,
    onCurveChange,
    onHslChange,
    onCropChange,
    onColorBalanceChange,
    resetKeys,
    reset,
    addMask,
    updateMask,
    updateMaskAdjust,
    updateMaskComponentKind,
    addComponent,
    updateComponent,
    deleteComponent,
    appendStroke,
    deleteMask,
  } = useDevelop();

  // Natural sensor dims from the selected ImageRow (drives viewport math + readout).
  const selectedRow = libraryImages.find((r) => r.id === selectedId) ?? null;
  const natural = {
    w: selectedRow?.width ?? 3,
    h: selectedRow?.height ?? 2,
  };

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
        <Stage
          showBefore={showBefore}
          rendering={rendering}
          masks={params.masks}
          crop={params.crop}
          natural={natural}
          onCropChange={onCropChange}
          onChangeMaskKind={updateMaskComponentKind}
          onCommitStroke={appendStroke}
          renderFn={renderFrame}
          renderTick={renderTick}
          previewImg={previewImg}
        />
        <InstrumentPanel
          params={params}
          onParamChange={onParamChange}
          onCurveChange={onCurveChange}
          onHslChange={onHslChange}
          onCropChange={onCropChange}
          onColorBalanceChange={onColorBalanceChange}
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
    </div>
  );
}
