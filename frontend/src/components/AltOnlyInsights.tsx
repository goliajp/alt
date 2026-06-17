import type { DiffPartAware, DiffStructured } from "../lib/api";

/** Top-of-card explainer comparing what git would surface against what
 *  alt actually computed. The point is to make the alt-only outputs
 *  read first — the visual diff tools below are just an inspection
 *  surface, this strip is what git literally cannot reproduce. */
interface Props {
  file: DiffPartAware | DiffStructured;
}

export function AltOnlyInsights({ file }: Props) {
  const items = collectInsights(file);
  if (items.length === 0) return null;

  return (
    <div className="border-b border-border-default bg-canvas-inset/40">
      <div className="px-4 py-3 grid grid-cols-1 md:grid-cols-[1fr_auto_1fr] gap-3 items-stretch">
        <Side
          tone="dim"
          title="What git would show"
          body={gitVerdict(file)}
          tag="git"
        />
        <div className="hidden md:flex items-center justify-center text-fg-subtle font-mono text-xs">
          ⇄
        </div>
        <Side
          tone="alt"
          title="What alt computed"
          body={items}
          tag="alt"
          format={file.format}
        />
      </div>
    </div>
  );
}

function Side({
  tone,
  title,
  body,
  tag,
  format,
}: {
  tone: "alt" | "dim";
  title: string;
  body: string | Insight[];
  tag: string;
  format?: string;
}) {
  const isAlt = tone === "alt";
  return (
    <div
      className={`rounded-md border px-4 py-3 ${
        isAlt
          ? "border-warm/40 bg-warm/5"
          : "border-border-muted bg-canvas-inset/60"
      }`}
    >
      <div className="flex items-center gap-2 mb-2">
        <span
          className={`text-[10px] uppercase tracking-[0.22em] font-mono ${
            isAlt ? "text-warm" : "text-fg-subtle"
          }`}
        >
          {tag}
        </span>
        {format ? (
          <span className="text-[10px] uppercase tracking-[0.18em] font-mono text-fg-subtle">
            · {format}
          </span>
        ) : null}
        <span className="text-[11px] uppercase tracking-[0.18em] text-fg-muted ml-auto">
          {title}
        </span>
      </div>
      {typeof body === "string" ? (
        <div className="font-mono text-sm text-fg-default">{body}</div>
      ) : (
        <ul className="space-y-1.5">
          {body.map((item, i) => (
            <li key={i} className="font-mono text-[13px] text-fg-default leading-snug">
              <span className="text-warm mr-2">▸</span>
              {item.body}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

interface Insight {
  body: React.ReactNode;
}

function collectInsights(file: DiffPartAware | DiffStructured): Insight[] {
  const out: Insight[] = [];
  if (file.kind === "structured") {
    out.push({
      body: (
        <>
          parsed as <strong>{file.format.toUpperCase()}</strong>;{" "}
          <strong>{file.paths.length}</strong> per-jq-path semantic change
          {file.paths.length === 1 ? "" : "s"} reported (git: line diff only)
        </>
      ),
    });
    return out;
  }
  // part_aware
  if (file.format === "png") {
    const changedChunks = file.parts.filter(
      (p) => p.change === "changed",
    ).length;
    const sameChunks = file.parts.filter((p) => p.change === "same").length;
    out.push({
      body: (
        <>
          PNG container parsed: <strong>{changedChunks}</strong> chunk
          {changedChunks === 1 ? "" : "s"} changed, <strong>{sameChunks}</strong>{" "}
          unchanged
        </>
      ),
    });
    if (file.perceptual_distance != null) {
      out.push({
        body: (
          <>
            perceptual fingerprint Δ ={" "}
            <strong>{file.perceptual_distance.toFixed(3)}</strong>{" "}
            <span className="text-fg-muted">
              (0 = identical pixels, 1 = inverted)
            </span>
          </>
        ),
      });
    }
    if (file.perceptual_hash_old && file.perceptual_hash_new) {
      const bitsDiff = hammingDistance(
        file.perceptual_hash_old,
        file.perceptual_hash_new,
      );
      out.push({
        body: (
          <>
            inflated-IDAT bucket hash diff:{" "}
            <strong>
              {bitsDiff} / 64 bits
            </strong>{" "}
            <span className="text-fg-muted">(visualised below)</span>
          </>
        ),
      });
    }
    return out;
  }
  // zip / ooxml
  const ext = file.path.split(".").pop()?.toLowerCase();
  const isOOXML = ext && ["docx", "xlsx", "pptx"].includes(ext);
  const changedMembers = file.parts.filter(
    (p) => p.change === "changed",
  ).length;
  const sameMembers = file.parts.filter((p) => p.change === "same").length;
  const innerDiffs = file.parts.filter((p) => !!p.text_patch).length;
  out.push({
    body: (
      <>
        {isOOXML ? "OOXML container" : "ZIP container"} walked:{" "}
        <strong>{changedMembers}</strong> member
        {changedMembers === 1 ? "" : "s"} changed, <strong>{sameMembers}</strong>{" "}
        unchanged
      </>
    ),
  });
  if (innerDiffs > 0) {
    out.push({
      body: (
        <>
          <strong>{innerDiffs}</strong> changed member
          {innerDiffs === 1 ? "" : "s"} has a per-line unified diff over the
          inflated XML body{" "}
          <span className="text-fg-muted">(expand a row to see it)</span>
        </>
      ),
    });
  }
  return out;
}

function gitVerdict(file: DiffPartAware | DiffStructured): string {
  if (file.kind === "structured") {
    return `git diff would render line-by-line over ${file.format.toUpperCase()} text. The semantic structure is invisible to git.`;
  }
  return "Binary files differ";
}

function hammingDistance(a: string, b: string): number {
  // Count bit differences between two 64-bit hex strings.
  const ai = BigInt("0x" + a);
  const bi = BigInt("0x" + b);
  let diff = ai ^ bi;
  let bits = 0;
  while (diff > 0n) {
    if (diff & 1n) bits += 1;
    diff >>= 1n;
  }
  return bits;
}
