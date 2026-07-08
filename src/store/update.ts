import { create } from "zustand";

/** Lifecycle of an in-app update. `idle` = nothing to show (never checked, or dismissed). */
export type UpdateStatus =
  | "idle"
  | "checking"
  | "available"
  | "downloading"
  | "ready"
  | "uptodate"
  | "error";

export interface UpdateState {
  status: UpdateStatus;
  /** Remote version string once an update is available/ready; null otherwise. */
  version: string | null;
  /** Release notes body from latest.json (may be empty). */
  notes: string | null;
  /** Release date string from latest.json. */
  date: string | null;
  bytesDone: number;
  /** Total download size; 0 when the server didn't report a content-length. */
  bytesTotal: number;
  /** Failure message when status === "error". */
  error: string | null;
  patch: (p: Partial<Omit<UpdateState, "patch" | "reset">>) => void;
  reset: () => void;
}

const INITIAL = {
  status: "idle" as UpdateStatus,
  version: null,
  notes: null,
  date: null,
  bytesDone: 0,
  bytesTotal: 0,
  error: null,
};

/** Single source of truth for the update prompt/progress UI. The `Update` handle itself is
 *  non-serializable and lives module-side in `lib/useUpdater.ts`, not here. */
export const useUpdateStore = create<UpdateState>((set) => ({
  ...INITIAL,
  patch: (p) => set(p),
  reset: () => set(INITIAL),
}));
