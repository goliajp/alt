const IMAGE_EXTS = new Set([
  "png",
  "jpg",
  "jpeg",
  "gif",
  "webp",
  "avif",
  "bmp",
  "ico",
  "svg",
]);

export function isImagePath(path: string): boolean {
  const ext = (path.split(".").pop() ?? "").toLowerCase();
  return IMAGE_EXTS.has(ext);
}

export function isRasterImagePath(path: string): boolean {
  const ext = (path.split(".").pop() ?? "").toLowerCase();
  return ext !== "svg" && IMAGE_EXTS.has(ext);
}
