//! `alt diff`: index to working tree, or HEAD to index with `--cached`.

use super::*;
use crate::file_header::{FileHeader, Side, Style};

/// One file of a patch: a change in place, or a file moved from
/// `old_path` to `new_path`.
#[derive(Clone, Copy)]
struct Pair<'a> {
    old_path: &'a BString,
    new_path: &'a BString,
    old: Option<&'a WorkEntry>,
    new: Option<&'a WorkEntry>,
    /// similarity of a renamed file, in percent
    similarity: Option<u32>,
}

mod worktree;

use worktree::Worktree;

impl NativeRepo<'_> {
    /// `alt diff` (index → working tree) or `alt diff --cached` (HEAD →
    /// index): a git-style unified diff of the tracked changes. With `json`,
    /// emits the structured per-file/per-hunk schema. With
    /// `semantic`, item-level AST diff replaces the unified diff for
    /// files in a language we have a parser for (`.rs` today); other files
    /// fall back to the same line/binary output as without `--semantic`.
    pub fn diff(&self, cached: bool, json: bool, semantic: bool, out: &mut impl Write) -> Res<()> {
        // old side is always read from the object store; the new side is the
        // index (cached) or the live working tree (default).
        let (old, new, disk): (Vec<WorkEntry>, Vec<WorkEntry>, Option<Worktree>) = if cached {
            let staged = self.staged_changes()?;
            let side = |pick: fn(alt_worktree::Staged) -> Option<WorkEntry>| {
                staged.iter().cloned().filter_map(pick).collect()
            };
            (side(|s| s.old), side(|s| s.new), None)
        } else {
            let (old, new, disk) = self.worktree_sides()?;
            (old, new, Some(disk))
        };
        // a working-tree file with no index entry is untracked, not a diff
        let show_added = cached;
        let disk = disk.as_ref();

        let changes: Vec<_> = alt_worktree::changes(&old, &new)
            .into_iter()
            .filter(|ch| ch.old.is_some() || show_added)
            .collect();

        if json {
            return self.diff_json(&changes, disk, semantic, out);
        }
        let style = Style {
            quotepath: self
                .settings()?
                .get_bool("core", None, "quotepath")
                .transpose()?
                .unwrap_or(true),
            abbrev: Some(self.store.odb.abbrev()),
        };
        let mut buf = Vec::new();
        for pair in self.pairs(&changes, cached)? {
            // a path that changed kind (file, symlink, submodule) is shown
            // as a removal followed by an addition, as git does
            let kind = |w: &WorkEntry| w.mode & 0o170000;
            match (pair.old, pair.new) {
                (Some(o), Some(n)) if kind(o) != kind(n) => {
                    let removed = Pair { new: None, ..pair };
                    let added = Pair { old: None, ..pair };
                    self.emit_file_diff(&mut buf, &removed, disk, semantic, &style)?;
                    self.emit_file_diff(&mut buf, &added, disk, semantic, &style)?;
                }
                _ => self.emit_file_diff(&mut buf, &pair, disk, semantic, &style)?,
            }
        }
        out.write_all(&buf)?;
        Ok(())
    }

    /// The changes as the patch shows them. Between HEAD and the index a
    /// removed file and an added file that are one file renamed become one
    /// entry, reported at the new path, as in git; the working tree never
    /// adds a file to a diff, so there is nothing to pair there.
    fn pairs<'c>(
        &self,
        changes: &'c [alt_worktree::Change<'c>],
        cached: bool,
    ) -> Res<Vec<Pair<'c>>> {
        let plain = |ch: &'c alt_worktree::Change<'c>| Pair {
            old_path: ch.path,
            new_path: ch.path,
            old: ch.old,
            new: ch.new,
            similarity: None,
        };
        if !cached {
            return Ok(changes.iter().map(plain).collect());
        }
        use alt_diff::rename::File;
        let side = |removed: bool| -> Vec<&'c alt_worktree::Change<'c>> {
            changes
                .iter()
                .filter(|ch| match (ch.old, ch.new) {
                    (Some(_), None) => removed,
                    (None, Some(_)) => !removed,
                    _ => false,
                })
                .collect()
        };
        let (removed, added) = (side(true), side(false));
        let file = |ch: &&'c alt_worktree::Change<'c>| {
            let w = ch.old.or(ch.new).expect("one side exists");
            File {
                path: ch.path.as_slice(),
                id: w.oid.as_bytes(),
                mode: w.mode,
            }
        };
        let sources: Vec<File<'_>> = removed.iter().map(file).collect();
        let targets: Vec<File<'_>> = added.iter().map(file).collect();
        let mut load = |source: bool, i: usize| -> Res<Vec<u8>> {
            let ch = if source { removed[i] } else { added[i] };
            self.blob_bytes(ch.old.or(ch.new).expect("one side exists").oid)
        };
        let renames = alt_diff::rename::detect(&sources, &targets, &mut load)?;
        let mut pairs = Vec::with_capacity(changes.len());
        for ch in changes {
            if renames.iter().any(|r| std::ptr::eq(removed[r.src], ch)) {
                continue;
            }
            let from = renames.iter().find(|r| std::ptr::eq(added[r.dst], ch));
            pairs.push(match from {
                Some(r) => Pair {
                    old_path: removed[r.src].path,
                    old: removed[r.src].old,
                    similarity: Some(r.percent()),
                    ..plain(ch)
                },
                None => plain(ch),
            });
        }
        Ok(pairs)
    }

    /// Builds the `diff --json` document: `{schema_version, files:[…]}`, one
    /// entry per changed file with its oids/modes, a `binary` flag, and the
    /// structured hunks (empty for binary files). When `semantic` is set,
    /// each file gains an `ast_diff` object for languages we have a parser
    /// for; the field is `null` otherwise (including non-`--semantic` runs).
    fn diff_json(
        &self,
        changes: &[alt_worktree::Change],
        disk: Option<&Worktree>,
        semantic: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        use crate::json::Json;
        let mut files = Vec::with_capacity(changes.len());
        for ch in changes {
            let old_bytes = self.side_bytes(ch.old, None)?;
            let new_bytes = self.side_bytes(ch.new, disk)?;
            let status = match (ch.old, ch.new) {
                (None, _) => "added",
                (_, None) => "deleted",
                _ => "modified",
            };
            let binary = alt_diff::is_binary(&old_bytes) || alt_diff::is_binary(&new_bytes);
            let hunks = if binary {
                Vec::new()
            } else {
                alt_diff::hunks(&old_bytes, &new_bytes, 3)
                    .iter()
                    .map(hunk_json)
                    .collect()
            };
            // binary files get a structured chunk-diff summary so
            // an agent can answer "how much of this binary is genuinely
            // shared" via the same `--json` surface it uses for text — without
            // the dump-the-bytes blow-up. Text files leave the field null;
            // adding (not replacing) keeps the v1 schema backward-compatible.
            let chunk_diff_field = if binary {
                binary_chunk_diff_json(&old_bytes, &new_bytes)
            } else {
                Json::Null
            };
            // perceptual-style hint for recognised binary kinds
            // (PNG today). Mirrors the human view's "perceptual diff: …"
            // line; `null` for non-image / unknown / text. Additive — v1
            // schema stays backward-compatible.
            let perceptual_diff_field = if binary {
                perceptual_diff_json(&old_bytes, &new_bytes)
            } else {
                Json::Null
            };
            // structured part-aware breakdown so an
            // agent answers "which named chunk changed" without
            // re-parsing the binary. `null` when neither side is a
            // recognised kind (PNG today).
            let part_diff_field = if binary {
                part_aware_diff_json(&old_bytes, &new_bytes)
            } else {
                Json::Null
            };
            let ast_diff_field = if semantic && !binary {
                ast_diff_json(ch.path.as_bytes(), &old_bytes, &new_bytes)
            } else {
                Json::Null
            };
            let oid = |w: Option<&WorkEntry>| match w {
                Some(w) => Json::str(w.oid.to_string()),
                None => Json::Null,
            };
            let mode = |w: Option<&WorkEntry>| match w {
                Some(w) => Json::str(format!("{:06o}", w.mode)),
                None => Json::Null,
            };
            files.push(Json::Object(vec![
                ("path", Json::str(ch.path)),
                ("status", Json::str(status)),
                ("old_oid", oid(ch.old)),
                ("new_oid", oid(ch.new)),
                ("old_mode", mode(ch.old)),
                ("new_mode", mode(ch.new)),
                ("binary", Json::Bool(binary)),
                ("hunks", Json::Array(hunks)),
                ("chunk_diff", chunk_diff_field),
                ("perceptual_diff", perceptual_diff_field),
                ("part_diff", part_diff_field),
                ("ast_diff", ast_diff_field),
            ]));
        }
        let doc = Json::Object(vec![
            ("schema_version", Json::Num(1)),
            ("files", Json::Array(files)),
        ]);
        doc.write(out)?;
        out.write_all(b"\n")?;
        Ok(())
    }

    /// Writes one file's `diff --git` stanza (header + hunks, or a binary
    /// notice) to `buf`. When `semantic` is set and the path has a parser,
    /// the body is replaced by an AST-diff summary instead of the
    /// unified hunks.
    fn emit_file_diff(
        &self,
        buf: &mut Vec<u8>,
        pair: &Pair<'_>,
        disk: Option<&Worktree>,
        semantic: bool,
        style: &Style<'_>,
    ) -> Res<()> {
        let path = pair.new_path;
        let side = |w: &WorkEntry| Side {
            oid: w.oid,
            mode: w.mode,
        };
        let header = FileHeader {
            old_path: pair.old_path,
            new_path: pair.new_path,
            old: pair.old.map(side),
            new: pair.new.map(side),
            similarity: pair.similarity,
        };
        if !header.write(buf, style)? {
            return Ok(()); // moved or re-moded without a change of content
        }
        let old_bytes = self.side_bytes(pair.old, None)?;
        let new_bytes = self.side_bytes(pair.new, disk)?;

        // when --semantic is set and we have a parser for this path,
        // render the AST-level summary in place of the unified hunks. A
        // parse error or unsupported language falls through to the regular
        // diff body — the caller never loses signal, only sometimes the
        // semantic resolution.
        if semantic
            && !alt_diff::is_binary(&old_bytes)
            && !alt_diff::is_binary(&new_bytes)
            && let Some(lang) = lang_for(path.as_bytes())
            && let (Ok(old_s), Ok(new_s)) = (
                std::str::from_utf8(&old_bytes),
                std::str::from_utf8(&new_bytes),
            )
            && let Ok(ast) = alt_treediff::tree_diff(old_s, new_s, lang)
        {
            write_ast_diff_summary(buf, &ast);
            return Ok(());
        }

        // structured-data semantic diff. `--semantic`
        // gate as above; dispatch routes by file extension via
        // `summary_for_path` (.json content-detect + .toml explicit).
        // Parse failures fall through to the line diff — no signal loss.
        if semantic
            && !alt_diff::is_binary(&old_bytes)
            && !alt_diff::is_binary(&new_bytes)
            && let Ok(path_str) = std::str::from_utf8(path.as_bytes())
            && let Some(structured) =
                alt_diff::structured::summary_for_path(path_str, &old_bytes, &new_bytes)
        {
            buf.extend_from_slice(format!("{}\n", structured.render()).as_bytes());
            return Ok(());
        }

        if alt_diff::is_binary(&old_bytes) || alt_diff::is_binary(&new_bytes) {
            // git-compat line first (existing tests + muscle memory key off
            // this exact wording); then a chunk-diff summary so the
            // human view answers "how much is genuinely shared" too.
            header.write_binary_notice(buf, style)?;
            let cd = alt_diff::binary::chunk_diff(
                &old_bytes,
                &new_bytes,
                alt_diff::binary::DEFAULT_PARAMS,
            );
            let pct = (cd.byte_shared_ratio() * 100.0).round() as u32;
            buf.extend_from_slice(
                format!(
                    "chunks: {} shared, {} added, {} removed ({pct}% bytes shared)\n",
                    cd.shared_chunks, cd.added_chunks, cd.removed_chunks,
                )
                .as_bytes(),
            );
            // when both sides decompose under a known
            // format (PNG today), surface the part-aware line so a
            // reviewer reads "IDAT changed but IHDR didn't" rather
            // than "47% bytes shared". Silent when either side isn't
            // a recognised kind — the chunk-diff line above already
            // covers the generic case.
            if let Some(ps) = alt_diff::part_aware::summary(&old_bytes, &new_bytes) {
                buf.extend_from_slice(format!("{}\n", ps.render()).as_bytes());
            }
            // when both sides are a kind we have a fingerprint for
            // (PNG today), surface the perceptual-style hint so a reader
            // sees "is this a small tweak or a wholly different image"
            // without an external image tool. Stays silent when either
            // side isn't a recognised kind — the chunk-diff line already
            // covers the generic-binary case.
            let old_fp = alt_diff::perceptual::fingerprint(&old_bytes);
            let new_fp = alt_diff::perceptual::fingerprint(&new_bytes);
            if let Some(d) = alt_diff::perceptual::distance(old_fp, new_fp) {
                let kind = old_fp.unwrap().kind.as_str();
                let pct_off = (d * 100.0).round() as u32;
                buf.extend_from_slice(
                    format!("perceptual diff: {pct_off}% off (prism={kind})\n").as_bytes(),
                );
            }
            return Ok(());
        }

        // the file header belongs to the hunks: none without them
        let mut hunks = Vec::new();
        if alt_diff::write_unified(&mut hunks, &old_bytes, &new_bytes, 3) {
            header.write_hunk_names(buf, style)?;
            buf.extend_from_slice(&hunks);
        }
        Ok(())
    }

    /// The content one side of a change is diffed as. A gitlink has no blob
    /// in this store: it diffs as the line git prints for a submodule.
    fn side_bytes(&self, side: Option<&WorkEntry>, disk: Option<&Worktree>) -> Res<Vec<u8>> {
        match (side, disk) {
            (None, _) => Ok(Vec::new()),
            (Some(w), _) if w.mode == 0o160000 => {
                Ok(format!("Subproject commit {}\n", w.oid).into_bytes())
            }
            (Some(w), Some(disk)) => self.worktree_bytes(w, disk),
            (Some(w), None) => self.blob_bytes(w.oid),
        }
    }
}
