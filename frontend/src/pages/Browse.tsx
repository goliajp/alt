import { Link, useParams } from "react-router";
import { useQuery } from "@tanstack/react-query";
import { api, type Blob, type TreeEntry, type TreeEntryKind } from "../lib/api";
import { formatBytes } from "../lib/format";
import { detectLang } from "../lib/lang";
import { SyntaxBlock } from "../components/SyntaxBlock";

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
        <BlobView blob={browse.data.blob} path={path[path.length - 1] ?? ""} />
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

function BlobView({ blob, path }: { blob: Blob; path: string }) {
  const lang = detectLang(path);
  return (
    <div className="bg-canvas-subtle border border-border-default rounded-lg overflow-hidden">
      <div className="flex items-center justify-between gap-3 border-b border-border-default px-4 py-2.5 bg-canvas-inset/40">
        <div className="flex items-center gap-3 text-xs font-mono text-fg-muted">
          <span>{blob.oid.slice(0, 12)}</span>
          <span>·</span>
          <span>{formatBytes(blob.size)}</span>
          {lang ? (
            <>
              <span>·</span>
              <span className="text-fg-default">{lang}</span>
            </>
          ) : null}
          {blob.binary ? (
            <>
              <span>·</span>
              <span className="text-attention">binary</span>
            </>
          ) : null}
        </div>
      </div>
      {blob.binary || blob.content == null ? (
        <div className="p-12 text-center font-mono text-sm text-fg-muted">
          Binary file — preview not available.
        </div>
      ) : (
        <SyntaxBlock code={blob.content} lang={lang} lineNumbers />
      )}
    </div>
  );
}
