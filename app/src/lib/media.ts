import { api, type MediaRef } from "./api";

const urls = new Map<string, Promise<string>>();

/** A blob URL for an image or voice note, fetched from the core once. */
export function mediaUrl(m: MediaRef): Promise<string> {
  let url = urls.get(m.id);
  if (!url) {
    url = api.media(m.id).then((bytes) => URL.createObjectURL(new Blob([bytes], { type: m.mime })));
    urls.set(m.id, url);
    url.catch(() => urls.delete(m.id));
  }
  return url;
}

const MAX_SOURCE_BYTES = 10 * 1024 * 1024;
const MAX_DIMENSION = 448;
const TARGET_BYTES = 45_000;

function jpeg(canvas: HTMLCanvasElement, quality: number): Promise<Blob> {
  return new Promise((resolve, reject) =>
    canvas.toBlob((b) => (b ? resolve(b) : reject(new Error("encode failed"))), "image/jpeg", quality),
  );
}

/** As iOS: at most 448 px, JPEG at 0.82 stepping down to about 45 KB.
 *  Redrawing drops all metadata (EXIF, GPS). */
export async function prepareImage(file: Blob): Promise<Uint8Array> {
  if (file.size > MAX_SOURCE_BYTES) throw new Error("image is too large (10 MB max)");
  const bitmap = await createImageBitmap(file);
  const scale = Math.min(1, MAX_DIMENSION / Math.max(bitmap.width, bitmap.height));
  const canvas = document.createElement("canvas");
  canvas.width = Math.max(1, Math.round(bitmap.width * scale));
  canvas.height = Math.max(1, Math.round(bitmap.height * scale));
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error("no canvas");
  ctx.fillStyle = "#fff"; // transparent images get a white background
  ctx.fillRect(0, 0, canvas.width, canvas.height);
  ctx.drawImage(bitmap, 0, 0, canvas.width, canvas.height);
  bitmap.close();
  let quality = 0.82;
  let blob = await jpeg(canvas, quality);
  while (blob.size > TARGET_BYTES && quality > 0.3) {
    quality -= 0.1;
    blob = await jpeg(canvas, quality);
  }
  return new Uint8Array(await blob.arrayBuffer());
}
