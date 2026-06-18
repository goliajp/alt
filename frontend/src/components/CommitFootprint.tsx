import { useCommitFootprint } from "../lib/hooks";
import { formatBytes } from "../lib/format";

/** "What did this commit cost alt." Sits at the top of a commit detail
 *  page: a copper headline of net-new bytes, a small "shared with parent"
 *  number for the reuse story, and a per-file breakdown showing CDC +
 *  prism dedup at work. */
export function CommitFootprint({ repo, oid }: { repo: string; oid: string }) {
  const q = useCommitFootprint(repo, oid);
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
  const f = q.data!;
  const { totals, files } = f;
  const totalBytes = totals.net_new_bytes + totals.shared_bytes;
  const reusePct =
    totalBytes > 0 ? (totals.shared_bytes / totalBytes) * 100 : 0;

  if (files.length === 0) {
    return (
      <Frame>
        <div className="px-4 py-3 font-mono text-xs text-fg-muted">
          no per-file footprint — this commit didn't change any tree
          contents (merge with empty diff, or root commit with no parent
          comparison).
        </div>
      </Frame>
    );
  }

  return (
    <Frame>
      <div className="px-5 py-4 grid grid-cols-1 md:grid-cols-[auto_1fr] gap-6 items-center border-b border-border-default">
        <div>
          <div className="text-[10px] uppercase tracking-[0.22em] font-mono text-warm mb-1">
            net new on disk
          </div>
          <div className="font-mono">
            <span className="text-3xl text-warm tabular-nums font-medium">
              {formatBytes(totals.net_new_bytes)}
            </span>
            <span className="text-sm text-fg-muted ml-2">
              · {totals.net_new_chunks} chunk
              {totals.net_new_chunks === 1 ? "" : "s"}
            </span>
          </div>
          {totals.shared_bytes > 0 ? (
            <div className="text-xs font-mono text-fg-muted mt-1">
              reused {formatBytes(totals.shared_bytes)} from parent (
              {totals.shared_chunks} chunk
              {totals.shared_chunks === 1 ? "" : "s"})
            </div>
          ) : (
            <div className="text-xs font-mono text-fg-subtle mt-1">
              nothing shared with parent
            </div>
          )}
        </div>
        <div className="text-xs font-mono text-fg-muted leading-relaxed border-l border-border-muted pl-6 md:max-w-md">
          {f.parent ? (
            <p>
              CDC chunked every changed blob, kept{" "}
              <strong className="text-fg-default">{reusePct.toFixed(0)}%</strong>{" "}
              of the bytes by content-address — git pack would re-encode
              each file fresh.
            </p>
          ) : (
            <p>Root commit; no parent to share with — every chunk is new.</p>
          )}
        </div>
      </div>

      {/* Stacked bar */}
      {totalBytes > 0 ? (
        <div className="px-5 py-3 border-b border-border-default">
          <div className="flex h-3 rounded-md overflow-hidden bg-canvas-inset border border-border-muted">
            <div
              className="bg-warm"
              style={{
                width: `${(totals.net_new_bytes / totalBytes) * 100}%`,
              }}
              title="net new"
            />
            <div
              className="bg-diff-add-fg/30"
              style={{
                width: `${(totals.shared_bytes / totalBytes) * 100}%`,
              }}
              title="shared with parent"
            />
          </div>
          <div className="mt-1.5 grid grid-cols-2 text-[11px] font-mono">
            <span>
              <span className="inline-block w-2 h-2 bg-warm rounded-sm mr-1.5" />
              <span className="text-fg-muted">net new</span>
              <span className="text-fg-default tabular-nums float-right">
                {formatBytes(totals.net_new_bytes)}
              </span>
            </span>
            <span className="pl-4">
              <span className="inline-block w-2 h-2 bg-diff-add-fg/40 rounded-sm mr-1.5" />
              <span className="text-fg-muted">shared</span>
              <span className="text-fg-default tabular-nums float-right">
                {formatBytes(totals.shared_bytes)}
              </span>
            </span>
          </div>
        </div>
      ) : null}

      <ul className="divide-y divide-border-muted">
        {files.map((f) => (
          <FileRow key={f.path + f.change} f={f} />
        ))}
      </ul>
    </Frame>
  );
}

function FileRow({
  f,
}: {
  f: import("../lib/api").CommitFootprintFile;
}) {
  const fileTotal = f.net_new_bytes + f.shared_bytes;
  const reusedPct = fileTotal > 0 ? (f.shared_bytes / fileTotal) * 100 : 0;

  const changeChip =
    f.change === "added"
      ? "bg-diff-add-bg/30 text-diff-add-fg"
      : f.change === "removed"
        ? "bg-diff-del-bg/30 text-diff-del-fg"
        : "bg-canvas-inset/40 text-warm";
  const changeSym =
    f.change === "added" ? "+" : f.change === "removed" ? "−" : "~";

  return (
    <li className="px-5 py-2.5 grid grid-cols-[auto_1fr_auto_auto] gap-3 items-center">
      <span
        className={`shrink-0 w-5 h-5 rounded font-mono text-xs flex items-center justify-center ${changeChip}`}
      >
        {changeSym}
      </span>
      <div className="min-w-0">
        <div className="font-mono text-sm text-fg-default truncate">
          {f.path}
        </div>
        <div className="text-[11px] font-mono text-fg-muted">
          {f.new_chunks} chunk{f.new_chunks === 1 ? "" : "s"}
          {f.shared_chunks > 0 ? (
            <>
              {" "}
              ·{" "}
              <span className="text-diff-add-fg">{f.shared_chunks} reused</span>
            </>
          ) : null}
          {f.net_new_chunks > 0 ? (
            <>
              {" "}
              ·{" "}
              <span className="text-warm">{f.net_new_chunks} new</span>
            </>
          ) : null}
        </div>
      </div>
      {fileTotal > 0 ? (
        <div className="hidden sm:flex flex-col w-32 gap-1">
          <div className="h-1.5 rounded-sm overflow-hidden bg-canvas-inset border border-border-muted">
            <div
              className="bg-warm h-full"
              style={{ width: `${100 - reusedPct}%` }}
            />
          </div>
          <div className="text-[10px] font-mono text-fg-subtle text-right tabular-nums">
            {reusedPct.toFixed(0)}% reused
          </div>
        </div>
      ) : (
        <span />
      )}
      <span className="font-mono text-xs text-warm tabular-nums">
        +{formatBytes(f.net_new_bytes)}
      </span>
    </li>
  );
}

function Frame({ children }: { children: React.ReactNode }) {
  return (
    <section className="bg-canvas-subtle border border-border-default rounded-lg overflow-hidden">
      <header className="px-4 py-2.5 bg-canvas-inset/40 border-b border-border-default flex items-center gap-3">
        <span className="text-[10px] uppercase tracking-[0.22em] font-mono text-warm">
          alt-only
        </span>
        <span className="text-sm text-fg-default">Commit footprint</span>
        <span className="ml-auto text-[10px] uppercase tracking-[0.22em] font-mono text-fg-subtle">
          git: pack object count
        </span>
      </header>
      {children}
    </section>
  );
}
