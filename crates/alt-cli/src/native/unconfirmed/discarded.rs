//! What goes off the ends of the append files when a command writes, as
//! the user is told of it: by the command that cut it, and beforehand by
//! the full check.

use std::collections::BTreeMap;
use std::path::Path;

use alt_oplog::Op;

use super::*;

/// What a writer cut, or will cut, off the ends of the append files of a
/// repository.
#[derive(Default)]
pub struct Discards {
    /// bytes off the end of each file, named as it is under `.alt`
    pub files: Vec<(String, u64)>,
    /// ops that were never synced and name objects which are not stored
    pub named: Vec<Op>,
    /// ops that were never synced and follow a record that does not read
    pub after: Vec<Op>,
}

fn target(target: &Option<RefTarget>) -> String {
    match target {
        None => "nothing".to_owned(),
        Some(RefTarget::Oid(oid)) => oid.to_string(),
        Some(RefTarget::Symbolic(name)) => name.clone(),
    }
}

impl Discards {
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    pub fn ops(&self) -> usize {
        self.named.len() + self.after.len()
    }

    /// Counts `bytes` more off the end of `file`.
    pub fn file(&mut self, alt_dir: &Path, file: &Path, bytes: u64) {
        if bytes == 0 {
            return;
        }
        let name = file.strip_prefix(alt_dir).unwrap_or(file);
        let name = name.display().to_string();
        match self.files.iter_mut().find(|(known, _)| *known == name) {
            Some((_, known)) => *known += bytes,
            None => self.files.push((name, bytes)),
        }
    }

    /// What the ops did to the refs, a line for each ref in name order:
    /// how many of them moved it, what it pointed at before the first and
    /// after the last. Then a line counting the ops that moved none.
    pub fn moves(&self) -> Vec<String> {
        let mut refs: BTreeMap<String, (usize, String, String)> = BTreeMap::new();
        let mut others = 0;
        for op in self.named.iter().chain(&self.after) {
            let Ok(Some(tx)) = alt_refs::parse_tx(&op.payload) else {
                others += 1;
                continue;
            };
            for change in tx.changes {
                let new = target(&change.new);
                match refs.get_mut(&change.name) {
                    Some((updates, _, last)) => (*updates, *last) = (*updates + 1, new),
                    None => drop(refs.insert(change.name, (1, target(&change.old), new))),
                }
            }
        }
        let mut lines: Vec<_> = refs
            .into_iter()
            .map(|(name, (updates, old, new))| {
                format!("{name}: {updates} update(s), from {old} to {new}")
            })
            .collect();
        if others > 0 {
            lines.push(format!("{others} operation(s) that move no ref"));
        }
        lines
    }

    /// Why the ops were never kept.
    fn why(&self) -> String {
        match (self.named.len(), self.after.len()) {
            (_, 0) => "they were never synced to disk and name objects that are not stored".into(),
            (0, _) => {
                "they were never synced to disk and follow a record that does not read".into()
            }
            (_, after) => format!(
                "they were never synced to disk; {after} of them follow a record that does not \
                 read, the others name objects that are not stored"
            ),
        }
    }

    /// What a command that cut all this says on stderr.
    pub fn warning(&self) -> String {
        let mut said = String::new();
        if self.ops() > 0 {
            said.push_str(&format!(
                "warning: discarded {} operation(s) at the end of the operation log: {}\n",
                self.ops(),
                self.why()
            ));
            for line in self.moves() {
                said.push_str(&format!("warning:   {line}\n"));
            }
        }
        for (file, bytes) in &self.files {
            said.push_str(&format!(
                "warning: discarded {bytes} byte(s) at the end of {file}: they were never \
                 confirmed as synced to disk\n"
            ));
        }
        said
    }
}

/// A failure of a command that had already cut something off the ends of
/// the append files: what it cut is told with the failure.
#[derive(Debug)]
pub struct AfterDiscards {
    pub said: String,
    pub cause: Box<dyn std::error::Error>,
}

impl std::fmt::Display for AfterDiscards {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}{}", self.said, self.cause)
    }
}

impl std::error::Error for AfterDiscards {}

impl Store {
    /// What this store cut off the ends of the append files since the
    /// last call.
    pub(crate) fn take_discards(&mut self) -> Discards {
        let mut discards = Discards::default();
        for cut in self.odb.take_discarded() {
            discards.file(&self.alt_dir, &cut.file, cut.to - cut.from);
        }
        let mut cuts = self.refs.take_discarded();
        cuts.sort_by_key(|cut| cut.from);
        for cut in cuts {
            discards.file(&self.alt_dir, Path::new("oplog/log"), cut.to - cut.from);
            match cut.unread {
                0 => discards.named.extend(cut.ops),
                _ => discards.after.extend(cut.ops),
            }
        }
        discards
    }

    /// Says on stderr what this store cut since it last said, and which
    /// files are staged with a blob that is no longer stored.
    pub fn warn_discarded(&mut self) -> Res<()> {
        let mut said = self.take_discards().warning();
        if said.is_empty() {
            return Ok(());
        }
        for entry in self.unstored_index_entries(&Default::default())? {
            said.push_str(&format!("warning: {entry}\n"));
        }
        Ok(crate::diagnostics::emit(said.as_bytes())?)
    }

    /// [`Self::settle`] for a caller with no command to tell what was cut:
    /// it goes to stderr.
    pub fn settle_told(&mut self) -> Res<()> {
        self.settle()?;
        self.warn_discarded()
    }

    /// `error` with what this store cut and has not told of yet.
    pub(super) fn after_discards(
        &mut self,
        error: Box<dyn std::error::Error>,
    ) -> Box<dyn std::error::Error> {
        let said = self.take_discards().warning();
        match said.is_empty() {
            true => error,
            false => Box::new(AfterDiscards { said, cause: error }),
        }
    }
}
