import { useEffect } from "react";
import { useAppStore } from "./store/app";
import { useKeyboard } from "./hooks/useKeyboard";
import { useEditSync } from "./hooks/useEditSync";
import { useModelDownloadListeners } from "./lib/useModelDownloads";
import { useUpdaterStartup } from "./lib/useUpdater";
import { effectivePreviewEdge } from "./lib/ipc";
import TopBar from "./components/TopBar";
import CommandPalette from "./components/CommandPalette";
import ExportModal from "./views/Export/ExportModal";
import PanoramaModal from "./views/Panorama/PanoramaModal";
import PanoSuggestions from "./views/Panorama/PanoSuggestions";
import ModelManagerModal from "./views/Library/ModelManagerModal";
import DownloadPill from "./components/DownloadPill";
import PanoramaPill from "./components/PanoramaPill";
import UpdateBanner from "./components/UpdateBanner";
import Toast from "./components/Toast";
import LibraryView from "./views/Library/LibraryView";
import DevelopView from "./views/Develop/DevelopView";
import DedupView from "./views/Dedup/DedupView";

export default function App() {
  useKeyboard();
  useEditSync();
  // Subscribe once to all AI-model download progress streams (drives the manager + the global pill).
  useModelDownloadListeners();
  // Silent check for app updates shortly after launch (if auto-check is enabled).
  useUpdaterStartup();
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
      <ExportModal />
      <PanoramaModal />
      <PanoSuggestions />
      <ModelManagerModal />
      <DownloadPill />
      <PanoramaPill />
      <UpdateBanner />
      <Toast />
    </div>
  );
}
