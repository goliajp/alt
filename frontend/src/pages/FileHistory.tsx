import { Link, useParams, useSearchParams } from "react-router";
import { useFileHistory } from "../lib/hooks";
import { formatRelative } from "../lib/format";

/** /r/:name/history?path=… — every commit that touched a file. */
export function FileHistory() {
  const { name = "" } = useParams();
  const [params] = useSearchParams();
  const path = params.get("path") ?? "";
  const ref = params.get("ref") ?? undefined;
  const history = useFileHistory(name, { path, ref, n: 100 });

  const segments = path.split("/").filter(Boolean);

  return (
    <div className="px-6 py-10">
      <header className="border-b border-border-default pb-6 mb-7">
        <div className="flex items-center gap-2 flex-wrap text-sm mb-3">
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
          <span className="text-fg-subtle">/</span>
          <span className="text-fg-muted">history</span>
        </div>
        <h1 className="text-xl font-semibold text-fg-default tracking-tight mb-2">
          History of{" "}
          <span className="font-mono text-warm">
            {segments.length > 0 ? segments.join("/") : "(no path)"}
          </span>
        </h1>
        <p className="text-sm text-fg-muted">
          Every commit that added, changed, or removed this file. The same
          commit may touch other paths too — click an entry to jump to the
          full commit diff.
        </p>
      </header>

      {history.isLoading ? (
        <div className="font-mono text-sm text-fg-muted">loading…</div>
      ) : history.isError ? (
        <div className="font-mono text-sm text-danger">
          {(history.error as Error).message}
        </div>
      ) : history.data?.commits.length === 0 ? (
        <div className="font-mono text-sm text-fg-muted">
          no commits matched this path
        </div>
      ) : (
        <ol className="border border-border-default rounded-lg overflow-hidden divide-y divide-border-muted">
          {history.data?.commits.map((c) => (
            <li
              key={c.oid + c.change}
              className="bg-canvas-subtle hover:bg-canvas-inset/60 transition-colors"
            >
              <Link
                to={`/r/${name}/commits/${c.oid}`}
                className="flex items-start gap-4 px-5 py-4 group"
              >
                <ChangeMarker change={c.change} />
                <div className="flex-1 min-w-0">
                  <div className="text-fg-default group-hover:text-warm line-clamp-2">
                    {c.subject || "(no subject)"}
                  </div>
                  <div className="mt-1.5 flex items-center gap-2 text-xs font-mono text-fg-muted">
                    <span>{c.author.name || "unknown"}</span>
                    <span className="text-fg-subtle">·</span>
                    <span>{formatRelative(c.author.when)}</span>
                    <span className="text-fg-subtle">·</span>
                    <span className="uppercase tracking-[0.18em] text-fg-subtle">
                      {c.change}
                    </span>
                  </div>
                </div>
                <span className="shrink-0 font-mono text-xs text-fg-muted bg-canvas-inset border border-border-muted rounded px-2 py-1 group-hover:border-warm/60 group-hover:text-warm transition-colors">
                  {c.oid.slice(0, 7)}
                </span>
              </Link>
            </li>
          ))}
        </ol>
      )}
    </div>
  );
}

function ChangeMarker({ change }: { change: "added" | "changed" | "removed" }) {
  const cls =
    change === "added"
      ? "bg-diff-add-bg text-diff-add-fg"
      : change === "removed"
        ? "bg-diff-del-bg text-diff-del-fg"
        : "bg-canvas-inset text-warm";
  const sym = change === "added" ? "+" : change === "removed" ? "−" : "~";
  return (
    <span
      className={`shrink-0 w-6 h-6 rounded font-mono text-sm flex items-center justify-center ${cls}`}
    >
      {sym}
    </span>
  );
}
