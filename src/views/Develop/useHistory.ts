import { useCallback, useEffect, useState } from "react";
import { useAppStore } from "../../store/app";
import { useDevelopStore } from "../../store/develop";
import {
  snapshotCreate,
  snapshotDelete,
  snapshotRename,
  snapshotRestore,
  snapshotsList,
  type DevelopParams,
  type SnapshotSummary,
} from "../../lib/ipc";
import { log } from "../../lib/logger";

interface UseHistoryOpts {
  /** Commit a restored snapshot's params (so the restore is an undoable step). */
  apply: (p: DevelopParams) => void;
  getCurrentParams: () => DevelopParams;
}

/** Persistent named snapshots for the current image (the saved side of the hybrid history). */
export function useHistory(opts: UseHistoryOpts) {
  const { apply, getCurrentParams } = opts;
  const selectedId = useAppStore((s) => s.selectedId);
  const setToast = useAppStore((s) => s.setToast);
  const [snapshots, setSnapshots] = useState<SnapshotSummary[]>([]);

  const refresh = useCallback(() => {
    if (selectedId === null) {
      setSnapshots([]);
      return;
    }
    snapshotsList(selectedId)
      .then(setSnapshots)
      .catch((e) => log.warn("history", "list failed", log.errorSummary(e)));
  }, [selectedId]);

  useEffect(() => {
    refresh();
  }, [refresh]);

  const createSnapshot = useCallback(
    async (name: string) => {
      if (selectedId === null) return;
      try {
        await snapshotCreate(
          selectedId,
          name.trim() || "Snapshot",
          getCurrentParams(),
        );
        refresh();
        setToast("Snapshot saved");
      } catch (e) {
        setToast("Couldn't save snapshot");
        log.warn("history", "create failed", log.errorSummary(e));
      }
    },
    [selectedId, getCurrentParams, refresh, setToast],
  );

  const restoreSnapshot = useCallback(
    async (id: number) => {
      // Snapshot the params reference; if it changes during the async restore (a slider drag in
      // flight, or the user switched images), don't clobber that newer state with the restore.
      const before = useDevelopStore.getState().params;
      try {
        const p = await snapshotRestore(id);
        if (useDevelopStore.getState().params !== before) return;
        apply(p);
      } catch (e) {
        setToast("Couldn't restore snapshot");
        log.warn("history", "restore failed", log.errorSummary(e));
      }
    },
    [apply, setToast],
  );

  const renameSnapshot = useCallback(
    async (id: number, name: string) => {
      if (!name.trim()) return;
      try {
        await snapshotRename(id, name.trim());
        refresh();
      } catch (e) {
        log.warn("history", "rename failed", log.errorSummary(e));
      }
    },
    [refresh],
  );

  const removeSnapshot = useCallback(
    async (id: number) => {
      try {
        await snapshotDelete(id);
        refresh();
      } catch (e) {
        log.warn("history", "delete failed", log.errorSummary(e));
      }
    },
    [refresh],
  );

  return {
    snapshots,
    refresh,
    createSnapshot,
    restoreSnapshot,
    renameSnapshot,
    removeSnapshot,
  };
}

export type HistoryApi = ReturnType<typeof useHistory>;
