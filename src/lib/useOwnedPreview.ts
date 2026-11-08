import { useEffect, useRef, useState } from "react";
import { log } from "./logger";

const OWNER_ATTR = "dataDarkroomOwnerImageId";

/** Tag a loaded image with the id it was loaded for (readable via `ownedImageId`). */
function setOwnedImageId(img: HTMLImageElement, imageId: number): void {
  Reflect.set(img, OWNER_ATTR, imageId);
}

/** The image id `img` was loaded for, or null when untagged. */
export function ownedImageId(img: HTMLImageElement | null): number | null {
  if (!img) return null;
  const v = Reflect.get(img, OWNER_ATTR);
  return typeof v === "number" ? v : null;
}

/**
 * Loads `url` into an image owned by (imageId, generation). Results that arrive after the
 * owner changed are dropped; abandoned object URLs are revoked.
 */
export function useOwnedPreview(
  imageId: number | null,
  url: string | null,
  fallback: (imageId: number) => Promise<string>,
): HTMLImageElement | null {
  const [img, setImg] = useState<HTMLImageElement | null>(null);
  const generationRef = useRef(0);

  useEffect(() => {
    const owner = { cancelled: false };
    const generation = ++generationRef.current;
    // Clear first: a previous image's element is never shown under the new owner.
    setImg(null);
    if (imageId === null || url === null) return;

    const inFlight: HTMLImageElement[] = [];
    let objectUrl: string | null = null;
    const stale = () => owner.cancelled || generationRef.current !== generation;
    const adopt = (el: HTMLImageElement) => {
      if (!stale()) {
        setOwnedImageId(el, imageId);
        setImg(el);
      }
    };
    const release = (u: string) => {
      URL.revokeObjectURL(u);
      if (objectUrl === u) objectUrl = null;
    };

    const primary = new Image();
    inFlight.push(primary);
    primary.onload = () => adopt(primary);
    primary.onerror = () =>
      loadFallback(imageId, url, fallback, stale, adopt, release, (el, src) => {
        inFlight.push(el);
        objectUrl = src;
      });
    primary.src = url;

    return () => {
      owner.cancelled = true;
      for (const el of inFlight) {
        el.onload = null;
        el.onerror = null;
        el.src = "";
      }
      if (objectUrl !== null) {
        URL.revokeObjectURL(objectUrl);
        objectUrl = null;
      }
    };
  }, [imageId, url, fallback]);

  return img;
}

/** Load the embedded-JPEG fallback for `capturedId` (pinned at effect start, never read from
 *  the store at error time). Abandoned object URLs are revoked immediately. */
function loadFallback(
  capturedId: number,
  url: string,
  fallback: (imageId: number) => Promise<string>,
  stale: () => boolean,
  adopt: (el: HTMLImageElement) => void,
  release: (u: string) => void,
  track: (el: HTMLImageElement, src: string) => void,
): void {
  // thumb:// / object URL failed — the embedded camera JPEG keeps the first paint non-blank.
  log.warn("develop", "preview url failed to load", { url });
  fallback(capturedId)
    .then((fbUrl) => {
      if (stale()) {
        URL.revokeObjectURL(fbUrl);
        return;
      }
      const fb = new Image();
      track(fb, fbUrl);
      fb.onload = () => {
        adopt(fb);
        release(fbUrl);
      };
      fb.onerror = () => release(fbUrl);
      fb.src = fbUrl;
    })
    .catch((e) =>
      log.warn("develop", "preview jpeg fallback failed", {
        imageId: capturedId,
        ...log.errorSummary(e),
      }),
    );
}
