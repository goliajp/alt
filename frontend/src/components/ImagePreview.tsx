interface Props {
  /** Path like /api/repos/{name}/blob/{oid}/raw — the alt-web raw blob endpoint. */
  src: string;
  alt?: string;
  /** Cap the rendered height. */
  maxHeight?: number;
  /** Skip the dark padded frame; caller arranges its own layout. */
  bare?: boolean;
}

/** Render an image blob via the alt-web raw endpoint. */
export function ImagePreview({
  src,
  alt = "",
  maxHeight = 480,
  bare = false,
}: Props) {
  const img = (
    <img
      src={src}
      alt={alt}
      loading="lazy"
      className="max-w-full object-contain"
      style={{ maxHeight }}
    />
  );
  if (bare) return img;
  return <div className="bg-canvas-inset/40 p-4 flex justify-center">{img}</div>;
}
