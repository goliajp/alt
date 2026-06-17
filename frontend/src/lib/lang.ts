/* Map a file path / name to a Shiki language id. Returns null for
   unknown extensions; the highlighter falls back to plaintext then. */

const BY_EXT: Record<string, string> = {
  ts: "typescript",
  tsx: "tsx",
  js: "javascript",
  jsx: "jsx",
  mjs: "javascript",
  cjs: "javascript",
  rs: "rust",
  toml: "toml",
  json: "json",
  jsonc: "jsonc",
  yaml: "yaml",
  yml: "yaml",
  md: "markdown",
  mdx: "mdx",
  html: "html",
  htm: "html",
  css: "css",
  scss: "scss",
  sh: "shell",
  bash: "shell",
  zsh: "shell",
  fish: "shell",
  py: "python",
  go: "go",
  c: "c",
  h: "c",
  cpp: "cpp",
  cc: "cpp",
  hpp: "cpp",
  java: "java",
  kt: "kotlin",
  swift: "swift",
  rb: "ruby",
  php: "php",
  sql: "sql",
  dockerfile: "docker",
  vue: "vue",
  svelte: "svelte",
  graphql: "graphql",
  gql: "graphql",
  proto: "proto",
  diff: "diff",
  patch: "diff",
  lock: "yaml",
};

const BY_NAME: Record<string, string> = {
  Dockerfile: "docker",
  ".gitignore": "ini",
  ".dockerignore": "ini",
  "Cargo.toml": "toml",
  "Cargo.lock": "toml",
  Makefile: "makefile",
};

/** Best-effort language id for shiki, given a path. `null` when we
 *  don't recognise the extension — the caller treats that as plaintext. */
export function detectLang(path: string): string | null {
  if (!path) return null;
  const base = path.split("/").pop() ?? path;
  if (BY_NAME[base]) return BY_NAME[base];
  const lowerBase = base.toLowerCase();
  if (BY_NAME[lowerBase]) return BY_NAME[lowerBase];
  const dot = base.lastIndexOf(".");
  if (dot < 0) {
    if (base === base.toLowerCase()) return null;
    return null;
  }
  const ext = base.slice(dot + 1).toLowerCase();
  return BY_EXT[ext] ?? null;
}

/** A small superset of languages we want preloaded in the highlighter. */
export const PRELOADED_LANGS = [
  "typescript",
  "tsx",
  "javascript",
  "jsx",
  "rust",
  "toml",
  "json",
  "yaml",
  "markdown",
  "html",
  "css",
  "shell",
  "python",
  "go",
  "c",
  "cpp",
  "docker",
  "ini",
  "makefile",
  "sql",
  "diff",
] as const;
