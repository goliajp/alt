# alt

A version control system written in pure Rust. Native handling for large and
binary files, byte-exact import/export of git repositories, and a built-in
HTTP server that speaks the git smart-protocol so you can keep using existing
hosts (GitHub, self-hosted, anything that talks git).

> Status: pre-1.0, in active dogfood. The on-disk format is stabilising. No
> releases yet — install from source.

## Why

git is the universal lingua franca, but it shows its age on a few axes:

- **Big and binary files** balloon the pack on every change.
- **The CLI surface** has decades of porcelain/plumbing accretion.
- **The on-disk format** wasn't designed for content-defined chunking, hash
  agility, or pluggable encodings.

`alt` keeps git's wire format and history model — so you can fetch from and
push to any git server — while rebuilding the store and CLI from first
principles in safe Rust, with no C dependencies in the default build.

## What works today

- **Repository read** — open any `.git` directory and read commits, trees,
  refs, the index, packs (including delta resolution).
- **Native store** — a `.alt` directory that holds BLAKE3-addressed,
  zstd-compressed chunks. Reproducible content-defined chunking gives
  cross-file and across-commit deduplication on the binary and large-file
  shapes git struggles with.
- **Import / export** — `alt import` ingests a `.git` directory into `.alt`;
  `alt export` rebuilds a byte-exact `.git`. Round-trip fidelity is tested
  against a multi-repo corpus.
- **Daily CLI** — `init`, `add`, `commit`, `status`, `branch`, `switch`,
  `diff`, `log`, `merge`, plus a built-in `git-flow` model (`alt flow
  feature start / finish`, release, hotfix) and an `undo` for the last
  operation.
- **Wire protocol** — `alt clone`, `fetch`, `push` against any git
  smart-HTTP v2 server. Authentication via Basic auth tokens.
- **Server** — `altd-server` is a self-contained HTTP server that hosts
  `.alt` repositories over the same git smart-protocol, so a stock `git`
  (or `alt`) client can clone and push to it.
- **Per-actor signing and policy** — Ed25519 signatures on push and on
  commit, plus a `.alt/policy` file that gates writes by principal
  (read-only, force-deny, branch and path allow-lists).
- **Structured diff** — line, binary, and format-aware (PNG perceptual
  fingerprinting, ZIP/OOXML, JSON, TOML).
- **Operation log** — every state change is recorded; `alt undo` rolls back
  the last one. Tamper-evident via signature chain.

## Install

```sh
git clone https://github.com/goliajp/alt.git
cd alt
cargo install --path crates/alt-cli --bin alt --bin altd --bin altd-server
```

Requires a recent stable Rust toolchain. No system C libraries needed.

## Quick start

```sh
# fresh project
alt init my-project
cd my-project
echo "hello" > a.txt
alt add a.txt
alt commit -m "first"
alt log

# work with an existing git repo
alt clone https://github.com/some/repo.git
cd repo
alt status
alt diff

# git-flow built in
alt flow init
alt flow feature start newthing
# ...edit...
alt commit -am "wip"
alt flow feature finish newthing
```

## Compatibility with git

Refs, commits, trees, blobs, and tags use the same SHA-1 object IDs git
does, so:

- Pushing from `alt` to a git server is byte-exact.
- Cloning a git server into `alt` and exporting back to `.git` gives the
  same object graph.
- A `.alt` repository can be exported to `.git` at any time and used by
  stock `git` — `alt export` is the escape hatch.

`alt` does not coexist with `.git` in the same working tree by design:
running `alt init` in a `.git`-managed directory is rejected. Use
`alt import .` to migrate, or work in separate directories.

## Server

`altd-server` is a single binary. Pointed at a directory of `.alt`
repositories, it serves them over git smart-HTTP v2.

```sh
altd-server --bind 127.0.0.1:8080 --root /srv/repos
```

Front it with a reverse proxy (nginx, Caddy, …) for TLS — the server is
plain HTTP and assumes the proxy terminates TLS. Authentication is HTTP
Basic with per-principal tokens; the `.alt/policy` file in each repo gates
what each principal may do.

## Status and roadmap

`alt` is in dogfood. The CLI surface and on-disk format are stable enough
for the author's daily use, but neither has a versioned compatibility
promise yet. There are no binary releases — install from source. A 1.0 will
freeze the on-disk format and publish binary releases through normal
channels.

## License

Dual-licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

## Contributing

The project is solo-maintained. Bug reports and feature requests via GitHub
issues are welcome; pull requests are not currently accepted. If you want
to use a part of `alt` as a library in your own project, the workspace
crates (`alt-store`, `alt-git-pack`, `alt-wire`, `alt-diff`, …) are
self-contained and each has its own purpose described in their rustdoc.
