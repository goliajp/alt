//! The lines that introduce one file in a patch, laid out and quoted as
//! git does: `diff --git`, mode and rename lines, `index`, and the names
//! the hunks or the binary notice are printed under.

use std::io::Write;

use alt_git_codec::ObjectId;

/// One side of a file: what it holds and its mode.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct Side {
    pub(crate) oid: ObjectId,
    pub(crate) mode: u32,
}

/// How names and ids are printed.
pub(crate) struct Style<'a> {
    /// `core.quotePath`: bytes outside ASCII are escaped too
    pub(crate) quotepath: bool,
    /// absent for a store that cannot say which ids share a prefix
    pub(crate) abbrev: Option<alt_odb::Abbrev<'a>>,
    /// what the hunk headers name
    pub(crate) sections: crate::funcname::Sections,
}

impl Style<'_> {
    pub(crate) fn short(&self, oid: Option<ObjectId>) -> String {
        match (oid, &self.abbrev) {
            (Some(oid), Some(abbrev)) => abbrev.of(&oid),
            (Some(oid), None) => oid.to_string()[..7].to_owned(),
            (None, Some(abbrev)) => "0".repeat(abbrev.min_len()),
            (None, None) => "0".repeat(7),
        }
    }

    /// `prefix` and `path` as one name, in double quotes with C escapes
    /// when the path has bytes that need them.
    pub(crate) fn name(&self, prefix: &str, path: &[u8]) -> Vec<u8> {
        let mut whole = prefix.as_bytes().to_vec();
        whole.extend_from_slice(path);
        let mut out = Vec::new();
        crate::quote::write_path(&mut out, &whole, self.quotepath).expect("writing to memory");
        out
    }
}

pub(crate) struct FileHeader<'a> {
    pub(crate) old_path: &'a [u8],
    pub(crate) new_path: &'a [u8],
    pub(crate) old: Option<Side>,
    pub(crate) new: Option<Side>,
    /// similarity of a renamed file, in percent
    pub(crate) similarity: Option<u32>,
}

impl FileHeader<'_> {
    /// Writes the header down to the `index` line. Returns whether the
    /// content differs: when it does not (a move or a mode change alone)
    /// the header is complete and nothing follows it.
    pub(crate) fn write(&self, out: &mut impl Write, style: &Style<'_>) -> std::io::Result<bool> {
        out.write_all(b"diff --git ")?;
        out.write_all(&style.name("a/", self.old_path))?;
        out.write_all(b" ")?;
        out.write_all(&style.name("b/", self.new_path))?;
        out.write_all(b"\n")?;
        match (&self.old, &self.new) {
            (None, Some(n)) => writeln!(out, "new file mode {:06o}", n.mode)?,
            (Some(o), None) => writeln!(out, "deleted file mode {:06o}", o.mode)?,
            (Some(o), Some(n)) if o.mode != n.mode => {
                writeln!(out, "old mode {:06o}", o.mode)?;
                writeln!(out, "new mode {:06o}", n.mode)?;
            }
            _ => {}
        }
        if let Some(percent) = self.similarity {
            writeln!(out, "similarity index {percent}%")?;
            for (word, path) in [("from", self.old_path), ("to", self.new_path)] {
                write!(out, "rename {word} ")?;
                out.write_all(&style.name("", path))?;
                out.write_all(b"\n")?;
            }
        }
        let (old_oid, new_oid) = (self.old.map(|s| s.oid), self.new.map(|s| s.oid));
        if old_oid == new_oid {
            return Ok(false);
        }
        write!(
            out,
            "index {}..{}",
            style.short(old_oid),
            style.short(new_oid)
        )?;
        match (&self.old, &self.new) {
            (Some(o), Some(n)) if o.mode == n.mode => writeln!(out, " {:06o}", o.mode)?,
            _ => writeln!(out)?,
        }
        Ok(true)
    }

    /// The names of the two sides: `a/<path>` and `b/<path>`, or
    /// `/dev/null` for a side that does not exist.
    fn names(&self, style: &Style<'_>) -> [Vec<u8>; 2] {
        let name = |side: &Option<Side>, prefix, path| match side {
            Some(_) => style.name(prefix, path),
            None => b"/dev/null".to_vec(),
        };
        [
            name(&self.old, "a/", self.old_path),
            name(&self.new, "b/", self.new_path),
        ]
    }

    /// The `---` and `+++` lines that go in front of the hunks. A name
    /// with a space in it is closed off by a tab.
    pub(crate) fn write_hunk_names(
        &self,
        out: &mut impl Write,
        style: &Style<'_>,
    ) -> std::io::Result<()> {
        for (mark, name) in ["--- ", "+++ "].iter().zip(self.names(style)) {
            out.write_all(mark.as_bytes())?;
            out.write_all(&name)?;
            if name.contains(&b' ') {
                out.write_all(b"\t")?;
            }
            out.write_all(b"\n")?;
        }
        Ok(())
    }

    pub(crate) fn write_binary_notice(
        &self,
        out: &mut impl Write,
        style: &Style<'_>,
    ) -> std::io::Result<()> {
        let [old, new] = self.names(style);
        out.write_all(b"Binary files ")?;
        out.write_all(&old)?;
        out.write_all(b" and ")?;
        out.write_all(&new)?;
        out.write_all(b" differ\n")
    }
}
