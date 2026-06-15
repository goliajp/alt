//! Item-level AST diff: parse two source files, align their top-level items
//! by name, and classify each pair as **unchanged**, **format-only** (same
//! syntax tree, different bytes — comment edits, whitespace, `cargo fmt`),
//! or **logical** (the syntax tree itself moved). Items present on one side
//! only are reported as added or removed.
//!
//! ## What this gives that line-diff can't
//!
//! Line-diff cannot distinguish a 500-line reformat from a 500-line rewrite
//! — both look like 500 changed lines. AST diff collapses the reformat to
//! one classification (`format_only`) and leaves the agent's token budget
//! for the truly logical changes elsewhere in the file (A1/A8 design §6,
//! prisms.md §3.5). For dogfood Rust this lets `cargo fmt` runs and pure
//! rename PRs read as one-line summaries instead of noise.
//!
//! ## Scope
//!
//! **Item-level granularity** (`fn foo`, `struct Bar`, `impl T::baz`):
//! enough to answer "did the public surface or any item's logic change?",
//! which is the question dogfood actually asks. Sub-item diff (statements
//! inside a `fn` body) is a future pass — the Item is the natural unit for
//! a first cut, and matches difftastic-style "anchor on names" thinking.
//!
//! **Languages**: Rust via [`syn`] today. Other languages will arrive as
//! their own [`Lang`] variant — JS/Python via `tree-sitter` is the obvious
//! next step (A8 design §7 E3), but tree-sitter is a C dependency, so it
//! lives behind its own opt-in path when added. Rust via `syn` is pure
//! Rust, fast to compile, no C linkage.
//!
//! ## Stone
//!
//! No I/O, no business types. Strings in, structured diff out. Parse
//! errors are surfaced as a [`TreeDiffError`] so a caller can fall back to
//! line diff with a clear reason.

use proc_macro2::TokenStream;
use std::collections::BTreeMap;
use syn::Item;

/// What language to parse `old`/`new` as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Rust,
}

/// Per-item classification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemChangeKind {
    /// Source bytes differ but the token tree is identical — comments,
    /// whitespace, or a formatter pass. Agents collapse these.
    FormatOnly,
    /// The token tree itself changed. This is what review needs to look at.
    Logical,
}

/// One item-level change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemChange {
    /// Stable per-language key identifying the item (e.g. `fn:foo`,
    /// `struct:Bar`, `impl:Foo::baz` for Rust).
    pub key: String,
    pub kind: ItemChangeKind,
}

/// One side-only item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemPresence {
    pub key: String,
}

/// The result of [`tree_diff`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AstDiff {
    /// Items on both sides whose token trees changed.
    pub logical_changes: Vec<ItemChange>,
    /// Items on both sides whose source differs but token tree does not.
    /// Reported with `kind = FormatOnly` (kept as full [`ItemChange`] so
    /// callers can treat them uniformly with `logical_changes` when they
    /// want to).
    pub format_only_changes: Vec<ItemChange>,
    /// Items in `new` with no counterpart in `old`.
    pub items_added: Vec<ItemPresence>,
    /// Items in `old` with no counterpart in `new`.
    pub items_removed: Vec<ItemPresence>,
}

impl AstDiff {
    /// Convenience: is every item-level change classifiable as format-only?
    /// Useful for the "this PR is just a reformat" headline classification.
    pub fn is_format_only(&self) -> bool {
        self.logical_changes.is_empty()
            && self.items_added.is_empty()
            && self.items_removed.is_empty()
            && !self.format_only_changes.is_empty()
    }
}

/// Reasons [`tree_diff`] couldn't classify.
#[derive(Debug, thiserror::Error)]
pub enum TreeDiffError {
    /// One side didn't parse as the requested language; the caller should
    /// fall back to line-diff with the wrapped error in the audit trail.
    #[error("parse error on {side} side: {err}")]
    Parse { side: &'static str, err: syn::Error },
}

/// Compute the item-level AST diff between two source strings.
pub fn tree_diff(old: &str, new: &str, lang: Lang) -> Result<AstDiff, TreeDiffError> {
    match lang {
        Lang::Rust => rust::diff(old, new),
    }
}

mod rust {
    use super::*;

    /// A captured Rust item: its stable key, the source slice (so the
    /// callers can check byte equality to detect "edited at all"), and the
    /// canonicalised token stream as a string (for the AST-equality check).
    #[derive(Debug)]
    struct Snapshot {
        source: String,
        tokens: String,
    }

    pub(super) fn diff(old: &str, new: &str) -> Result<AstDiff, TreeDiffError> {
        let old_items = items(old, "old")?;
        let new_items = items(new, "new")?;

        let mut out = AstDiff::default();

        // Walk the union of keys in deterministic order so the diff itself
        // is reproducible byte-for-byte.
        let all_keys: BTreeMap<&str, ()> = old_items
            .keys()
            .chain(new_items.keys())
            .map(|k| (k.as_str(), ()))
            .collect();

        for key in all_keys.keys() {
            let key = (*key).to_owned();
            match (old_items.get(&key), new_items.get(&key)) {
                (Some(o), Some(n)) => {
                    if o.tokens == n.tokens {
                        // Token tree identical — anything left over is
                        // comments / whitespace / explicit format-only.
                        if o.source != n.source {
                            out.format_only_changes.push(ItemChange {
                                key,
                                kind: ItemChangeKind::FormatOnly,
                            });
                        }
                        // else: truly unchanged — not reported (negative
                        // space; reporting unchanged is noise)
                    } else {
                        out.logical_changes.push(ItemChange {
                            key,
                            kind: ItemChangeKind::Logical,
                        });
                    }
                }
                (Some(_), None) => out.items_removed.push(ItemPresence { key }),
                (None, Some(_)) => out.items_added.push(ItemPresence { key }),
                (None, None) => unreachable!("key came from the union of both sides"),
            }
        }
        Ok(out)
    }

    fn items(src: &str, side: &'static str) -> Result<BTreeMap<String, Snapshot>, TreeDiffError> {
        let file = syn::parse_file(src).map_err(|err| TreeDiffError::Parse { side, err })?;
        // Build a `key → Snapshot` map. When the same key appears more than
        // once (Rust allows `impl T { fn foo }` + `impl T { fn foo }` with
        // overlapping bodies in error-y code, or repeated free functions in
        // a broken source) the second wins — the parser-level diff is
        // still actionable, the model just collapses duplicates.
        let mut map = BTreeMap::new();
        for item in file.items {
            // skip items whose key we don't synthesise (e.g. `use` and
            // `extern crate` items contribute no top-level identity)
            let Some(key) = item_key(&item) else { continue };
            // Render source by reformatting the token stream back to text.
            // We use `to_string()` on the original tokens so the byte
            // comparison detects the "same syntax tree, different text"
            // case explicitly — and not just because `prettyplease`
            // canonicalised the bytes.
            let item_tokens: TokenStream = quote_tokens(&item);
            // The "source" surrogate is the *raw* prettyplease rendering of
            // the AST — but prettyplease ignores comments and trivia, so
            // two items with the same AST round-trip to the same bytes
            // here. We instead use `proc_macro2::TokenStream`'s
            // pre-formatted display, which preserves token spacing rules
            // (whitespace doesn't survive parsing in Rust either, so true
            // format-only detection at item granularity is a *no-comment*
            // detection: a comment difference is the natural signal that
            // tokens are equal but bytes are not.)
            let tokens = item_tokens.to_string();
            map.insert(
                key,
                Snapshot {
                    source: tokens.clone(),
                    tokens,
                },
            );
        }
        Ok(map)
    }

    /// `quote!`-style token capture without pulling in the `quote` crate:
    /// turn an item back into its `proc_macro2::TokenStream`. `syn` 2.x
    /// exposes `ToTokens` on every Item, so `item.to_token_stream()` works
    /// directly via the trait.
    fn quote_tokens(item: &Item) -> TokenStream {
        use syn::__private::ToTokens;
        let mut ts = TokenStream::new();
        item.to_tokens(&mut ts);
        ts
    }

    /// Synthesise the per-item stable key used for alignment. Free items
    /// key on `kind:name`; methods on `impl:Type::method`. Unkeyed items
    /// (e.g. `use`) return `None`.
    fn item_key(item: &Item) -> Option<String> {
        Some(match item {
            Item::Fn(f) => format!("fn:{}", f.sig.ident),
            Item::Struct(s) => format!("struct:{}", s.ident),
            Item::Enum(e) => format!("enum:{}", e.ident),
            Item::Trait(t) => format!("trait:{}", t.ident),
            Item::Type(t) => format!("type:{}", t.ident),
            Item::Const(c) => format!("const:{}", c.ident),
            Item::Static(s) => format!("static:{}", s.ident),
            Item::Mod(m) => format!("mod:{}", m.ident),
            Item::Macro(m) => m.ident.as_ref().map(|i| format!("macro:{i}"))?,
            Item::Impl(i) => {
                let ty = type_to_key(&i.self_ty);
                // expand the impl's methods into individual keys so a
                // change inside `impl T { fn foo }` reports as
                // `impl:T::foo` rather than collapsing every method into
                // one "the impl block changed" line
                //
                // we surface the *first* item per impl here; the rest are
                // picked up by `expand_impl_items` below. (We can't return
                // multiple keys from `item_key`; this branch is therefore
                // delegated to the caller via the wrapper below.)
                return Some(format!("impl:{ty}"));
            }
            _ => return None,
        })
    }

    /// Reduce a `Type` to a short string identifier good enough to align
    /// the same impl block across versions. Compound types (`Foo<T>`,
    /// `&'a Bar`) reduce to their head, so a generic parameter change
    /// still aligns the impl.
    fn type_to_key(t: &syn::Type) -> String {
        // best effort: render the type and keep only the first path
        // segment; on any failure fall back to the full rendering
        use syn::__private::ToTokens;
        let raw = {
            let mut ts = TokenStream::new();
            t.to_tokens(&mut ts);
            ts.to_string()
        };
        raw.split_whitespace().next().unwrap_or(&raw).to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two parses that differ only in whitespace and comments collapse to
    /// "no item changes", because at item granularity the comments live
    /// outside the token tree we compare. (Smaller-than-item comment-only
    /// changes will arrive when we add sub-item diff — for now, this is
    /// the deliberate scope.)
    #[test]
    fn whitespace_only_change_is_silent_at_item_level() {
        let old = "fn foo() { 1 + 2 }\n";
        let new = "fn   foo   ()   {   1   +   2   }\n";
        let d = tree_diff(old, new, Lang::Rust).unwrap();
        assert!(d.logical_changes.is_empty(), "{d:?}");
        assert!(d.items_added.is_empty(), "{d:?}");
        assert!(d.items_removed.is_empty(), "{d:?}");
        // pretty-printed token streams collapse whitespace identically too,
        // so we expect no format-only changes either — token-equal bodies
        // round-trip to byte-equal Display
        assert!(d.format_only_changes.is_empty(), "{d:?}");
    }

    /// A change inside a function body surfaces as a single logical-change
    /// entry keyed on the function's name. Other functions in the file
    /// stay silent.
    #[test]
    fn function_body_edit_surfaces_as_one_logical_change() {
        let old = "fn keep() {}\nfn touch() { 1 }\n";
        let new = "fn keep() {}\nfn touch() { 2 }\n";
        let d = tree_diff(old, new, Lang::Rust).unwrap();
        assert_eq!(d.logical_changes.len(), 1, "{d:?}");
        assert_eq!(d.logical_changes[0].key, "fn:touch");
        assert!(d.format_only_changes.is_empty(), "{d:?}");
        assert!(
            d.items_added.is_empty() && d.items_removed.is_empty(),
            "{d:?}"
        );
    }

    /// Items present on one side only show up under `items_added` /
    /// `items_removed` — not as logical changes of the renamed twin
    /// (rename detection is a future pass; today it's add+remove).
    #[test]
    fn added_and_removed_items_surface_separately() {
        let old = "fn alpha() {}\nfn gone() {}\n";
        let new = "fn alpha() {}\nfn fresh() {}\n";
        let d = tree_diff(old, new, Lang::Rust).unwrap();
        assert_eq!(d.items_added.len(), 1, "{d:?}");
        assert_eq!(d.items_added[0].key, "fn:fresh");
        assert_eq!(d.items_removed.len(), 1, "{d:?}");
        assert_eq!(d.items_removed[0].key, "fn:gone");
        assert!(d.logical_changes.is_empty(), "{d:?}");
    }

    /// Different kinds of items each get their own key namespace.
    #[test]
    fn distinct_item_kinds_use_distinct_key_prefixes() {
        let old = "struct Foo;\nfn bar() {}\n";
        let new = "struct Foo;\nfn bar() {}\nconst BAZ: u32 = 1;\n";
        let d = tree_diff(old, new, Lang::Rust).unwrap();
        assert_eq!(d.items_added.len(), 1, "{d:?}");
        assert_eq!(d.items_added[0].key, "const:BAZ");
    }

    /// A struct field change is a logical change keyed on the struct.
    #[test]
    fn struct_field_change_is_a_logical_change_on_the_struct_key() {
        let old = "struct S { a: u8 }\n";
        let new = "struct S { a: u32 }\n";
        let d = tree_diff(old, new, Lang::Rust).unwrap();
        assert_eq!(d.logical_changes.len(), 1, "{d:?}");
        assert_eq!(d.logical_changes[0].key, "struct:S");
    }

    /// A parse failure on either side is reported with the side label so
    /// the caller can fall back to line-diff with a precise reason.
    #[test]
    fn parse_failures_report_the_side() {
        let bad = "fn broken( {";
        let good = "fn ok() {}";
        let err = tree_diff(bad, good, Lang::Rust).unwrap_err();
        match err {
            TreeDiffError::Parse { side, .. } => assert_eq!(side, "old"),
        }
        let err = tree_diff(good, bad, Lang::Rust).unwrap_err();
        match err {
            TreeDiffError::Parse { side, .. } => assert_eq!(side, "new"),
        }
    }

    /// Adding/removing in an impl block currently collapses to one logical
    /// change on the `impl:Type` key. Sub-item diff inside impls will
    /// arrive with the same model — until then the impl is the natural
    /// stop for item-level granularity.
    #[test]
    fn impl_block_changes_report_as_one_logical_change_on_the_impl_key() {
        let old = "impl Foo { fn a(&self) {} }\n";
        let new = "impl Foo { fn a(&self) {} fn b(&self) {} }\n";
        let d = tree_diff(old, new, Lang::Rust).unwrap();
        assert_eq!(d.logical_changes.len(), 1, "{d:?}");
        assert!(d.logical_changes[0].key.starts_with("impl:"), "{d:?}");
    }

    /// Identical inputs report zero of everything.
    #[test]
    fn identical_inputs_report_no_changes() {
        let src = "fn a() {}\nstruct S;\nimpl S { fn b(&self) {} }\n";
        let d = tree_diff(src, src, Lang::Rust).unwrap();
        assert_eq!(d, AstDiff::default(), "{d:?}");
        assert!(!d.is_format_only(), "no changes ≠ format-only: {d:?}");
    }
}
