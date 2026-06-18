import { useStorageStats } from "../lib/hooks";
import { formatBytes } from "../lib/format";

/** Repo-wide answer to "how big is alt vs the content it holds." Headline
 *  metric: stored / logical ratio. Then a tier 0/1 split, a chunk
 *  encoding mix (raw / zstd / delta), and per-prism hit counts so the
 *  reader can see *which* prism is responsible. */
export function StorageFootprint({ repo }: { repo: string }) {
  const q = useStorageStats(repo);

  if (q.isLoading) {
    return (
      <Frame>
        <div className="px-4 py-3 font-mono text-xs text-fg-muted">loading…</div>
      </Frame>
    );
  }
  if (q.isError) {
    return (
      <Frame>
        <div className="px-4 py-3 font-mono text-xs text-fg-muted">
          unavailable: {(q.error as Error).message}
        </div>
      </Frame>
    );
  }
  const s = q.data!;
  const ratio = s.logical_total > 0 ? s.stored_total / s.logical_total : 1;
  const diskRatio = s.logical_total > 0 ? s.disk_total / s.logical_total : 1;
  const saved = s.logical_total - s.stored_total;

  return (
    <Frame>
      {/* Headline */}
      <div className="px-5 py-5 border-b border-border-default grid grid-cols-1 md:grid-cols-[auto_1fr] gap-6 items-center">
        <div>
          <div className="text-[10px] uppercase tracking-[0.22em] font-mono text-warm mb-1">
            chunked + deduped
          </div>
          <div className="font-mono">
            <span className="text-4xl text-warm tabular-nums font-medium">
              {(ratio * 100).toFixed(1)}%
            </span>
            <span className="text-sm text-fg-muted ml-2">of logical</span>
          </div>
          <div className="text-xs font-mono text-fg-muted mt-1">
            {formatBytes(s.stored_total)} / {formatBytes(s.logical_total)} ·{" "}
            <span className={saved > 0 ? "text-diff-add-fg" : "text-fg-subtle"}>
              {saved > 0 ? `−${formatBytes(saved)}` : "no saving"}
            </span>
          </div>
        </div>
        <div className="text-xs font-mono text-fg-muted leading-relaxed border-l border-border-muted pl-6 md:max-w-md">
          <p>
            <strong className="text-fg-default">{s.objects.total}</strong> git
            objects ({s.objects.blobs} blobs, {s.objects.commits} commits,{" "}
            {s.objects.trees} trees)
            {" → "}
            <strong className="text-fg-default">{s.chunks.total}</strong> deduped
            CDC chunks.
          </p>
          <p className="mt-2">
            On-disk total (incl. maps + tier1):{" "}
            <strong className="text-fg-default">
              {formatBytes(s.disk_total)}
            </strong>{" "}
            ({(diskRatio * 100).toFixed(1)}% of logical).
          </p>
        </div>
      </div>

      {/* Tier split bar */}
      <Section title="tier" subtitle="how blobs entered storage">
        <Bar
          segments={[
            {
              label: "Tier 1 · prism-decomposed",
              value: s.tier.prismatic,
              tone: "warm",
            },
            {
              label: "Tier 0 · verbatim CDC",
              value: s.tier.verbatim,
              tone: "dim",
            },
          ]}
          unit="blobs"
        />
      </Section>

      {/* Encoding mix bar */}
      <Section title="chunk encoding" subtitle="how each chunk was stored">
        <Bar
          segments={[
            {
              label: "delta · zstd patch-from a lineage base",
              value: s.encoding.delta.stored,
              tone: "warm",
              detail: `${s.encoding.delta.chunks} chunks · ${formatBytes(s.encoding.delta.stored)}`,
            },
            {
              label: "zstd · per-chunk compression",
              value: s.encoding.zstd.stored,
              tone: "accent",
              detail: `${s.encoding.zstd.chunks} chunks · ${formatBytes(s.encoding.zstd.stored)}`,
            },
            {
              label: "raw · stored verbatim",
              value: s.encoding.raw.stored,
              tone: "dim",
              detail: `${s.encoding.raw.chunks} chunks · ${formatBytes(s.encoding.raw.stored)}`,
            },
          ]}
          unit="bytes"
        />
      </Section>

      {/* Per-prism hits */}
      {s.prisms.length > 0 ? (
        <Section title="prisms" subtitle="format-aware decomposers that fired">
          <ul className="px-4 py-3 grid grid-cols-1 sm:grid-cols-3 gap-3">
            {s.prisms.map((p) => (
              <li
                key={p.id}
                className="bg-canvas-inset/40 border border-warm/30 rounded-md px-3 py-2"
              >
                <div className="text-[10px] uppercase tracking-[0.22em] font-mono text-warm">
                  #{p.id} · {p.label}
                </div>
                <div className="font-mono text-sm text-fg-default mt-0.5">
                  {p.blobs} blob{p.blobs === 1 ? "" : "s"}
                </div>
                <div className="font-mono text-[11px] text-fg-muted">
                  {p.parts} part{p.parts === 1 ? "" : "s"} after split
                </div>
              </li>
            ))}
          </ul>
        </Section>
      ) : (
        <Section title="prisms" subtitle="format-aware decomposers that fired">
          <div className="px-4 py-3 font-mono text-xs text-fg-muted">
            no prism accepted any blob in this repo — every object is at Tier 0
            (verbatim CDC + zstd).
          </div>
        </Section>
      )}
    </Frame>
  );
}

function Frame({ children }: { children: React.ReactNode }) {
  return (
    <section className="bg-canvas-subtle border border-border-default rounded-lg overflow-hidden">
      <header className="px-4 py-2.5 bg-canvas-inset/40 border-b border-border-default flex items-center gap-3">
        <span className="text-[10px] uppercase tracking-[0.22em] font-mono text-warm">
          alt-only
        </span>
        <span className="text-sm text-fg-default">Storage footprint</span>
        <span className="ml-auto text-[10px] uppercase tracking-[0.22em] font-mono text-fg-subtle">
          git: a single pack file
        </span>
      </header>
      {children}
    </section>
  );
}

function Section({
  title,
  subtitle,
  children,
}: {
  title: string;
  subtitle: string;
  children: React.ReactNode;
}) {
  return (
    <div className="border-b border-border-default last:border-b-0">
      <div className="px-4 pt-3 pb-1 flex items-baseline gap-3">
        <span className="text-[10px] uppercase tracking-[0.22em] font-mono text-fg-subtle">
          {title}
        </span>
        <span className="text-[11px] font-mono text-fg-muted">{subtitle}</span>
      </div>
      {children}
    </div>
  );
}

interface BarSegment {
  label: string;
  value: number;
  tone: "warm" | "accent" | "dim";
  detail?: string;
}

function Bar({ segments, unit }: { segments: BarSegment[]; unit: string }) {
  const total = segments.reduce((acc, s) => acc + s.value, 0);
  return (
    <div className="px-4 pb-3 pt-2">
      <div className="flex w-full h-4 rounded-md overflow-hidden bg-canvas-inset border border-border-muted">
        {segments.map((seg, i) => {
          const pct = total > 0 ? (seg.value / total) * 100 : 0;
          if (pct <= 0) return null;
          const bg =
            seg.tone === "warm"
              ? "bg-warm"
              : seg.tone === "accent"
                ? "bg-accent"
                : "bg-border-muted";
          return (
            <div
              key={i}
              className={bg}
              style={{ width: `${pct}%` }}
              title={seg.label}
            />
          );
        })}
      </div>
      <ul className="mt-2 grid grid-cols-1 sm:grid-cols-3 gap-x-4 gap-y-1 text-[11px] font-mono">
        {segments.map((seg, i) => {
          const pct = total > 0 ? (seg.value / total) * 100 : 0;
          const dot =
            seg.tone === "warm"
              ? "bg-warm"
              : seg.tone === "accent"
                ? "bg-accent"
                : "bg-border-muted";
          return (
            <li key={i} className="flex items-center gap-2">
              <span className={`shrink-0 w-2 h-2 rounded-sm ${dot}`} />
              <span className="text-fg-muted truncate">{seg.label}</span>
              <span className="ml-auto text-fg-default tabular-nums">
                {pct.toFixed(0)}%
              </span>
            </li>
          );
        })}
      </ul>
      {segments.some((s) => s.detail) ? (
        <ul className="mt-1 text-[10px] font-mono text-fg-subtle space-y-0.5">
          {segments.map((s, i) =>
            s.detail ? <li key={i}>{s.detail}</li> : null,
          )}
          <li className="italic">unit: {unit}</li>
        </ul>
      ) : null}
    </div>
  );
}
