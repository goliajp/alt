//! `alt diff`: index to working tree, or HEAD to index with `--cached`.

use super::*;

/// What the working-tree side of a diff is read with: the index its paths
/// came from, and the attribute files that say how content is stored.
struct Worktree<'a> {
    index: &'a Index,
    attrs: eol::WorktreeAttributes,
}

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
        let raw = self.index()?;
        let disk = (!cached).then(|| Worktree {
            index: &raw,
            attrs: eol::WorktreeAttributes::new(&self.root),
        });
        let disk = disk.as_ref();
        let (old, new, show_added) = if cached {
            (self.head_entries()?, index_entries(&raw), true)
        } else {
            // Sparse-from-index: no directory walk. One stat per
            // indexed path, only read+hash on a stat mismatch. Untracked
            // files are not in `alt diff`'s output (show_added=false), so
            // skipping the dir traversal is correct — same shape git diff
            // uses. Drops `alt diff` on a clean 50k-file monorepo from
            // seconds to ≤100 ms.
            (index_entries(&raw), self.scan_index_paths(&raw)?, false)
        };

        let changes: Vec<_> = alt_worktree::changes(&old, &new)
            .into_iter()
            // a working-tree file with no index entry is untracked, not a diff
            .filter(|ch| ch.old.is_some() || show_added)
            .collect();

        if json {
            return self.diff_json(&changes, disk, semantic, out);
        }
        let mut buf = Vec::new();
        for ch in &changes {
            // a path that changed kind (file, symlink, submodule) is shown
            // as a removal followed by an addition, as git does
            let kind = |w: &WorkEntry| w.mode & 0o170000;
            match (ch.old, ch.new) {
                (Some(o), Some(n)) if kind(o) != kind(n) => {
                    let removed = alt_worktree::Change {
                        new: None,
                        ..ch.clone()
                    };
                    let added = alt_worktree::Change {
                        old: None,
                        ..ch.clone()
                    };
                    self.emit_file_diff(&mut buf, &removed, disk, semantic)?;
                    self.emit_file_diff(&mut buf, &added, disk, semantic)?;
                }
                _ => self.emit_file_diff(&mut buf, ch, disk, semantic)?,
            }
        }
        out.write_all(&buf)?;
        Ok(())
    }

    /// Builds the `diff --json` document: `{schema_version, files:[…]}`, one
    /// entry per changed file with its oids/modes, a `binary` flag, and the
    /// structured hunks (empty for binary files). When `semantic` is set,
    /// each file gains an `ast_diff` object for languages we have a parser
    /// for; the field is `null` otherwise (including non-`--semantic` runs).
    fn diff_json(
        &self,
        changes: &[alt_worktree::Change],
        disk: Option<&Worktree<'_>>,
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
        ch: &alt_worktree::Change,
        disk: Option<&Worktree<'_>>,
        semantic: bool,
    ) -> Res<()> {
        let path = ch.path;
        let old_bytes = self.side_bytes(ch.old, None)?;
        let new_bytes = self.side_bytes(ch.new, disk)?;

        buf.extend_from_slice(format!("diff --git a/{path} b/{path}\n").as_bytes());
        match (ch.old, ch.new) {
            (None, Some(w)) => {
                buf.extend_from_slice(format!("new file mode {:06o}\n", w.mode).as_bytes());
            }
            (Some(w), None) => {
                buf.extend_from_slice(format!("deleted file mode {:06o}\n", w.mode).as_bytes());
            }
            (Some(o), Some(n)) if o.mode != n.mode => {
                buf.extend_from_slice(format!("old mode {:06o}\n", o.mode).as_bytes());
                buf.extend_from_slice(format!("new mode {:06o}\n", n.mode).as_bytes());
            }
            _ => {}
        }
        let (old_oid, new_oid) = (ch.old.map(|w| w.oid), ch.new.map(|w| w.oid));
        if old_oid == new_oid {
            return Ok(()); // only the mode changed
        }
        let digits = crate::patch::abbrev_len(self.store.odb.len());
        let short = |oid: Option<ObjectId>| match oid {
            Some(oid) => oid.to_string()[..digits].to_owned(),
            None => "0".repeat(digits),
        };
        let mode = match (ch.old, ch.new) {
            (Some(o), Some(n)) if o.mode == n.mode => format!(" {:06o}", o.mode),
            _ => String::new(),
        };
        buf.extend_from_slice(
            format!("index {}..{}{mode}\n", short(old_oid), short(new_oid)).as_bytes(),
        );
        let old_name = match ch.old {
            Some(_) => format!("a/{path}"),
            None => "/dev/null".to_owned(),
        };
        let new_name = match ch.new {
            Some(_) => format!("b/{path}"),
            None => "/dev/null".to_owned(),
        };

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
            buf.extend_from_slice(
                format!("Binary files {old_name} and {new_name} differ\n").as_bytes(),
            );
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
            buf.extend_from_slice(format!("--- {old_name}\n+++ {new_name}\n").as_bytes());
            buf.extend_from_slice(&hunks);
        }
        Ok(())
    }

    /// The content one side of a change is diffed as. A gitlink has no blob
    /// in this store: it diffs as the line git prints for a submodule.
    fn side_bytes(&self, side: Option<&WorkEntry>, disk: Option<&Worktree<'_>>) -> Res<Vec<u8>> {
        match (side, disk) {
            (None, _) => Ok(Vec::new()),
            (Some(w), _) if w.mode == 0o160000 => {
                Ok(format!("Subproject commit {}\n", w.oid).into_bytes())
            }
            (Some(w), Some(disk)) => {
                let content = self.read_for(w)?;
                if w.mode == 0o120000 {
                    return Ok(content);
                }
                // compared as it would be stored, as the old side is
                let stored = self.eol_to_store(&disk.attrs, disk.index, &w.path, &content);
                disk.attrs.check()?;
                Ok(stored.unwrap_or(content))
            }
            (Some(w), None) => self.blob_bytes(w.oid),
        }
    }
}
