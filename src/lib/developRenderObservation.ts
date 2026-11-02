import type { DevelopParams } from "./ipc";

type RenderRequest = { requestId: number; imageId: number; params: DevelopParams };
type Observer = (request: RenderRequest) => (buffer: ArrayBuffer) => void;
let observer: Observer | undefined;
const ignore = (_buffer: ArrayBuffer): void => {};

/** Opt-in measurement hook: capture request identity before asynchronous IPC completes. */
export function setDevelopRenderObserver(value: Observer | undefined): void {
  observer = value;
}

export function beginDevelopRenderObservation(request: RenderRequest): (buffer: ArrayBuffer) => void {
  return observer?.(request) ?? ignore;
}
