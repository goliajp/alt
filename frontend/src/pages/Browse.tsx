import { Link, useParams } from "react-router";
import { useQuery } from "@tanstack/react-query";
import { api, type Blob, type TreeEntry, type TreeEntryKind } from "../lib/api";
import { useFileHistory } from "../lib/hooks";
import { formatBytes, formatRelative } from "../lib/format";
import { detectLang } from "../lib/lang";
import { SyntaxBlock } from "../components/SyntaxBlock";
import { ImagePreview } from "../components/ImagePreview";
import { isImagePath, isRasterImagePath } from "../lib/image";

type Result =
  | { kind: "tree"; tree: { oid: string; entries: TreeEntry[] }; trail: Crumb[] }
  | { kind: "blob"; blob: Blob; trail: Crumb[] }
  | { kind: "submodule"; oid: string; trail: Crumb[] };

interface Crumb {
  name: string;
  oid: string;
  kind: TreeEntryKind;
}

function useBrowsePath(name: string, spec: string, path: string[]) {
  return useQuery({
    queryKey: ["browse", name, spec, path] as const,
    queryFn: async (): Promise<Result> => {
      let current = await api.tree(name, spec);
      const trail: Crumb[] = [];
      for (const seg of path) {
        const entry = current.entries.find((e) => e.name === seg);
        if (!entry) throw new Error(`path segment not found: ${seg}`);
        trail.push({ name: seg, oid: entry.oid, kind: entry.kind });
        if (entry.kind === "blob") {
          const blob = await api.blob(name, entry.oid);
          return { kind: "blob", blob, trail };
        }
        if (entry.kind === "commit") {
          return { kind: "submodule", oid: entry.oid, trail };
        }
        current = await api.tree(name, entry.oid);
      }
      return { kind: "tree", tree: current, trail };
    },
    enabled: !!name && !!spec,
  });
}

export function Browse() {
  const params = useParams();
  const name = params.name ?? "";
  const spec = params.spec ?? "";
  const splat = (params["*"] ?? "").trim();
  const path = splat ? splat.split("/").filter(Boolean) : [];

  const browse = useBrowsePath(name, spec, path);

  return (
    <div className="px-6 py-10">
      <header className="border-b border-border-default pb-5 mb-6">
        <div className="flex items-center gap-2 flex-wrap text-sm">
          <Link to="/" className="text-fg-muted hover:text-fg-default">
            alt.golia.jp
          </Link>
          <span className="text-fg-subtle">/</span>
          <Link
            to={`/r/${name}`}
            className="font-mono text-fg-default hover:text-warm"
          >
            {name}
          </Link>
          <span className="text-fg-subtle">@</span>
          <span className="font-mono text-warm">{spec}</span>
          <span className="text-fg-subtle">·</span>
          <Link
            to={`/r/${name}/tree/${spec}`}
            className="font-mono text-fg-muted hover:text-accent"
          >
            /
          </Link>
          {path.map((seg, i) => (
            <span key={i} className="inline-flex items-center gap-2">
              <Link
                to={`/r/${name}/tree/${spec}/${path.slice(0, i + 1).join("/")}`}
                className="font-mono text-fg-default hover:text-accent"
              >
                {seg}
              </Link>
              {i < path.length - 1 ? (
                <span className="text-fg-subtle">/</span>
              ) : null}
            </span>
          ))}
        </div>
      </header>

      {browse.isLoading ? (
        <div className="font-mono text-sm text-fg-muted">loading…</div>
      ) : browse.isError ? (
        <div className="font-mono text-sm text-danger">
          {(browse.error as Error).message}
        </div>
      ) : browse.data?.kind === "blob" ? (
        <BlobView
          repo={name}
          blob={browse.data.blob}
          fullPath={path.join("/")}
          fileName={path[path.length - 1] ?? ""}
          spec={spec}
          historyHref={`/r/${name}/history?path=${encodeURIComponent(path.join("/"))}&ref=${encodeURIComponent(spec)}`}
        />
      ) : browse.data?.kind === "tree" ? (
        <TreeView
          name={name}
          spec={spec}
          path={path}
          entries={browse.data.tree.entries}
        />
      ) : browse.data?.kind === "submodule" ? (
        <div className="bg-canvas-subtle border border-border-default rounded-lg p-6 font-mono text-sm text-fg-muted">
          submodule · {browse.data.oid.slice(0, 12)}
        </div>
      ) : null}
    </div>
  );
}

function TreeView({
  name,
  spec,
  path,
  entries,
}: {
  name: string;
  spec: string;
  path: string[];
  entries: TreeEntry[];
}) {
  const sorted = [...entries].sort((a, b) => {
    const ak = a.kind === "tree" ? 0 : 1;
    const bk = b.kind === "tree" ? 0 : 1;
    if (ak !== bk) return ak - bk;
    return a.name.localeCompare(b.name);
  });

  const parent = path.slice(0, -1).join("/");
  const parentHref = parent
    ? `/r/${name}/tree/${spec}/${parent}`
    : `/r/${name}/tree/${spec}`;

  return (
    <div className="bg-canvas-subtle border border-border-default rounded-lg overflow-hidden">
      <table className="w-full text-sm">
        <tbody>
          {path.length > 0 ? (
            <tr className="border-b border-border-muted hover:bg-canvas-inset/60 transition-colors">
              <td className="py-2 pl-4 pr-2 w-8 text-center text-fg-subtle">
                ↑
              </td>
              <td className="py-2">
                <Link
                  to={parentHref}
                  className="font-mono text-fg-muted hover:text-accent"
                >
                  ..
                </Link>
              </td>
              <td className="py-2 pr-4" />
            </tr>
          ) : null}
          {sorted.map((e) => {
            const childPath = [...path, e.name].join("/");
            return (
              <tr
                key={e.oid + e.name}
                className="border-t border-border-muted first:border-t-0 hover:bg-canvas-inset/60 transition-colors"
              >
                <td className="py-2 pl-4 pr-2 w-8 text-center">
                  {e.kind === "tree" ? (
                    <span className="text-accent">▸</span>
                  ) : e.kind === "commit" ? (
                    <span className="text-attention">◆</span>
                  ) : (
                    <span className="text-fg-subtle">·</span>
                  )}
                </td>
                <td className="py-2">
                  <Link
                    to={`/r/${name}/tree/${spec}/${childPath}`}
                    className="font-mono text-fg-default hover:text-accent"
                  >
                    {e.name}
                  </Link>
                </td>
                <td className="py-2 pr-4 text-right font-mono text-[11px] text-fg-subtle">
                  {e.mode}
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}

function BlobView({
  repo,
  blob,
  fileName,
  fullPath,
  spec,
  historyHref,
}: {
  repo: string;
  blob: Blob;
  fileName: string;
  fullPath: string;
  spec: string;
  historyHref: string;
}) {
  const lang = detectLang(fileName);
  const showImage = isImagePath(fileName);
  const rawSrc = `/api/repos/${encodeURIComponent(repo)}/blob/${blob.oid}/raw`;

  return (
    <div className="grid grid-cols-1 lg:grid-cols-[minmax(0,1fr)_320px] gap-6">
      <div className="bg-canvas-subtle border border-border-default rounded-lg overflow-hidden">
        <div className="flex items-center justify-between gap-3 border-b border-border-default px-4 py-2.5 bg-canvas-inset/40">
          <div className="flex items-center gap-3 text-xs font-mono text-fg-muted flex-wrap">
            <span>{blob.oid.slice(0, 12)}</span>
            <span>·</span>
            <span>{formatBytes(blob.size)}</span>
            {lang ? (
              <>
                <span>·</span>
                <span className="text-fg-default">{lang}</span>
              </>
            ) : null}
            {blob.binary && !showImage ? (
              <>
                <span>·</span>
                <span className="text-attention">binary</span>
              </>
            ) : null}
            <span>·</span>
            <a
              href={rawSrc}
              target="_blank"
              rel="noreferrer noopener"
              className="text-accent hover:underline"
            >
              raw
            </a>
          </div>
          <Link
            to={historyHref}
            className="text-xs font-mono text-accent hover:underline shrink-0"
          >
            History →
          </Link>
        </div>
        {showImage ? (
          <ImagePreview src={rawSrc} alt={fileName} />
        ) : blob.binary || blob.content == null ? (
          <div className="p-12 text-center font-mono text-sm text-fg-muted">
            Binary file — preview not available.
          </div>
        ) : (
          <SyntaxBlock code={blob.content} lang={lang} lineNumbers />
        )}
      </div>

      {fullPath ? (
        <FileVersionRail repo={repo} path={fullPath} spec={spec} fileName={fileName} />
      ) : null}
    </div>
  );
}

function FileVersionRail({
  repo,
  path,
  spec,
  fileName,
}: {
  repo: string;
  path: string;
  spec: string;
  fileName: string;
}) {
  const history = useFileHistory(repo, { path, ref: spec, n: 20 });
  const isRaster = isRasterImagePath(fileName);

  return (
    <aside className="bg-canvas-subtle border border-border-default rounded-lg overflow-hidden h-fit lg:sticky lg:top-20">
      <div className="px-4 py-2.5 border-b border-border-default bg-canvas-inset/40 text-[11px] uppercase tracking-[0.22em] font-mono text-fg-muted">
        Versions
      </div>
      {history.isLoading ? (
        <div className="p-4 font-mono text-xs text-fg-muted">loading…</div>
      ) : history.isError ? (
        <div className="p-4 font-mono text-xs text-danger">
          {(history.error as Error).message}
        </div>
      ) : history.data?.commits.length === 0 ? (
        <div className="p-4 font-mono text-xs text-fg-muted">
          no history for this path
        </div>
      ) : (
        <ol className="divide-y divide-border-muted">
          {history.data?.commits.map((c) => {
            const thumbOid = c.change === "removed" ? c.old_oid : c.new_oid;
            return (
              <li key={c.oid + c.change} className="hover:bg-canvas-inset/40 transition-colors">
                <Link
                  to={`/r/${repo}/commits/${c.oid}`}
                  className="flex gap-3 px-4 py-3 group items-start"
                >
                  {isRaster && thumbOid ? (
                    <img
                      src={`/api/repos/${encodeURIComponent(repo)}/blob/${thumbOid}/raw`}
                      alt={`${fileName} @ ${c.oid.slice(0, 7)}`}
                      loading="lazy"
                      className="w-12 h-12 object-contain rounded bg-canvas-inset border border-border-muted shrink-0"
                    />
                  ) : (
                    <span className="w-6 h-6 mt-1 rounded font-mono text-xs flex items-center justify-center bg-canvas-inset text-warm shrink-0">
                      {c.change === "added" ? "+" : c.change === "removed" ? "−" : "~"}
                    </span>
                  )}
                  <div className="min-w-0 flex-1">
                    <div className="text-xs text-fg-default group-hover:text-warm line-clamp-2">
                      {c.subject || "(no subject)"}
                    </div>
                    <div className="mt-1 flex items-center gap-1.5 text-[10px] font-mono text-fg-subtle">
                      <span>{c.oid.slice(0, 7)}</span>
                      <span>·</span>
                      <span>{formatRelative(c.author.when)}</span>
                    </div>
                  </div>
                </Link>
              </li>
            );
          })}
        </ol>
      )}
    </aside>
  );
}
