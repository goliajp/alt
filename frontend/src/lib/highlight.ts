/* Shiki-backed code highlighter. Loads a single shared highlighter
   instance lazily (the language + theme registration is heavy, so we
   only pay for it once across the whole SPA), then exposes a tiny
   `highlight(code, lang)` that returns the per-line token spans we
   render ourselves so we can drop them next to a line-number gutter. */

import {
  createHighlighter,
  type Highlighter,
  type ThemedToken,
} from "shiki";
import { PRELOADED_LANGS } from "./lang";

const THEME = "github-dark-dimmed";

let highlighterPromise: Promise<Highlighter> | null = null;

function getHighlighter() {
  if (!highlighterPromise) {
    highlighterPromise = createHighlighter({
      themes: [THEME],
      langs: PRELOADED_LANGS.slice(),
    });
  }
  return highlighterPromise;
}

export interface HighlightedLine {
  tokens: ThemedToken[];
}

/** Highlight `code` as `lang`. `lang === null` or an unrecognised
 *  language returns plaintext spans (still one entry per line). */
export async function highlight(
  code: string,
  lang: string | null,
): Promise<HighlightedLine[]> {
  const h = await getHighlighter();
  const safeLang =
    lang && h.getLoadedLanguages().includes(lang)
      ? lang
      : ((await tryLoadLang(h, lang)) ?? "text");
  const result = h.codeToTokens(code, {
    lang: safeLang as never,
    theme: THEME,
  });
  return result.tokens.map((tokens) => ({ tokens }));
}

async function tryLoadLang(
  h: Highlighter,
  lang: string | null,
): Promise<string | null> {
  if (!lang) return null;
  try {
    await h.loadLanguage(lang as never);
    return lang;
  } catch {
    return null;
  }
}

/** Compose the inline style for a single shiki token. */
export function tokenStyle(token: ThemedToken): React.CSSProperties {
  const style: React.CSSProperties = { color: token.color };
  if (token.fontStyle != null) {
    // shiki encodes fontStyle as a bitmask: 1=italic, 2=bold, 4=underline
    if (token.fontStyle & 1) style.fontStyle = "italic";
    if (token.fontStyle & 2) style.fontWeight = "bold";
    if (token.fontStyle & 4) style.textDecoration = "underline";
  }
  return style;
}
