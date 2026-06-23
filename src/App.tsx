import { useEffect } from "react";
import { useAppStore } from "./store/app";
import { useKeyboard } from "./hooks/useKeyboard";
import { useEditSync } from "./hooks/useEditSync";
import { effectivePreviewEdge } from "./lib/ipc";
import TopBar from "./components/TopBar";
import CommandPalette from "./components/CommandPalette";
import Toast from "./components/Toast";
import LibraryView from "./views/Library/LibraryView";
import DevelopView from "./views/Develop/DevelopView";
import DedupView from "./views/Dedup/DedupView";

export default function App() {
  useKeyboard();
  useEditSync();
  // Resolve (and on first launch, persist) the display-sharp preview edge early, so the background
  // queue starts rendering previews at the right size before the first loupe open.
  useEffect(() => {
    void effectivePreviewEdge();
  }, []);
  const view = useAppStore((s) => s.view);

  return (
    <div
      style={{
        display: "flex",
        flexDirection: "column",
        height: "100vh",
        overflow: "hidden",
      }}
    >
      <TopBar />
      {view === "library" && <LibraryView />}
      {view === "develop" && <DevelopView />}
      {view === "dedup" && <DedupView />}
      <CommandPalette />
      <Toast />
    </div>
  );
}
