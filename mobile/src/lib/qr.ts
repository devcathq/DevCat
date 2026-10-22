// Read a QR code from a photo. The browser version is served over plain http, where Safari
// gives no live camera, so pairing takes a photo of the QR code instead.
/** Longest side the photo is scaled down to before scanning (phone photos are ~4000px). */
const MAX = 1600;

/** The text in the QR code in this image, or null when none is found. */
export async function readQr(file: Blob): Promise<string | null> {
  const { default: jsQR } = await import("jsqr");
  const url = URL.createObjectURL(file);
  try {
    const img = new Image();
    img.src = url;
    await img.decode();
    const k = Math.min(1, MAX / Math.max(img.naturalWidth, img.naturalHeight));
    const w = Math.round(img.naturalWidth * k), h = Math.round(img.naturalHeight * k);
    const ctx = Object.assign(document.createElement("canvas"), { width: w, height: h }).getContext("2d", { willReadFrequently: true });
    if (!ctx) return null;
    ctx.drawImage(img, 0, 0, w, h);
    return jsQR(ctx.getImageData(0, 0, w, h).data, w, h)?.data ?? null;
  } finally {
    URL.revokeObjectURL(url);
  }
}
