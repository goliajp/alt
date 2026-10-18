import { Link } from "react-router";
import { useRepos } from "../lib/hooks";
import { CodeBlock } from "../components/CodeBlock";

export function Home() {
  const repos = useRepos();

  return (
    <div className="hero-bg">
      {/* HERO */}
      <section className="px-6 pt-24 pb-28 relative">
        <div className="rise relative">
          <div className="inline-flex items-center gap-2 text-[11px] font-mono uppercase tracking-[0.22em] text-fg-muted border border-border-default bg-canvas-subtle/60 rounded-full px-3 py-1 mb-9">
            <span className="w-1.5 h-1.5 rounded-full bg-warm pulse-soft"></span>
            pre-1.0 · dogfood
          </div>

          <h1 className="font-mono text-6xl sm:text-[88px] font-bold tracking-tighter text-fg-default leading-[0.92] mb-6">
            <span className="cursor-blink">alt</span>
          </h1>

          <p className="text-2xl sm:text-[28px] leading-snug text-fg-default font-medium mb-5 tracking-tight">
            A version control system{" "}
            <span className="text-warm">rebuilt in pure Rust.</span>
          </p>
          <p className="text-base sm:text-lg text-fg-muted leading-relaxed mb-10">
            Native large- and binary-file handling. Git object and history
            preservation, reviewed tasks, and an independently verified Git exit.
            A built-in git smart-http v2 server for existing hosts.
          </p>

          <div className="flex flex-wrap gap-3 mb-14">
            <Link
              to="/r/alt"
              className="inline-flex items-center gap-2 bg-warm text-canvas-inset font-semibold px-5 py-2.5 rounded-md hover:bg-warm-deep transition-colors shadow-[0_0_0_1px_rgba(232,168,124,0.4)]"
            >
              Browse the source
              <span aria-hidden>→</span>
            </Link>
            <a
              href="https://github.com/goliajp/alt"
              target="_blank"
              rel="noreferrer noopener"
              className="inline-flex items-center gap-2 border border-border-default text-fg-default font-medium px-5 py-2.5 rounded-md hover:bg-canvas-subtle transition-colors"
            >
              View on GitHub
            </a>
          </div>

          <div>
            <div className="text-[11px] font-mono uppercase tracking-[0.22em] text-fg-subtle mb-3">
              Install from source
            </div>
            <CodeBlock className="text-[13px]">
              {`git clone https://github.com/goliajp/alt.git
cd alt
cargo install --locked --path crates/alt-cli \\
  --bin alt --bin altd --bin altd-server`}
            </CodeBlock>
          </div>
        </div>
      </section>

      {/* FEATURES */}
      <section className="border-y border-border-default bg-canvas-subtle/70">
        <div className="px-6 py-16 grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 gap-x-14 gap-y-10">
          <Feature
            tag="01"
            title="Native Rust tools"
            body="The CLI, local daemon, and HTTP server are separate executables. Internal packages target macOS arm64 and Linux x86_64; system runtime requirements are recorded with each build."
          />
          <Feature
            tag="02"
            title="Tasks with observed evidence"
            body="Bind selected source and validation inputs to an isolated run, inspect the full change, then review and integrate against the expected target."
          />
          <Feature
            tag="03"
            title="Git-flow + undo, first-class"
            body="alt flow feature start/finish and alt undo are recorded in a tamper-evident op log — not shell wrappers around git porcelain."
          />
          <Feature
            tag="04"
            title="Per-actor signing"
            body="Ed25519 signatures on pushes and commits, plus a .alt/policy file that gates writes per principal — read-only, force-deny, allow-lists."
          />
          <Feature
            tag="05"
            title="Format-aware diff"
            body="ZIP / OOXML, JSON, TOML, and perceptual PNG diffs out of the box. Large blob churn is no longer a wall of red and green."
          />
          <Feature
            tag="06"
            title="Git protocol and independent exit"
            body="Clone, fetch, and push use git smart-http v2. Reviewed delivery verifies canonical objects, refs, HEAD, configuration semantics, and files; pack layout and reflogs are outside that guarantee."
          />
        </div>
      </section>

      {/* REPOS */}
      <section className="px-6 py-20">
        <div className="flex items-end justify-between mb-7 border-b border-border-default pb-4">
          <div>
            <div className="text-[11px] font-mono uppercase tracking-[0.22em] text-fg-subtle mb-1">
              On this server
            </div>
            <h2 className="text-2xl font-semibold text-fg-default tracking-tight">
              Repositories
            </h2>
          </div>
          {repos.data?.repos ? (
            <span className="text-sm font-mono text-fg-muted">
              {repos.data.repos.length}
            </span>
          ) : null}
        </div>

        {repos.isLoading ? (
          <div className="text-sm font-mono text-fg-muted">loading…</div>
        ) : repos.isError ? (
          <div className="text-sm font-mono text-danger">
            {(repos.error as Error).message}
          </div>
        ) : (
          <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-4">
            {repos.data?.repos.map((r) => (
              <Link
                key={r.name}
                to={`/r/${r.name}`}
                className="group block bg-canvas-subtle border border-border-default rounded-lg p-5 hover:border-warm/70 transition-colors"
              >
                <div className="flex items-center justify-between mb-3">
                  <div className="font-mono text-lg font-semibold text-fg-default group-hover:text-warm transition-colors">
                    {r.name}
                  </div>
                  <span className="text-[10px] font-mono uppercase tracking-[0.18em] text-fg-subtle border border-border-muted rounded px-1.5 py-0.5">
                    public
                  </span>
                </div>
                <div className="flex items-center gap-2 text-xs font-mono text-fg-muted">
                  <span className="inline-flex items-center gap-1.5">
                    <span className="w-1.5 h-1.5 rounded-full bg-success"></span>
                    {r.head_branch || "—"}
                  </span>
                  <span className="text-fg-subtle">·</span>
                  <span>{r.head ? r.head.slice(0, 7) : "—"}</span>
                  <span className="text-fg-subtle">·</span>
                  <span>{r.refs} refs</span>
                </div>
              </Link>
            ))}
          </div>
        )}
      </section>
    </div>
  );
}

function Feature({
  tag,
  title,
  body,
}: {
  tag: string;
  title: string;
  body: string;
}) {
  return (
    <div>
      <div className="flex items-baseline gap-3 mb-2">
        <span className="font-mono text-warm text-xs tracking-[0.15em]">
          {tag}
        </span>
        <h3 className="text-base font-semibold text-fg-default tracking-tight">
          {title}
        </h3>
      </div>
      <p className="text-sm text-fg-muted leading-relaxed">{body}</p>
    </div>
  );
}
