import { Link, useParams } from "react-router";
import { useLog, useRepo, useTree } from "../lib/hooks";
import { CodeBlock } from "../components/CodeBlock";
import { formatRelative } from "../lib/format";

export function RepoHome() {
  const { name = "" } = useParams();
  const repo = useRepo(name);
  const branch = repo.data?.repo.head_branch || "HEAD";
  const tree = useTree(name, branch);
  const log = useLog(name, { n: 10 });

  return (
    <div className="max-w-7xl mx-auto px-6 py-10">
      {/* Header */}
      <header className="border-b border-border-default pb-6">
        <div className="flex items-center gap-2 text-sm mb-3">
          <Link to="/" className="text-fg-muted hover:text-fg-default">
            alt.golia.jp
          </Link>
          <span className="text-fg-subtle">/</span>
          <h1 className="font-mono text-2xl font-semibold text-fg-default tracking-tight">
            {name}
          </h1>
        </div>
        {repo.data?.repo ? (
          <div className="flex items-center flex-wrap gap-3 text-xs font-mono">
            <span className="inline-flex items-center gap-1.5 bg-canvas-subtle border border-border-default rounded-full px-2.5 py-1">
              <span className="w-1.5 h-1.5 rounded-full bg-success"></span>
              {branch}
            </span>
            <span className="text-fg-muted">
              {repo.data.repo.head.slice(0, 12)}
            </span>
            <span className="text-fg-subtle">·</span>
            <span className="text-fg-muted">{repo.data.repo.refs} refs</span>
            <Link
              to={`/r/${name}/commits`}
              className="text-accent hover:underline ml-auto"
            >
              History →
            </Link>
          </div>
        ) : repo.isLoading ? (
          <div className="text-xs font-mono text-fg-muted">…</div>
        ) : (
          <div className="text-xs font-mono text-danger">
            {(repo.error as Error)?.message ?? "unknown error"}
          </div>
        )}
      </header>

      <div className="grid grid-cols-1 lg:grid-cols-3 gap-8 mt-8">
        {/* Tree */}
        <section className="lg:col-span-2">
          <div className="flex items-center justify-between mb-3">
            <h2 className="text-[11px] font-mono uppercase tracking-[0.22em] text-fg-muted">
              Source / {branch}
            </h2>
            <Link
              to={`/r/${name}/tree/${branch}`}
              className="text-sm text-accent hover:underline"
            >
              Browse files →
            </Link>
          </div>
          <div className="bg-canvas-subtle border border-border-default rounded-lg overflow-hidden">
            {tree.isLoading ? (
              <div className="p-4 font-mono text-sm text-fg-muted">
                loading…
              </div>
            ) : tree.isError ? (
              <div className="p-4 font-mono text-sm text-danger">
                {(tree.error as Error).message}
              </div>
            ) : (
              <table className="w-full text-sm">
                <tbody>
                  {tree.data?.entries
                    .slice()
                    .sort((a, b) => {
                      const ak = a.kind === "tree" ? 0 : 1;
                      const bk = b.kind === "tree" ? 0 : 1;
                      if (ak !== bk) return ak - bk;
                      return a.name.localeCompare(b.name);
                    })
                    .map((e) => (
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
                            to={`/r/${name}/tree/${branch}/${e.name}`}
                            className="font-mono text-fg-default hover:text-accent"
                          >
                            {e.name}
                          </Link>
                        </td>
                        <td className="py-2 pr-4 text-right font-mono text-[11px] text-fg-subtle">
                          {e.mode}
                        </td>
                      </tr>
                    ))}
                </tbody>
              </table>
            )}
          </div>
        </section>

        {/* Side: clone + recent commits */}
        <aside className="space-y-8">
          <section>
            <div className="text-[11px] font-mono uppercase tracking-[0.22em] text-fg-muted mb-3">
              Clone (alt)
            </div>
            <CodeBlock className="text-xs">
              {`alt clone https://alt.golia.jp/${name}`}
            </CodeBlock>
            <div className="text-[11px] font-mono uppercase tracking-[0.22em] text-fg-muted mt-5 mb-3">
              Clone (git)
            </div>
            <CodeBlock className="text-xs">
              {`git clone https://alt.golia.jp/${name}`}
            </CodeBlock>
          </section>

          <section>
            <div className="flex items-center justify-between mb-3">
              <div className="text-[11px] font-mono uppercase tracking-[0.22em] text-fg-muted">
                Recent commits
              </div>
              <Link
                to={`/r/${name}/commits`}
                className="text-xs text-accent hover:underline"
              >
                Log →
              </Link>
            </div>
            {log.isLoading ? (
              <div className="font-mono text-sm text-fg-muted">loading…</div>
            ) : log.isError ? (
              <div className="font-mono text-sm text-danger">
                {(log.error as Error).message}
              </div>
            ) : (
              <ul className="space-y-1.5">
                {log.data?.commits.map((c) => (
                  <li key={c.oid}>
                    <Link
                      to={`/r/${name}/commits/${c.oid}`}
                      className="block bg-canvas-subtle border border-border-default rounded-md px-3 py-2.5 hover:border-warm/70 transition-colors group"
                    >
                      <div className="text-sm text-fg-default group-hover:text-warm line-clamp-2">
                        {c.subject || "(no subject)"}
                      </div>
                      <div className="mt-1 flex items-center gap-2 text-[11px] font-mono text-fg-subtle">
                        <span>{c.oid.slice(0, 7)}</span>
                        <span>·</span>
                        <span>{c.author.name || "unknown"}</span>
                        <span>·</span>
                        <span>{formatRelative(c.author.when)}</span>
                      </div>
                    </Link>
                  </li>
                ))}
              </ul>
            )}
          </section>
        </aside>
      </div>
    </div>
  );
}
