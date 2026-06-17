import { Link, NavLink, Outlet } from "react-router";
import { useVersion } from "../lib/hooks";

export function Layout() {
  const version = useVersion();
  const buildLabel = version.data
    ? `${version.data.version}/${version.data.build}`
    : "0.0.0/dev";

  return (
    <div className="min-h-screen flex flex-col">
      <header className="sticky top-0 z-30 bg-canvas/85 backdrop-blur-md border-b border-border-default">
        <div className="max-w-7xl mx-auto px-6 h-14 flex items-center gap-6">
          <Link to="/" className="flex items-baseline gap-2 group">
            <span className="font-mono text-2xl font-bold tracking-tight text-fg-default group-hover:text-warm transition-colors">
              alt
            </span>
            <span className="hidden sm:inline text-[10px] font-mono uppercase tracking-[0.3em] text-fg-subtle">
              .golia.jp
            </span>
          </Link>

          <div className="hidden md:flex flex-1 max-w-md">
            <div className="relative w-full">
              <input
                type="text"
                placeholder="Search repositories…"
                disabled
                className="w-full bg-canvas-inset border border-border-default rounded-md px-3 py-1.5 text-sm font-mono placeholder:text-fg-subtle focus:outline-none focus:border-accent disabled:cursor-not-allowed"
              />
              <span className="absolute right-2 top-1/2 -translate-y-1/2 text-[10px] font-mono uppercase tracking-[0.15em] text-fg-subtle border border-border-muted rounded px-1.5 py-0.5">
                soon
              </span>
            </div>
          </div>

          <nav className="ml-auto flex items-center gap-1 text-sm">
            <NavLink
              to="/r/alt"
              className={({ isActive }) =>
                `px-3 py-1.5 rounded-md transition-colors ${
                  isActive
                    ? "text-fg-default bg-canvas-subtle"
                    : "text-fg-muted hover:text-fg-default hover:bg-canvas-subtle"
                }`
              }
            >
              Browse
            </NavLink>
            <a
              href="https://github.com/goliajp/alt"
              target="_blank"
              rel="noreferrer noopener"
              className="px-3 py-1.5 rounded-md text-fg-muted hover:text-fg-default hover:bg-canvas-subtle transition-colors"
            >
              Source
            </a>
            <a
              href="https://github.com/goliajp/alt/issues"
              target="_blank"
              rel="noreferrer noopener"
              className="px-3 py-1.5 rounded-md text-fg-muted hover:text-fg-default hover:bg-canvas-subtle transition-colors"
            >
              Issues
            </a>
          </nav>
        </div>
      </header>

      <main className="flex-1">
        <Outlet />
      </main>

      <footer className="border-t border-border-default mt-24">
        <div className="max-w-7xl mx-auto px-6 py-8 flex flex-col sm:flex-row gap-4 items-start sm:items-center justify-between text-xs">
          <div className="flex items-center gap-3 font-mono text-fg-muted">
            <span className="text-fg-default font-semibold">alt</span>
            <span className="text-fg-subtle">·</span>
            <span>{buildLabel}</span>
            <span className="text-fg-subtle">·</span>
            <span>MIT or Apache-2.0</span>
          </div>
          <div className="flex items-center gap-4 text-fg-muted">
            <a
              href="https://github.com/goliajp/alt"
              className="hover:text-fg-default transition-colors"
              target="_blank"
              rel="noreferrer noopener"
            >
              GitHub
            </a>
            <a
              href="https://github.com/goliajp/alt/blob/develop/LICENSE-MIT"
              className="hover:text-fg-default transition-colors"
              target="_blank"
              rel="noreferrer noopener"
            >
              License
            </a>
          </div>
        </div>
      </footer>
    </div>
  );
}
