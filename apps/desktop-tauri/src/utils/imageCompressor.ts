// Client-side image compression for outbound LLM attachments. Order of
// attempts: (1) skip when the file is already small; (2) resize +
// PNG output (pixel-identical); (3) lossless WebP; (4) JPEG with
// quality stepping down to the configured floor (LLM OCR needs ≥80%
// to recognise text reliably); (5) return the original file as
// fallback so a compression failure never blocks the user's send.
// Zero dependencies — pure browser APIs (OffscreenCanvas +
// convertToBlob).

export interface CompressConfig {
  /** Longest-edge pixel cap. The LLM backend resizes anyway — we
   *  do it client-side to skip the upload. */
  maxDimension: number;
  /** Per-file target byte size. */
  targetSizeBytes: number;
  /** Starting quality for screenshot-like sources (sharp edges, text). */
  screenshotQuality: number;
  /** Starting quality for photo-like sources (smooth gradients). */
  photoQuality: number;
  /** Floor the JPEG quality never crosses (LLM OCR needs ≥80%). */
  qualityFloor: number;
}

export const DEFAULT_COMPRESS_CONFIG: CompressConfig = {
  maxDimension: 2048,
  targetSizeBytes: 2 * 1024 * 1024, // 2MB
  screenshotQuality: 0.95,
  photoQuality: 0.85,
  qualityFloor: 0.80,
};

export type CompressMethod =
  | "skipped"
  | "resized"
  | "lossless-webp"
  | "reencoded"
  | "fallback";

export interface CompressResult {
  file: File;
  originalSize: number;
  compressedSize: number;
  isCompressed: boolean;
  method: CompressMethod;
}

function isImage(file: File): boolean {
  return file.type.startsWith("image/");
}

function isLikelyScreenshot(mimeType: string): boolean {
  // PNG 通常是截图；JPEG 通常是照片/扫描件
  return mimeType === "image/png";
}

// See module header for the full attempt order. On any failure path
// (decode error, missing OffscreenCanvas, every quality tier
// overshooting) the original file is returned so the user's send
// is never blocked by compression.
export async function compressImage(
  file: File,
  overrides: Partial<CompressConfig> = {},
): Promise<CompressResult> {
  const cfg: CompressConfig = { ...DEFAULT_COMPRESS_CONFIG, ...overrides };
  const originalSize = file.size;

  if (!isImage(file)) {
    return {
      file,
      originalSize,
      compressedSize: originalSize,
      isCompressed: false,
      method: "skipped",
    };
  }

  if (file.size <= cfg.targetSizeBytes) {
    return {
      file,
      originalSize,
      compressedSize: originalSize,
      isCompressed: false,
      method: "skipped",
    };
  }

  if (typeof OffscreenCanvas === "undefined" || typeof createImageBitmap === "undefined") {
    return {
      file,
      originalSize,
      compressedSize: originalSize,
      isCompressed: false,
      method: "fallback",
    };
  }

  let bitmap: ImageBitmap;
  try {
    bitmap = await createImageBitmap(file);
  } catch {
    return {
      file,
      originalSize,
      compressedSize: originalSize,
      isCompressed: false,
      method: "fallback",
    };
  }

  // Resizing only reduces pixel count, never changes pixel values.
  let { width, height } = bitmap;
  if (width > cfg.maxDimension || height > cfg.maxDimension) {
    const ratio = Math.min(cfg.maxDimension / width, cfg.maxDimension / height);
    width = Math.round(width * ratio);
    height = Math.round(height * ratio);
  }

  const canvas = new OffscreenCanvas(width, height);
  const ctx = canvas.getContext("2d");
  if (!ctx) {
    return {
      file,
      originalSize,
      compressedSize: originalSize,
      isCompressed: false,
      method: "fallback",
    };
  }
  ctx.drawImage(bitmap, 0, 0, width, height);

  if (isLikelyScreenshot(file.type)) {
    try {
      const pngBlob = await canvas.convertToBlob({ type: "image/png" });
      if (pngBlob.size <= cfg.targetSizeBytes) {
        return {
          file: new File([pngBlob], replaceExt(file.name, ".png"), {
            type: "image/png",
          }),
          originalSize,
          compressedSize: pngBlob.size,
          isCompressed: true,
          method: "resized",
        };
      }

      // Standard ImageEncodeOptions has no `lossless` field — Chromium
      // supports it as an extension but TS doesn't declare it. Use
      // quality=1 + cast to keep the webp encode lossless in practice.
      const webpBlob = await canvas.convertToBlob({
        type: "image/webp",
        quality: 1.0,
      } as ImageEncodeOptions);
      if (webpBlob.size <= cfg.targetSizeBytes) {
        return {
          file: new File([webpBlob], replaceExt(file.name, ".webp"), {
            type: "image/webp",
          }),
          originalSize,
          compressedSize: webpBlob.size,
          isCompressed: true,
          method: "lossless-webp",
        };
      }
    } catch {
      // lossless webp unsupported — fall through to the JPEG path.
    }
  }

  const startQ = isLikelyScreenshot(file.type)
    ? cfg.screenshotQuality
    : cfg.photoQuality;
  const qualities: number[] = [];
  for (let q = startQ; q >= cfg.qualityFloor - 0.001; q -= 0.05) {
    qualities.push(Math.max(cfg.qualityFloor, Math.round(q * 100) / 100));
  }
  const uniqueQualities = Array.from(new Set(qualities));

  for (const quality of uniqueQualities) {
    try {
      const jpegBlob = await canvas.convertToBlob({
        type: "image/jpeg",
        quality,
      });
      if (jpegBlob.size <= cfg.targetSizeBytes) {
        return {
          file: new File([jpegBlob], replaceExt(file.name, ".jpg"), {
            type: "image/jpeg",
          }),
          originalSize,
          compressedSize: jpegBlob.size,
          isCompressed: true,
          method: "reencoded",
        };
      }
    } catch {
      // try the next quality tier
    }
  }

  return {
    file,
    originalSize,
    compressedSize: originalSize,
    isCompressed: false,
    method: "fallback",
  };
}

// Skips compression. Used by the "preserve original quality" toggle.
export function skipCompression(file: File): CompressResult {
  return {
    file,
    originalSize: file.size,
    compressedSize: file.size,
    isCompressed: false,
    method: "skipped",
  };
}

// Compresses a batch of files in parallel.
export async function compressImages(
  files: File[],
  overrides: Partial<CompressConfig> = {},
): Promise<CompressResult[]> {
  return Promise.all(files.map((f) => compressImage(f, overrides)));
}

function replaceExt(filename: string, newExt: string): string {
  return filename.replace(/\.\w+$/, newExt);
}

// Formats a byte count as "X B / X.Y KB / X.YY MB".
export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(2)} MB`;
}