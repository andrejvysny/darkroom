import { developSetEdit, type DevelopParams } from "./ipc";
import { log } from "./logger";

// Catalog persistence for Develop edits, owned per image.
//
// The previous implementation debounced through ONE shared timer, so `persist(B, …)` cancelled the
// timer still holding A's save: edit A → switch to B → edit B within the window silently lost A's
// edit. Nothing downstream could recover it, because it never left the JavaScript timeout.
//
// The model here mirrors Lightroom's: the catalog is the fast primary state, written on a short
// idle debounce and, during a continuous drag, at least every `MAX_WAIT_MS` — so an eight-second
// slider gesture does not sit unsaved for eight seconds. Rendering keeps its own much shorter
// debounce; the two are deliberately unrelated.
//
// Guarantees:
//   - per-image pending state: an edit to B never touches A's pending save;
//   - latest-wins: intermediate slider values collapse into the newest params;
//   - serialized per image: generation 43 waits for 42 to land, so an older async call can never
//     complete after a newer one;
//   - failures stay dirty and retry, and surface once the retries are exhausted.

/** Trailing debounce after the user stops moving a control. */
export const IDLE_MS = 250;
/** Hard ceiling between saves while the user keeps editing the same image. */
export const MAX_WAIT_MS = 1000;
/** Backoff for retrying a failed save, then the failure becomes visible. */
const RETRY_BACKOFF_MS = [500, 1500, 4000];
/**
 * Idle delay before refreshing the edited thumbnail. Regenerating it costs a full RAW decode, and
 * `MAX_WAIT_MS` makes saves land *during* a long drag — so tying the refresh to each save would
 * put a decode per second behind an interactive gesture. It waits for the gesture to end instead.
 */
const THUMB_IDLE_MS = 1200;

type SaveFn = (
  imageId: number,
  params: DevelopParams,
  touchCount: number,
) => Promise<void>;

interface Entry {
  latest: DevelopParams;
  /** Bumped on every edit; `savedGen` trails it until the catalog catches up. */
  dirtyGen: number;
  savedGen: number;
  /** Edits accumulated since the last successful save (behavioural log input). */
  touchCount: number;
  /** When this image first went dirty after being clean — the `MAX_WAIT_MS` anchor. */
  dirtySince: number;
  timer: ReturnType<typeof setTimeout> | null;
  /** Non-null while a save is in flight; the next save chains onto it. */
  inflight: Promise<void> | null;
  failures: number;
}

export class DevelopPersistence {
  private entries = new Map<number, Entry>();
  /** Pending idle thumbnail refreshes, keyed by image id (see `THUMB_IDLE_MS`). */
  private thumbTimers = new Map<number, ReturnType<typeof setTimeout>>();

  constructor(
    private readonly save: SaveFn,
    /** Called with a message when saves are failing, and with `null` once one succeeds. */
    private readonly onError: (message: string | null) => void,
    /** Optional post-save side effect (thumbnail refresh); failures are ignored. */
    private readonly afterSave?: (imageId: number) => void,
  ) {}

  /** Record an edit. Latest params win; the save itself is debounced. */
  queue(imageId: number, params: DevelopParams): void {
    this.cancelThumb(imageId); // the gesture continues — re-armed after the next save
    const e = this.entries.get(imageId);
    if (e) {
      if (e.dirtyGen === e.savedGen) e.dirtySince = Date.now();
      e.latest = params;
      e.dirtyGen += 1;
      e.touchCount += 1;
      // Deliberately NOT resetting `failures`: if saves are failing, continuing to edit must not
      // reset the backoff or hide the banner — that is exactly when the user needs to be told.
    } else {
      this.entries.set(imageId, {
        latest: params,
        dirtyGen: 1,
        savedGen: 0,
        touchCount: 1,
        dirtySince: Date.now(),
        timer: null,
        inflight: null,
        failures: 0,
      });
    }
    this.schedule(imageId);
  }

  /** True while `imageId` has edits the catalog has not accepted yet. */
  isDirty(imageId: number): boolean {
    const e = this.entries.get(imageId);
    return e !== undefined && e.dirtyGen !== e.savedGen;
  }

  /**
   * True while ANY image still has unsaved edits. The quit barrier needs this: a failed save is
   * swallowed into the retry schedule rather than rejecting, so `flushAll()` resolving is not by
   * itself evidence that everything landed.
   */
  hasPending(): boolean {
    for (const e of this.entries.values()) {
      if (e.dirtyGen !== e.savedGen) return true;
    }
    return false;
  }

  /**
   * Persist everything `imageId` had pending AT CALL TIME, and resolve once that generation has
   * landed or a save has failed. Edits made *during* the flush keep their own schedule — waiting
   * for them too would let a live gesture stall the caller indefinitely.
   */
  async flush(imageId: number): Promise<void> {
    const first = this.entries.get(imageId);
    if (!first) return;
    const target = first.dirtyGen;
    // A save may already be in flight for an OLDER generation, so one `run` need not reach the
    // target; each successful one saves the then-current generation, so this settles in a step or
    // two, and any failure exits to the backoff schedule.
    for (;;) {
      const e = this.entries.get(imageId);
      if (!e || e.savedGen >= target) return;
      this.clearTimer(e);
      const failuresBefore = e.failures;
      await this.run(imageId);
      if (e.failures > failuresBefore) return;
    }
  }

  /** Save every pending image now. Used on navigation, leaving Develop, and quit. */
  async flushAll(): Promise<void> {
    await Promise.all([...this.entries.keys()].map((id) => this.flush(id)));
  }

  /** Drop pending state for an image without saving — only for an explicit discard. */
  forget(imageId: number): void {
    const e = this.entries.get(imageId);
    if (e) this.clearTimer(e);
    this.entries.delete(imageId);
    this.cancelThumb(imageId);
  }

  private cancelThumb(imageId: number): void {
    const t = this.thumbTimers.get(imageId);
    if (t !== undefined) {
      clearTimeout(t);
      this.thumbTimers.delete(imageId);
    }
  }

  private armThumb(imageId: number): void {
    if (!this.afterSave) return;
    this.cancelThumb(imageId);
    this.thumbTimers.set(
      imageId,
      setTimeout(() => {
        this.thumbTimers.delete(imageId);
        // A newer edit may have landed while the timer ran; it will re-arm on its own save.
        if (!this.entries.has(imageId)) this.afterSave?.(imageId);
      }, THUMB_IDLE_MS),
    );
  }

  private clearTimer(e: Entry): void {
    if (e.timer !== null) {
      clearTimeout(e.timer);
      e.timer = null;
    }
  }

  private schedule(imageId: number, delay?: number): void {
    const e = this.entries.get(imageId);
    if (!e || e.dirtyGen === e.savedGen) return;
    this.clearTimer(e);
    // Idle debounce, but never postpone a continuously-edited image past MAX_WAIT_MS.
    const wait =
      delay ??
      Math.max(0, Math.min(IDLE_MS, e.dirtySince + MAX_WAIT_MS - Date.now()));
    e.timer = setTimeout(() => {
      e.timer = null;
      void this.run(imageId);
    }, wait);
  }

  /** Serialize saves per image: chain onto any in-flight one instead of racing it. */
  private run(imageId: number): Promise<void> {
    const e = this.entries.get(imageId);
    if (!e) return Promise.resolve();
    if (e.inflight) return e.inflight;
    if (e.dirtyGen === e.savedGen) return Promise.resolve();

    const gen = e.dirtyGen;
    const params = e.latest;
    const touches = e.touchCount;
    const p = this.save(imageId, params, touches)
      .then(() => {
        e.savedGen = gen;
        // Touches that arrived while this save was in flight belong to the next one.
        e.touchCount = Math.max(0, e.touchCount - touches);
        e.failures = 0;
        this.onError(null);
      })
      .catch((err: unknown) => {
        // Stay dirty: the edit is still only in memory, so the next attempt must resend it.
        e.failures += 1;
        log.warn("develop", "set edit failed", {
          imageId,
          attempt: e.failures,
          ...log.errorSummary(err),
        });
        if (e.failures > RETRY_BACKOFF_MS.length) {
          this.onError("Changes haven't been saved. Darkroom will keep trying.");
        }
      })
      .finally(() => {
        e.inflight = null;
        if (e.dirtyGen !== e.savedGen) {
          const backoff =
            e.failures > 0
              ? (RETRY_BACKOFF_MS[
                  Math.min(e.failures, RETRY_BACKOFF_MS.length) - 1
                ] ?? IDLE_MS)
              : 0;
          this.schedule(imageId, backoff);
        } else if (this.entries.get(imageId) === e) {
          // Clean and idle — drop the entry so the map does not grow with the library, and let the
          // thumbnail catch up once the user has actually stopped editing.
          this.entries.delete(imageId);
          this.armThumb(imageId);
        }
      });
    e.inflight = p;
    return p;
  }
}

/** The app-wide instance. `configure` wires the store-facing side effects once, from React. */
let errorSink: (message: string | null) => void = () => {};
let thumbSink: ((imageId: number) => void) | undefined;

export const developPersistence = new DevelopPersistence(
  (id, params, touchCount) => developSetEdit(id, params, touchCount),
  (message) => errorSink(message),
  (id) => thumbSink?.(id),
);

export function configureDevelopPersistence(opts: {
  onError: (message: string | null) => void;
  afterSave?: (imageId: number) => void;
}): void {
  errorSink = opts.onError;
  thumbSink = opts.afterSave;
}
