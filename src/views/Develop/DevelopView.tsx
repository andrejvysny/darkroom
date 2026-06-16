import { useEffect } from "react";
import Stage from "./Stage";
import InstrumentPanel from "./InstrumentPanel";
import Filmstrip from "./Filmstrip";
import { useDevelop } from "./useDevelop";
import { useAppStore } from "../../store/app";
import { useDevelopStore } from "../../store/develop";

export default function DevelopView() {
  const selectedId = useAppStore((s) => s.selectedId);
  const setOnDevelopReset = useAppStore((s) => s.setOnDevelopReset);
  const rendering = useDevelopStore((s) => s.rendering);
  const showBefore = useDevelopStore((s) => s.showBefore);
  const setShowBefore = useDevelopStore((s) => s.setShowBefore);
  const {
    params,
    imageUrl,
    previewUrl,
    onParamChange,
    onCurveChange,
    onHslChange,
    resetKeys,
    reset,
  } = useDevelop();

  // Expose develop reset to the TopBar / command palette.
  useEffect(() => {
    setOnDevelopReset(reset);
    return () => setOnDevelopReset(null);
  }, [reset, setOnDevelopReset]);

  // `\` toggles a real before/after (renders DEFAULT_PARAMS). Ignored while typing in a field.
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
          onHoldBefore={setShowBefore}
          imageUrl={imageUrl}
          previewUrl={previewUrl}
          rendering={rendering}
        />
        <InstrumentPanel
          params={params}
          onParamChange={onParamChange}
          onCurveChange={onCurveChange}
          onHslChange={onHslChange}
          resetKeys={resetKeys}
          onReset={reset}
        />
      </div>
      <Filmstrip />
    </div>
  );
}
