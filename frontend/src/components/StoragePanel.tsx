import { useStorage } from "../lib/hooks";
import { formatBytes } from "../lib/format";
import type { StorageChunk } from "../lib/api";

/** Surfaces alt's physical storage layout for one git object: tier
 *  (0 = verbatim CDC, 1 = prism-decomposed), the prism + parts when
 *  Tier 1, and a per-leaf-chunk breakdown showing encoding choice
 *  (raw / zstd / delta) plus original-vs-stored bytes.
 *
 *  This is the visible answer to "what does alt actually store as the
 *  delta" — git has no equivalent inspection surface. */
export function StoragePanel({ repo, oid }: { repo: string; oid: string }) {
  const q = useStorage(repo, oid);

  if (q.isLoading) {
    return (
      <Frame title="Storage layout">
        <div className="px-4 py-3 font-mono text-xs text-fg-muted">loading…</div>
      </Frame>
    );
  }
  if (q.isError) {
    // Most likely the repo is git-backed (no native odb) — degrade
    // gracefully: the explanation is the answer the user came for.
    const msg = (q.error as Error).message;
    return (
      <Frame title="Storage layout">
        <div className="px-4 py-3 font-mono text-xs text-fg-muted">
          unavailable: {msg}
        </div>
      </Frame>
    );
  }
  const v = q.data!;

  const ratio =
    v.chunks.logical_total > 0
      ? v.chunks.stored_total / v.chunks.logical_total
      : 1;
  const physSaved = v.logical_size - v.chunks.stored_total;
  const deltaChunks = v.chunks.entries.filter((c) => c.encoding === "delta").length;
  const zstdChunks = v.chunks.entries.filter((c) => c.encoding === "zstd").length;
  const rawChunks = v.chunks.entries.filter((c) => c.encoding === "raw").length;

  return (
    <Frame title="Storage layout">
      <div className="px-4 py-3 border-b border-border-muted grid grid-cols-2 md:grid-cols-4 gap-3 text-sm">
        <Stat
          label="tier"
          value={v.tier === 1 ? `1 · prism #${v.tier1?.prism ?? "?"}` : "0 · verbatim CDC"}
          tone={v.tier === 1 ? "alt" : "dim"}
        />
        <Stat label="logical" value={formatBytes(v.logical_size)} />
        <Stat
          label="on disk"
          value={formatBytes(v.chunks.stored_total)}
          subtitle={`${(ratio * 100).toFixed(1)}% of logical`}
          tone={physSaved > 0 ? "alt" : "dim"}
        />
        <Stat
          label="chunks"
          value={`${v.chunks.leaf_count}`}
          subtitle={[
            deltaChunks ? `${deltaChunks} delta` : null,
            zstdChunks ? `${zstdChunks} zstd` : null,
            rawChunks ? `${rawChunks} raw` : null,
          ]
            .filter(Boolean)
            .join(" · ")}
        />
      </div>

      {v.tier1 ? <Tier1Detail tier1={v.tier1} /> : <Tier0Reason />}

      <ChunkTable chunks={v.chunks.entries} />

      <div className="px-4 py-2 text-[11px] font-mono text-fg-subtle border-t border-border-muted">
        blob {v.blob_id.slice(0, 16)}… · {kindLabel(v.kind)} ·{" "}
        {v.git_oid.slice(0, 12)}
      </div>
    </Frame>
  );
}

function Frame({
  title,
  children,
}: {
  title: string;
  children: React.ReactNode;
}) {
  return (
    <div className="bg-canvas-subtle border border-border-default rounded-lg overflow-hidden">
      <div className="px-4 py-2.5 bg-canvas-inset/40 border-b border-border-default flex items-center gap-3">
        <span className="text-[10px] uppercase tracking-[0.22em] font-mono text-warm">
          alt-only
        </span>
        <span className="text-sm text-fg-default">{title}</span>
        <span
          className="ml-auto text-[10px] uppercase tracking-[0.22em] font-mono text-fg-subtle"
          title="git has no equivalent"
        >
          git: opaque blob
        </span>
      </div>
      {children}
    </div>
  );
}

function Stat({
  label,
  value,
  subtitle,
  tone = "dim",
}: {
  label: string;
  value: string;
  subtitle?: string;
  tone?: "alt" | "dim";
}) {
  return (
    <div>
      <div
        className={`text-[10px] uppercase tracking-[0.22em] font-mono ${
          tone === "alt" ? "text-warm" : "text-fg-subtle"
        }`}
      >
        {label}
      </div>
      <div className="font-mono text-fg-default mt-0.5">{value}</div>
      {subtitle ? (
        <div className="text-[11px] font-mono text-fg-muted">{subtitle}</div>
      ) : null}
    </div>
  );
}

function Tier1Detail({
  tier1,
}: {
  tier1: {
    prism: number;
    recipe_len: number;
    record_blob: string;
    parts: string[];
  };
}) {
  return (
    <div className="px-4 py-3 border-b border-border-muted bg-warm/5">
      <div className="text-[11px] font-mono text-warm mb-1.5">
        accepted by prism #{tier1.prism} ·{" "}
        {tier1.prism === 1 ? "deflate-strip" : "prism"} ·{" "}
        {tier1.parts.length} part{tier1.parts.length === 1 ? "" : "s"}
      </div>
      <p className="text-xs text-fg-muted leading-relaxed">
        At ingest, the prism inflated the deflate-wrapped stream and stored
        the inner bytes as a separate blob. Identical inner bytes from any
        other file dedup against this part — git stores the deflate-wrapped
        blob verbatim every time.
      </p>
      <details className="mt-2">
        <summary className="text-[11px] font-mono text-fg-subtle cursor-pointer hover:text-fg-default">
          parts · recipe {tier1.recipe_len} B
        </summary>
        <ol className="mt-2 space-y-0.5">
          {tier1.parts.map((p, i) => (
            <li
              key={p}
              className="text-[11px] font-mono text-fg-muted truncate"
            >
              <span className="text-fg-subtle mr-2">{i}.</span>
              {p}
            </li>
          ))}
        </ol>
      </details>
    </div>
  );
}

function Tier0Reason() {
  return (
    <div className="px-4 py-3 border-b border-border-muted">
      <div className="text-[11px] font-mono text-fg-muted mb-1.5">
        Tier 0 · stored verbatim
      </div>
      <p className="text-xs text-fg-muted leading-relaxed">
        No prism accepted this blob at ingest, so its bytes are stored
        verbatim — CDC dedup still fires across files, but the format-aware
        teardown (PNG IDAT split, ZIP member walk, OOXML part graph) is not
        in use here. The prism layer is the long-running roadmap: each
        format gets its own decomposer over time.
      </p>
    </div>
  );
}

function ChunkTable({ chunks }: { chunks: StorageChunk[] }) {
  if (chunks.length === 0) return null;
  return (
    <div className="overflow-x-auto">
      <table className="w-full font-mono text-xs">
        <thead className="bg-canvas-inset/30 text-fg-subtle">
          <tr>
            <th className="text-left px-3 py-1.5 font-normal uppercase tracking-[0.18em] text-[10px]">
              #
            </th>
            <th className="text-left px-3 py-1.5 font-normal uppercase tracking-[0.18em] text-[10px]">
              chunk
            </th>
            <th className="text-left px-3 py-1.5 font-normal uppercase tracking-[0.18em] text-[10px]">
              encoding
            </th>
            <th className="text-right px-3 py-1.5 font-normal uppercase tracking-[0.18em] text-[10px]">
              logical
            </th>
            <th className="text-right px-3 py-1.5 font-normal uppercase tracking-[0.18em] text-[10px]">
              stored
            </th>
            <th className="text-right px-3 py-1.5 font-normal uppercase tracking-[0.18em] text-[10px]">
              %
            </th>
          </tr>
        </thead>
        <tbody>
          {chunks.map((c, i) => {
            const pct =
              c.orig_len > 0
                ? ((c.stored_len / c.orig_len) * 100).toFixed(0)
                : "—";
            return (
              <tr
                key={c.chunk_id + i}
                className="border-t border-border-muted hover:bg-canvas-inset/30"
              >
                <td className="px-3 py-1 text-fg-subtle">{i}</td>
                <td className="px-3 py-1 text-fg-muted">
                  {c.chunk_id.slice(0, 12)}…
                </td>
                <td className="px-3 py-1">
                  <EncodingChip enc={c.encoding} />
                </td>
                <td className="px-3 py-1 text-right text-fg-default">
                  {formatBytes(c.orig_len)}
                </td>
                <td className="px-3 py-1 text-right text-fg-default">
                  {formatBytes(c.stored_len)}
                </td>
                <td className="px-3 py-1 text-right text-fg-muted">
                  {pct === "—" ? pct : `${pct}%`}
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}

function EncodingChip({ enc }: { enc: "raw" | "zstd" | "delta" }) {
  const cls =
    enc === "delta"
      ? "border-warm/50 text-warm bg-warm/10"
      : enc === "zstd"
        ? "border-accent/40 text-accent bg-accent/5"
        : "border-border-muted text-fg-muted bg-canvas-inset/40";
  return (
    <span
      className={`inline-flex items-center px-1.5 py-0.5 rounded border text-[10px] font-mono uppercase tracking-[0.18em] ${cls}`}
    >
      {enc}
    </span>
  );
}

function kindLabel(k: string): string {
  return k;
}
